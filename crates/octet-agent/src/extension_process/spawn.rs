//! Spawning an extension process: entrypoint staging and platform launch.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_connection(
    descriptor: &DiscoveredExtension,
    config: &ExtensionRuntimeConfig,
    host_state: ExtensionHostState,
    generation: u64,
    instance_id: &str,
    events: broadcast::Sender<ExtensionEvent>,
    artifact_store: ArtifactStore,
    catalog_updates: mpsc::Sender<CatalogUpdateRequest>,
    delegation_service: Arc<StdRwLock<Option<ExtensionDelegationService>>>,
    approval_store: Arc<ExtensionApprovalStore>,
    defer_post_initialize: bool,
) -> Result<(Arc<ProcessConnection>, ExtensionContributions), ExtensionRuntimeError> {
    let extension_dir =
        descriptor
            .manifest_path
            .parent()
            .ok_or_else(|| ExtensionRuntimeError::Spawn {
                extension: descriptor.manifest.name.clone(),
                message: "manifest has no parent directory".into(),
            })?;
    let resolved_entrypoint =
        resolve_entrypoint_command(extension_dir, &descriptor.manifest.entrypoint).map_err(
            |error| ExtensionRuntimeError::Spawn {
                extension: descriptor.manifest.name.clone(),
                message: error.to_string(),
            },
        )?;
    let scratch_directory = artifact_store
        .begin_generation(generation)
        .map_err(|error| ExtensionRuntimeError::Spawn {
            extension: descriptor.manifest.name.clone(),
            message: format!("cannot create generation scratch directory: {error}"),
        })?;
    let mut artifact_guard = ArtifactGenerationGuard {
        store: artifact_store.clone(),
        generation,
        armed: true,
    };
    #[cfg(windows)]
    let mut command = {
        let launch = windows_script_launch(&resolved_entrypoint.command).map_err(|error| {
            ExtensionRuntimeError::Spawn {
                extension: descriptor.manifest.name.clone(),
                message: error.to_string(),
            }
        })?;
        match launch {
            Some((interpreter, arguments)) => {
                let mut command = Command::new(interpreter);
                command.args(arguments).arg(&resolved_entrypoint.command);
                command
            }
            None => Command::new(&resolved_entrypoint.command),
        }
    };
    #[cfg(not(windows))]
    let mut command = Command::new(&resolved_entrypoint.command);
    command
        .args(&descriptor.manifest.entrypoint.args)
        .current_dir(&config.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear()
        .envs(sanitized_subprocess_environment())
        .envs(&descriptor.manifest.entrypoint.env)
        .envs(brokered_extension_environment(
            &descriptor.manifest.capabilities.environment,
        ))
        .env(
            "OCTET_EXTENSION_API_VERSION",
            &descriptor.manifest.api_version,
        )
        .env("OCTET_EXTENSION_NAME", &descriptor.manifest.name)
        .env("OCTET_EXTENSION_DIR", extension_dir)
        .env("OCTET_EXTENSION_MANIFEST", &descriptor.manifest_path)
        .env("OCTET_WORKSPACE", &config.workspace)
        .env("OCTET_EXTENSION_SCRATCH", &scratch_directory);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    let process_launch = WindowsProcessLaunch::extension(&mut command).map_err(|error| {
        ExtensionRuntimeError::Spawn {
            extension: descriptor.manifest.name.clone(),
            message: format!("failed to prepare Windows process supervision: {error}"),
        }
    })?;

    // Linux can transiently reject exec with ETXTBSY ("Text file busy") when a
    // freshly written entrypoint is launched while another host thread still
    // holds a write descriptor on it (a known race between concurrent fd close
    // and posix_spawn's vfork window in multithreaded processes). A short,
    // bounded retry keeps extension starts reliable without masking other spawn
    // failures.
    let mut child = {
        const MAX_TEXT_FILE_BUSY_RETRIES: usize = 4;
        let mut attempt = 0;
        loop {
            match command.spawn() {
                Ok(child) => break child,
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    attempt += 1;
                    if attempt > MAX_TEXT_FILE_BUSY_RETRIES {
                        return Err(ExtensionRuntimeError::Spawn {
                            extension: descriptor.manifest.name.clone(),
                            message: error.to_string(),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(10 * attempt as u64)).await;
                }
                Err(error) => {
                    return Err(ExtensionRuntimeError::Spawn {
                        extension: descriptor.manifest.name.clone(),
                        message: error.to_string(),
                    });
                }
            }
        }
    };
    #[cfg(unix)]
    let process_group_id = extension_process_group_id(&child);
    #[cfg(unix)]
    let process_group = ProcessGroupGuard::extension(process_group_id);
    #[cfg(windows)]
    let process_group = match process_launch.register(&child) {
        Ok(process_group) => process_group,
        Err(error) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(ExtensionRuntimeError::Spawn {
                extension: descriptor.manifest.name.clone(),
                message: format!("failed to register Windows process supervision: {error}"),
            });
        }
    };
    let termination = process_group.termination_handle();
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ExtensionRuntimeError::Spawn {
            extension: descriptor.manifest.name.clone(),
            message: "child stdin was not piped".into(),
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ExtensionRuntimeError::Spawn {
            extension: descriptor.manifest.name.clone(),
            message: "child stdout was not piped".into(),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ExtensionRuntimeError::Spawn {
            extension: descriptor.manifest.name.clone(),
            message: "child stderr was not piped".into(),
        })?;
    let child = Arc::new(Mutex::new(child));

    let pending = Arc::new(StdMutex::new(HashMap::new()));
    let issued_resource_owners = Arc::new(StdMutex::new(HashSet::new()));
    let session_leaf = Arc::new(session_leaf::SessionLeafMailbox::default());
    let remote_ui = Arc::new(RemoteUiMailbox::new(
        (descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4)
            .then(|| config.remote_ui.clone())
            .flatten(),
    ));
    let pending_changed = Arc::new(Notify::new());
    let child_requests = Arc::new(StdMutex::new(HashMap::new()));
    let child_work_slots = Arc::new(Semaphore::new(MAX_CHILD_WORKERS));
    let closed = Arc::new(AtomicBool::new(false));
    let draining = Arc::new(AtomicBool::new(false));
    let tombstones = Arc::new(StdMutex::new(RequestTombstones::default()));
    let resources = Arc::new(StdMutex::new(ResourceRegistry::with_bulk(
        (descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4)
            .then(|| config.bulk_store.clone())
            .flatten(),
        Arc::clone(&issued_resource_owners),
    )));
    let resource_cleanup_changed = Arc::new(Notify::new());
    let api_v03_contract = Arc::new(StdRwLock::new(None));
    // Binding-specific lifecycle authority is available only to modern peers.
    let session_lifecycle = if matches!(
        descriptor.manifest.api_version.as_str(),
        EXTENSION_API_VERSION_0_3 | EXTENSION_API_VERSION_0_4
    ) {
        config.session_lifecycle.clone()
    } else {
        None
    };
    let (protocol_max_message_bytes, api_v03_max_frame_bytes) =
        if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_3 {
            let max_frame_bytes = config
                .max_message_bytes
                .saturating_sub(1)
                .min(api_v03::MAX_FRAME_BYTES);
            (max_frame_bytes.saturating_add(1), max_frame_bytes)
        } else {
            (config.max_message_bytes, 0)
        };
    let frame_limit = Arc::new(ProtocolFrameLimit::new(
        protocol_max_message_bytes,
        descriptor.manifest.api_version == EXTENSION_API_VERSION_0_3,
    ));
    let protocol = Arc::new(StdRwLock::new(ExtensionNegotiatedProtocol {
        version: descriptor.manifest.api_version.clone(),
        features: BTreeSet::new(),
        max_concurrent_requests: config.max_pending_requests,
        lifecycle_events: BTreeSet::new(),
    }));
    let tool_catalog = Arc::new(StdRwLock::new(Vec::new()));
    let health = Arc::new(StdRwLock::new(ConnectionHealth {
        state: ExtensionHealthState::Initializing,
        last_error: None,
    }));
    let initialization_complete = Arc::new(AtomicBool::new(false));
    let initialization_changed = Arc::new(Notify::new());
    let provider_owner = ExtensionProviderOwner {
        extension_instance_id: instance_id.to_owned(),
        generation,
    };
    let provider_streams = Arc::new(StdMutex::new(HashMap::new()));
    let (writer, writer_frames) = mpsc::channel(config.writer_queue_capacity);
    tokio::spawn(run_protocol_writer(
        stdin,
        writer_frames,
        Arc::clone(&closed),
        Arc::clone(&draining),
        Arc::clone(&pending),
        Arc::clone(&pending_changed),
        Arc::clone(&remote_ui),
        Arc::clone(&health),
        events.clone(),
        Arc::clone(&child),
        termination.clone(),
        Arc::clone(&frame_limit),
    ));
    let (presentation_updates, presentation_update_rx) = watch::channel(None);
    tokio::spawn(dispatch_presentation_updates(
        presentation_update_rx,
        events.clone(),
        generation,
    ));
    tokio::spawn(read_protocol_stdout(
        stdout,
        Arc::clone(&pending),
        Arc::clone(&resources),
        Arc::clone(&resource_cleanup_changed),
        Arc::clone(&issued_resource_owners),
        Arc::clone(&session_leaf),
        Arc::clone(&remote_ui),
        Arc::clone(&pending_changed),
        Arc::clone(&closed),
        Arc::clone(&draining),
        events.clone(),
        presentation_updates,
        generation,
        instance_id.to_owned(),
        Arc::clone(&frame_limit),
        descriptor.manifest.contributes.clone(),
        writer.clone(),
        Arc::clone(&child_requests),
        child_work_slots,
        Arc::clone(&tombstones),
        Arc::clone(&protocol),
        Arc::clone(&api_v03_contract),
        config.provider_registry.clone(),
        provider_owner.clone(),
        Arc::clone(&provider_streams),
        Arc::clone(&tool_catalog),
        catalog_updates,
        delegation_service,
        session_lifecycle.clone(),
        config.event_bus.clone(),
        approval_store,
        config.secret_broker.clone(),
        ExtensionIdentity {
            name: descriptor.manifest.name.clone(),
            version: descriptor.manifest.version.clone(),
            manifest_path: descriptor.manifest_path.clone(),
            source: descriptor.source,
        },
        Arc::new(
            descriptor
                .manifest
                .capabilities
                .secrets
                .iter()
                .cloned()
                .collect(),
        ),
        Arc::clone(&health),
        Arc::clone(&initialization_complete),
        Arc::clone(&initialization_changed),
        artifact_store.clone(),
        Some(Arc::clone(&child)),
        Some(termination),
    ));
    tokio::spawn(read_extension_stderr(
        stderr,
        events.clone(),
        config.max_message_bytes,
    ));

    let connection = Arc::new(ProcessConnection {
        writer,
        child,
        pending,
        resources,
        resource_cleanup_changed: Arc::clone(&resource_cleanup_changed),
        issued_resource_owners,
        session_leaf,
        remote_ui,
        pending_changed,
        child_requests,
        next_id: AtomicU64::new(1),
        closed,
        draining,
        active_admissions: AtomicU64::new(0),
        slots: StdRwLock::new(Arc::new(Semaphore::new(config.max_pending_requests))),
        frame_limit,
        shutdown_timeout: config.shutdown_timeout,
        cancellation_grace: config.cancellation_grace,
        tombstone_ttl: config.tombstone_ttl,
        tombstones,
        protocol,
        api_v03_contract,
        initialization_complete: Arc::clone(&initialization_complete),
        initialization_changed: Arc::clone(&initialization_changed),
        catalog_guard: StdRwLock::new(()),
        tool_catalog,
        catalog_revision: AtomicU64::new(0),
        health,
        events: events.clone(),
        generation,
        artifact_store,
        artifact_leases: AtomicU64::new(0),
        artifact_leases_changed: Notify::new(),
        artifacts_settled: AtomicBool::new(false),
        provider_registry: config.provider_registry.clone(),
        provider_owner,
        provider_streams,
        next_provider_stream_id: AtomicU64::new(1),
        provider_stream_buffer: config.provider_stream_buffer,
        provider_stream_idle_timeout: config.provider_stream_idle_timeout,
        provider_stream_deadline: config.provider_stream_deadline,
        provider_owner_removed: AtomicBool::new(false),
        event_bus: config.event_bus.clone(),
        message_deltas: StdMutex::new(MessageDeltaCoalescer::default()),
        process_group,
    });
    tokio::spawn(run_resource_cleanup(
        Arc::downgrade(&connection),
        resource_cleanup_changed,
    ));
    artifact_guard.disarm();
    let offered_host_services = OfferedHostServices {
        provider_proxy: config.provider_registry.is_some()
            && descriptor.manifest.contributes.providers
            && descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4,
        remote_ui: config.remote_ui.is_some(),
        provider_pipeline: config.provider_pipeline
            && descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4,
        resource_paths: config.resource_paths
            && descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4,
        bulk_objects: config.bulk_store.is_some()
            && descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4,
        agent_sessions: config.agent_sessions,
        tool_composition: config.tool_composition
            && descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4,
        session_lifecycle: session_lifecycle.is_some(),
        session_compaction: session_lifecycle
            .as_ref()
            .is_some_and(|service| service.supports_compaction()),
        approvals: config.approvals,
        secrets: config.secret_broker.is_some()
            && !descriptor.manifest.capabilities.secrets.is_empty(),
    };
    let mut required_features = API_0_2_REQUIRED_FEATURES
        .iter()
        .map(|feature| (*feature).to_owned())
        .collect::<Vec<_>>();
    let requires_delegation_telemetry =
        offered_host_services.agent_sessions && descriptor.manifest.name == "octet-subagents";
    if requires_delegation_telemetry {
        required_features.push(EXTENSION_FEATURE_DELEGATION_TELEMETRY.to_owned());
    }
    let mut optional_features = API_0_2_OPTIONAL_FEATURES
        .iter()
        .map(|feature| (*feature).to_owned())
        .collect::<Vec<_>>();
    if offered_host_services.provider_proxy {
        optional_features.push("provider_proxy_v1".to_owned());
    }
    if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4
        && descriptor
            .manifest
            .contributes
            .hooks
            .contains(&ExtensionHook::CompactionStrategy)
    {
        optional_features.push(EXTENSION_FEATURE_COMPACTION_STRATEGY.to_owned());
    }
    if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4
        && descriptor
            .manifest
            .contributes
            .hooks
            .contains(&ExtensionHook::CacheWarmingDecision)
    {
        optional_features.push(EXTENSION_FEATURE_CACHE_WARMING_DECISION.to_owned());
    }
    if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4 {
        optional_features.push(EXTENSION_FEATURE_RESOURCE_REFS_V1.to_owned());
        optional_features.push(EXTENSION_FEATURE_OPERATION_DESCRIPTORS_V1.to_owned());
        optional_features.push(EXTENSION_FEATURE_TOOL_PROMPT_METADATA.to_owned());
        optional_features.push(EXTENSION_FEATURE_AUTOCOMPLETE_EDIT_V1.to_owned());
    }
    if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4
        && descriptor.manifest.capabilities.system_prompt
        && descriptor
            .manifest
            .contributes
            .hooks
            .contains(&ExtensionHook::BeforePrompt)
    {
        optional_features.push(EXTENSION_FEATURE_BEFORE_PROMPT_STATE_V1.to_owned());
    }
    if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4
        && descriptor
            .manifest
            .contributes
            .hooks
            .contains(&ExtensionHook::BeforePrompt)
    {
        optional_features.push("input_transform_v1".to_owned());
    }
    if offered_host_services.provider_pipeline
        && descriptor
            .manifest
            .contributes
            .hooks
            .iter()
            .any(|hook| hook.is_provider_pipeline())
    {
        optional_features.push(EXTENSION_FEATURE_PIPELINE_HOOKS_V1.to_owned());
    }
    if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4
        && offered_host_services.session_lifecycle
    {
        optional_features.push(EXTENSION_FEATURE_SESSION_CONTROL_V1.to_owned());
        if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4 {
            optional_features.push("process_exec_v1".to_owned());
            optional_features.push("mcp_registration_v1".to_owned());
        }
        if offered_host_services.session_compaction {
            optional_features.push(EXTENSION_FEATURE_SESSION_COMPACTION_V1.to_owned());
        }
    }
    if offered_host_services.resource_paths
        && descriptor
            .manifest
            .contributes
            .hooks
            .contains(&ExtensionHook::ResourcesDiscover)
    {
        optional_features.push(EXTENSION_FEATURE_RESOURCE_PATHS.to_owned());
    }
    if offered_host_services.bulk_objects {
        optional_features.push(EXTENSION_FEATURE_BULK_OBJECTS_V1.to_owned());
    }
    if offered_host_services.tool_composition {
        optional_features.push(EXTENSION_FEATURE_TOOL_COMPOSITION.to_owned());
    }
    if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4
        && offered_host_services.remote_ui
    {
        optional_features.push(EXTENSION_FEATURE_REMOTE_UI.to_owned());
    }
    if offered_host_services.agent_sessions {
        optional_features.push(EXTENSION_FEATURE_AGENT_SESSIONS.to_owned());
        optional_features.push(EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1.to_owned());
        if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_4 {
            optional_features.push(EXTENSION_FEATURE_AGENT_SESSION_EVENTS_V1.to_owned());
            optional_features.push(EXTENSION_FEATURE_AGENT_SESSION_LIFETIME_V1.to_owned());
        }
    }
    if offered_host_services.approvals {
        optional_features.push(EXTENSION_FEATURE_APPROVALS.to_owned());
    }
    if offered_host_services.secrets {
        optional_features.push(EXTENSION_FEATURE_SECRETS.to_owned());
    }
    // A disclosure surface is offered only to an extension that declares the
    // capability for it: the same posture as `secrets`, but for host-owned
    // prompt text. An undeclared extension never sees the feature, and echoing
    // it in the initialize response fails negotiation below.
    if descriptor.manifest.capabilities.system_prompt {
        optional_features.push(EXTENSION_FEATURE_SYSTEM_PROMPT_READ.to_owned());
    }

    let api_v03_offer = (descriptor.manifest.api_version == EXTENSION_API_VERSION_0_3)
        .then(|| {
            api_v03_host_offer_for_services(
                api_v03_max_frame_bytes,
                config.max_pending_requests,
                offered_host_services.session_lifecycle,
                config.event_bus.is_some(),
            )
        })
        .transpose()
        .map_err(api_v03_protocol_error)?;
    let extension_identity = ExtensionIdentity {
        name: descriptor.manifest.name.clone(),
        version: descriptor.manifest.version.clone(),
        manifest_path: descriptor.manifest_path.clone(),
        source: descriptor.source,
    };
    let initialize_value = if let Some(contract) = &api_v03_offer {
        let initialize = api_v03::InitializeRequest {
            api_version: EXTENSION_API_VERSION_0_3.to_owned(),
            octet_version: env!("CARGO_PKG_VERSION").to_owned(),
            extension: serde_json::to_value(&extension_identity)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?,
            workspace: config.workspace.to_string_lossy().into_owned(),
            capabilities: serde_json::to_value(&descriptor.manifest.capabilities)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?,
            contributes: serde_json::to_value(&descriptor.manifest.contributes)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?,
            flag_values: config
                .flag_values
                .iter()
                .map(|(name, value)| api_v03::InitializeFlagValue {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
            host: serde_json::to_value(&host_state)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?,
            contract: contract.clone(),
        };
        api_v03::validate_initialize_request(&initialize).map_err(api_v03_protocol_error)?;
        serde_json::to_value(initialize)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?
    } else {
        let flag_values = projected_initialize_flag_values(
            &descriptor.manifest.api_version,
            &config.flag_values,
        )?;
        let mut initialize_value = serde_json::to_value(InitializeRequest {
            api_version: descriptor.manifest.api_version.clone(),
            octet_version: env!("CARGO_PKG_VERSION").to_owned(),
            extension: extension_identity,
            workspace: config.workspace.clone(),
            capabilities: descriptor.manifest.capabilities.clone(),
            contributes: descriptor.manifest.contributes.clone(),
            host: host_state,
            flag_values,
            protocol: (uses_api_0_2_capabilities(&descriptor.manifest.api_version)).then(|| {
                ExtensionProtocolRequest {
                    version: descriptor.manifest.api_version.clone(),
                    required_features,
                    optional_features,
                    bulk_objects_v1: if offered_host_services.bulk_objects {
                        config.bulk_store.as_ref().map(|storage| {
                            let store = storage.lock();
                            serde_json::json!({"profile":"local-file.v1", "transfer_directory": store.transfer_directory(), "limits":store.limits()})
                        })
                    } else { None },
                    limits: ExtensionProtocolLimits {
                        max_concurrent_requests: config.max_pending_requests,
                        resource_refs_v1: (descriptor.manifest.api_version
                            == EXTENSION_API_VERSION_0_4)
                            .then(ResourceProtocolLimits::default),
                    },
                }
            }),
        })
        .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_1 {
            // API 0.1's frozen initialize payload predates shortcuts. Keep the
            // absent field distinct from an API 0.2 empty shortcut catalog.
            initialize_value
                .get_mut("contributes")
                .and_then(serde_json::Value::as_object_mut)
                .expect("InitializeRequest contributions serialize as an object")
                .remove("shortcuts");
            // API 0.1's frozen initialize payload also predates projected flag
            // values. `skip_serializing_if` already drops the absent field; the
            // explicit removal keeps the guarantee even if that attribute goes
            // away, matching the shortcut removal above.
            initialize_value
                .as_object_mut()
                .expect("InitializeRequest serializes as an object")
                .remove("flag_values");
        }
        initialize_value
    };
    let response = connection
        .request(
            methods::INITIALIZE,
            initialize_value,
            config.request_timeout,
        )
        .await;
    let negotiated = match response.and_then(|response| {
        if let Some(offer) = &api_v03_offer {
            let response = api_v03::parse_initialize_response(response).map_err(|error| {
                ExtensionRuntimeError::Protocol(format!(
                    "invalid API 0.3 initialize response: {error}"
                ))
            })?;
            negotiate_api_v03_contributions(
                &descriptor.manifest,
                offer,
                response,
                config.provider_registry.is_some(),
            )
            .map(|(contributions, protocol, contract)| (contributions, protocol, Some(contract)))
        } else {
            let response: InitializeResponse =
                serde_json::from_value(response).map_err(|error| {
                    ExtensionRuntimeError::Protocol(format!("invalid initialize response: {error}"))
                })?;
            negotiate_contributions_with_host_services(
                &descriptor.manifest,
                response,
                config.max_pending_requests,
                offered_host_services,
            )
            .map(|(contributions, protocol)| (contributions, protocol, None))
        }
    }) {
        Ok(negotiated) => negotiated,
        Err(error) => {
            initialization_complete.store(true, Ordering::Release);
            initialization_changed.notify_waiters();
            connection.terminate().await;
            return Err(error);
        }
    };
    let (contributions, protocol, api_v03_contract) = negotiated;
    if let Some(contract) = &api_v03_contract {
        // The reader waits after the initialize response, so this single atomic
        // install becomes visible to both outbound serialization and the next
        // buffered stdout byte before protocol traffic resumes.
        connection
            .frame_limit
            .install_selected_api_v03(contract.limits.max_frame_bytes);
    }
    *write_std_lock(&connection.slots) = Arc::new(Semaphore::new(protocol.max_concurrent_requests));
    *write_std_lock(&connection.protocol) = protocol;
    *write_std_lock(&connection.api_v03_contract) = api_v03_contract;
    *write_std_lock(&connection.tool_catalog) = contributions.tools.clone();
    if !defer_post_initialize {
        connection.activate_post_initialize();
    }
    update_health(&connection.health, ExtensionHealthState::Ready, None);
    Ok((connection, contributions))
}

pub(super) const MAX_STAGED_ENTRYPOINT_BYTES: u64 = 64 * 1024 * 1024;

pub(super) struct ResolvedEntrypoint {
    pub(super) command: PathBuf,
    pub(super) _staging: Option<tempfile::TempDir>,
}

pub(super) fn stage_entrypoint(path: &Path) -> std::io::Result<Option<ResolvedEntrypoint>> {
    let mut source = match crate::secure_fs::open_regular_file_for_read(path) {
        Ok(source) => source,
        Err(crate::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(None);
        }
        Err(error) => return Err(std::io::Error::other(error)),
    };
    let metadata = source.metadata()?;
    if metadata.len() > MAX_STAGED_ENTRYPOINT_BYTES {
        return Err(std::io::Error::other(
            "extension entrypoint exceeds the 64 MiB staging limit",
        ));
    }
    let temporary = tempfile::Builder::new()
        .prefix("octet-extension-entrypoint-")
        .tempdir()?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("extension entrypoint has no file name"))?;
    let staged = temporary.path().join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o700);
    }
    let mut destination = options.open(&staged)?;
    let copied = std::io::copy(
        &mut Read::by_ref(&mut source).take(MAX_STAGED_ENTRYPOINT_BYTES + 1),
        &mut destination,
    )?;
    if copied > MAX_STAGED_ENTRYPOINT_BYTES {
        return Err(std::io::Error::other(
            "extension entrypoint grew beyond the 64 MiB staging limit",
        ));
    }
    destination.flush()?;
    destination.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let executable = metadata.permissions().mode() & 0o111 != 0;
        destination.set_permissions(std::fs::Permissions::from_mode(if executable {
            0o700
        } else {
            0o600
        }))?;
        destination.sync_all()?;
    }
    Ok(Some(ResolvedEntrypoint {
        command: staged,
        _staging: Some(temporary),
    }))
}

/// Longest first line inspected for a `#!` interpreter line.
#[cfg(windows)]
pub(super) const MAX_SHEBANG_BYTES: usize = 256;

/// Whether an entrypoint is a Python script: a `.py` file or a `#!` line
/// naming Python (as the bundled `#!/usr/bin/env python3` entrypoints do).
#[cfg(any(windows, test))]
pub(super) fn is_python_script(path: &Path, first_line: &[u8]) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
        || (first_line.starts_with(b"#!") && first_line.windows(6).any(|part| part == b"python"))
}

/// Interpreter and leading arguments that execute a staged entrypoint.
///
/// Unix executes the staged file and the kernel honours its `#!` line.
/// Windows has no shebang support: `CreateProcess` rejects a script as "not
/// a valid Win32 application", so no Python extension could start. A Python
/// script therefore runs through a Python 3 interpreter found the way
/// `/usr/bin/env python3` finds one, on `PATH`: the `py -3` launcher, then
/// `python3.exe`/`python.exe`, with Microsoft Store app-execution aliases
/// (which open the Store when Python is absent) last. Other entrypoints are
/// executed directly.
#[cfg(windows)]
pub(super) fn windows_script_launch(
    entrypoint: &Path,
) -> std::io::Result<Option<(PathBuf, Vec<std::ffi::OsString>)>> {
    let mut first_line = Vec::with_capacity(MAX_SHEBANG_BYTES);
    std::fs::File::open(entrypoint)?
        .take(MAX_SHEBANG_BYTES as u64)
        .read_to_end(&mut first_line)?;
    if let Some(end) = first_line.iter().position(|byte| *byte == b'\n') {
        first_line.truncate(end);
    }
    if !is_python_script(entrypoint, &first_line) {
        return Ok(None);
    }
    // Only absolute entries: an empty or relative PATH entry would resolve
    // against the current directory, which is the (possibly untrusted)
    // workspace. Rust's own Command search excludes it for the same reason.
    let directories: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|directory| directory.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    let is_store_alias = |path: &Path| {
        path.to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\")
            .contains("\\microsoft\\windowsapps\\")
    };
    let find = |name: &str, allow_store_alias: bool| {
        directories
            .iter()
            .map(|directory| directory.join(name))
            .find(|candidate| {
                candidate.is_file() && (allow_store_alias || !is_store_alias(candidate))
            })
    };
    if let Some(launcher) = find("py.exe", false) {
        return Ok(Some((launcher, vec!["-3".into()])));
    }
    for allow_store_alias in [false, true] {
        for name in ["python3.exe", "python.exe"] {
            if let Some(interpreter) = find(name, allow_store_alias) {
                return Ok(Some((interpreter, Vec::new())));
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "this Python extension needs Python 3 on Windows: install it from python.org \
         (which adds the py launcher) or put python.exe on PATH",
    ))
}

pub(super) fn resolve_entrypoint_command(
    directory: &Path,
    entrypoint: &ExtensionEntrypoint,
) -> std::io::Result<ResolvedEntrypoint> {
    let configured = PathBuf::from(&entrypoint.command);
    if configured.is_absolute() {
        return stage_entrypoint(&configured)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "extension entrypoint is missing",
            )
        });
    }

    #[cfg(not(windows))]
    let names = vec![configured.clone()];
    // CreateProcess appends .exe for executable-name entrypoints. Resolve that
    // spelling ourselves so inspection/staging sees the actual executable,
    // rather than falling through to an uninspected Command PATH search.
    #[cfg(windows)]
    let names = if configured.extension().is_none() {
        vec![configured.clone(), configured.with_extension("exe")]
    } else {
        vec![configured.clone()]
    };
    for name in &names {
        if let Some(staged) = stage_entrypoint(&directory.join(name))? {
            return Ok(staged);
        }
    }

    if configured.components().count() == 1 {
        if let Some(path) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&path) {
                #[cfg(windows)]
                if !directory.is_absolute() {
                    continue;
                }
                for name in &names {
                    let candidate = directory.join(name);
                    let resolved = match candidate.canonicalize() {
                        Ok(resolved) => resolved,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error),
                    };
                    if let Some(staged) = stage_entrypoint(&resolved)? {
                        return Ok(staged);
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    return Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "extension entrypoint is missing",
    ));
    #[cfg(not(windows))]
    Ok(ResolvedEntrypoint {
        command: configured,
        _staging: None,
    })
}

#[cfg(all(test, windows))]
mod windows_resolution_tests {
    use super::*;

    #[test]
    fn executable_names_resolve_exe_spelling_before_staging() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["python", "node", "py"] {
            let executable = directory.path().join(format!("{name}.exe"));
            std::fs::write(&executable, b"MZ-inspected-executable-fixture").unwrap();
            let entrypoint = ExtensionEntrypoint {
                command: name.into(),
                ..Default::default()
            };
            let resolved = resolve_entrypoint_command(directory.path(), &entrypoint).unwrap();
            assert_ne!(resolved.command, executable);
            assert_eq!(resolved.command.extension().unwrap(), "exe");
            std::fs::write(&executable, b"changed after inspection").unwrap();
            assert_eq!(
                std::fs::read(&resolved.command).unwrap(),
                b"MZ-inspected-executable-fixture"
            );
        }
        let missing = ExtensionEntrypoint {
            command: "missing-entrypoint-v082-sentinel".into(),
            ..Default::default()
        };
        assert!(resolve_entrypoint_command(directory.path(), &missing).is_err());
    }
}

pub(super) fn api_v03_host_offer_for_services(
    max_frame_bytes: usize,
    max_pending_requests: usize,
    session_lifecycle: bool,
    event_bus: bool,
) -> Result<api_v03::ContractOffer, api_v03::ContractError> {
    let mut offer = api_v03::host_offer(max_frame_bytes, max_pending_requests)?;
    // Generated optional services are not automatically product services.
    // Theme selection has no host-owned catalog/namespace binding or handler;
    // offering it would route a negotiated call into the legacy fallback.
    offer
        .optional_capabilities
        .retain(|capability| capability != "theme_selection");
    offer.optional_methods.retain(|method| {
        api_v03::method_spec(method)
            .is_none_or(|specification| specification.capability != "theme_selection")
    });
    if !session_lifecycle {
        offer
            .optional_capabilities
            .retain(|capability| capability != "session_lifecycle");
        offer.optional_methods.retain(|method| {
            api_v03::method_spec(method)
                .is_none_or(|specification| specification.capability != "session_lifecycle")
        });
    }
    if !event_bus {
        offer
            .optional_capabilities
            .retain(|capability| capability != "event_bus");
        offer.optional_methods.retain(|method| {
            api_v03::method_spec(method).is_none_or(|spec| spec.capability != "event_bus")
        });
    }
    api_v03::validate_offer(&offer)?;
    Ok(offer)
}

pub(super) fn api_v03_protocol_error(error: api_v03::ContractError) -> ExtensionRuntimeError {
    ExtensionRuntimeError::Protocol(format!(
        "API 0.3 contract error {}: {}",
        error.code, error.message
    ))
}

pub(super) fn validate_session_hook_id(value: &str) -> Result<(), ExtensionRuntimeError> {
    if value.is_empty()
        || value.len() > api_v03::MAX_SESSION_HOOK_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ExtensionRuntimeError::Protocol(
            "session hook identifiers must be bounded opaque ASCII tokens".into(),
        ));
    }
    Ok(())
}

pub(super) fn session_hook_wire_binding(
    binding: &ActiveSessionHookBinding,
    extension_instance_id: &str,
) -> Result<api_v03::SessionBinding, ExtensionRuntimeError> {
    validate_session_hook_id(&binding.session_id)?;
    validate_session_hook_id(extension_instance_id)?;
    let max_generation = (api_v03::MAX_PORTABLE_JSON_INTEGER as u64).min(usize::MAX as u64);
    if binding.endpoint.generation > max_generation {
        return Err(ExtensionRuntimeError::Protocol(
            "session hook process generation exceeds the API 0.3 portable integer bound".into(),
        ));
    }
    Ok(api_v03::SessionBinding {
        session_id: binding.session_id.clone(),
        extension_instance_id: extension_instance_id.to_owned(),
        process_generation: binding.endpoint.generation as usize,
    })
}

pub(super) fn session_hook_duration_millis(duration: Duration) -> usize {
    duration
        .as_millis()
        .min((api_v03::MAX_PORTABLE_JSON_INTEGER as u64).min(usize::MAX as u64) as u128)
        as usize
}

pub(super) fn session_hook_outcome(outcome: ExtensionLifecycleOutcome) -> &'static str {
    match outcome {
        ExtensionLifecycleOutcome::Completed => "completed",
        ExtensionLifecycleOutcome::Failed => "failed",
        ExtensionLifecycleOutcome::Cancelled => "cancelled",
        ExtensionLifecycleOutcome::Interrupted => "interrupted",
        ExtensionLifecycleOutcome::FrontendDisconnected => "frontend_disconnected",
        ExtensionLifecycleOutcome::Shutdown => "shutdown",
        ExtensionLifecycleOutcome::LimitReached => "limit_reached",
    }
}

pub(super) fn session_hook_shutdown_reason(outcome: ExtensionLifecycleOutcome) -> &'static str {
    if outcome == ExtensionLifecycleOutcome::Cancelled {
        "cancelled"
    } else {
        "shutdown"
    }
}

pub(super) fn session_hook_error_kind(error: &ExtensionRuntimeError) -> &'static str {
    match error {
        ExtensionRuntimeError::Timeout { .. } => "timeout",
        ExtensionRuntimeError::Cancelled { .. } => "cancelled",
        ExtensionRuntimeError::Closed(_) => "closed",
        ExtensionRuntimeError::Remote { .. } => "remote_error",
        ExtensionRuntimeError::Protocol(_) => "protocol_error",
        ExtensionRuntimeError::MessageTooLarge { .. } => "message_too_large",
        _ => "internal_error",
    }
}
