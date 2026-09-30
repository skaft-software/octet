//! The OctetHost service surface and its error mapping.

use super::*;

impl OctetHost {
    #[cfg(test)]
    pub(super) fn new(config: Config) -> anyhow::Result<Self> {
        Self::new_with_session_name(config, None)
    }

    pub(super) fn new_with_session_name(
        config: Config,
        session_name: Option<String>,
    ) -> anyhow::Result<Self> {
        require_stable_directory_identity()?;
        let startup_session_name = startup::normalize_startup_session_name(session_name)?;
        let boot = crate::app::bootstrap::bootstrap(config.clone())?;
        let models = graphical_model_catalog(&boot.catalog, &config);
        if models.is_empty() {
            anyhow::bail!("no configured models are available for octet serve");
        }
        let host_id = load_or_create_host_id(&config)?;
        let workspace_name = config
            .workspace
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace");
        let descriptor = HostDescriptor {
            id: host_id,
            name: octet_serve_backend::sanitize_public_text(
                &format!("octet — {workspace_name}"),
                256,
                false,
            ),
        };
        let (themes, selected_theme_id) = graphical_themes(&config)?;
        let state_dir = secure_serve_state_dir(&config.session_dir)?;
        let mut projects = ProjectRegistry::open(state_dir.join("projects"))?;
        let launch_project = match projects.find_by_root(&config.workspace)? {
            Some(project) => project,
            None => projects.import(&config.workspace, Some(workspace_name))?,
        };
        if config.workspace_trusted && launch_project.state == RegistryProjectState::Untrusted {
            projects.grant_trust(&launch_project.id)?;
        }
        reconcile_session_bindings(&config, &mut projects, Some(&launch_project.id))?;
        if projects.default_project().is_none()
            && launch_project.state != RegistryProjectState::Archived
        {
            projects.set_default(&launch_project.id)?;
        }
        let launch_project_id =
            ProjectId::new(launch_project.id.as_str()).map_err(anyhow::Error::msg)?;
        let attachments = match AttachmentStore::open(&state_dir) {
            Ok(store) => Some(store),
            Err(_) => {
                crate::output::stderr_line(
                    "warning: secure attachment storage is unavailable; image uploads are disabled",
                );
                None
            }
        };
        let documents = match DocumentStore::open(&state_dir) {
            Ok(store) => Some(store),
            Err(_) => {
                crate::output::stderr_line(
                    "warning: secure document storage is unavailable; text, Markdown, and PDF uploads are disabled",
                );
                None
            }
        };
        let goals = GoalStore::open(&state_dir.join("goals"))?;
        let resources = match octet_serve_backend::ResourceStore::open(&state_dir) {
            Ok(store) => Some(store),
            Err(_) => {
                crate::output::stderr_line(
                    "warning: secure evidence storage is unavailable; durable sources and outputs are disabled",
                );
                None
            }
        };
        let mut usage = InferenceRequestStore::open(&state_dir)?;
        backfill_usage_store(&config, &projects, &mut usage)?;
        let pull_requests = PullRequestStore::open(&state_dir)
            .context("failed to open stored pull-request evidence")?;
        let host = Self {
            config,
            catalog: boot.catalog,
            models,
            descriptor,
            projects: Arc::new(Mutex::new(projects)),
            launch_project_id,
            themes,
            selected_theme_id,
            attachments,
            documents,
            goals,
            trusted_files: Arc::new(Mutex::new(HashMap::new())),
            search_index: Arc::new(Mutex::new(TranscriptSearchIndex::new())),
            search_index_initialized: Arc::new(AtomicBool::new(false)),
            resources,
            usage: Arc::new(Mutex::new(usage)),
            pull_requests: Arc::new(Mutex::new(pull_requests)),
            serve_state_dir: state_dir,
            session_deletion_lock: Arc::new(tokio::sync::Mutex::new(())),
            startup_session_name: Arc::new(Mutex::new(startup_session_name)),
            #[cfg(test)]
            checkout_hooks: Arc::new(Mutex::new(VecDeque::new())),
            #[cfg(test)]
            open_count: Arc::new(AtomicU64::new(0)),
        };
        host.recover_pending_session_deletions();
        Ok(host)
    }

    pub(super) fn cleanup_session_sidecars(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
    ) -> bool {
        // InferenceRequestStore is intentionally excluded: its
        // conversation-content-free, append-only records are host-level
        // accounting history, not replayable session content. Permanent
        // deletion removes every session-rehydratable
        // sidecar while preserving lifetime usage totals.
        let mut complete = true;
        match &self.attachments {
            Some(store) => complete &= store.delete_session(session_id).is_ok(),
            None => complete = false,
        }
        match &self.documents {
            Some(store) => {
                complete &= store
                    .delete_session(project_id.as_str(), session_id.as_str())
                    .is_ok();
            }
            None => complete = false,
        }
        match &self.resources {
            Some(store) => complete &= store.delete_session(session_id).is_ok(),
            None => complete = false,
        }
        complete &= self.goals.delete_session(session_id).is_ok();
        match self.search_index.lock() {
            Ok(mut search_index) => {
                complete &= search_index.remove_session(session_id.as_str()).is_ok();
            }
            Err(_) => complete = false,
        }
        match self.pull_requests.lock() {
            Ok(mut pull_requests) => {
                complete &= pull_requests.delete_session(session_id).is_ok();
            }
            Err(_) => complete = false,
        }
        complete
    }

    pub(super) fn recover_pending_session_deletions(&self) {
        // Construction performs recovery before the host is published, so the
        // deletion mutex must be immediately available. Keep recovery under
        // the same lock as live deletion in case this method gains another
        // caller later.
        let Ok(_deletion_guard) = self.session_deletion_lock.try_lock() else {
            crate::output::stderr_line(
                "warning: pending permanent session deletions are already being recovered",
            );
            return;
        };
        let Ok(records) = load_pending_session_deletions(&self.serve_state_dir) else {
            crate::output::stderr_line(
                "warning: pending permanent session deletions could not be inspected",
            );
            return;
        };
        for mut record in records {
            let Ok(session_id) = SessionId::new(record.session_id.clone()) else {
                continue;
            };
            let Ok(project_id) = ProjectId::new(record.project_id.clone()) else {
                continue;
            };
            let Ok(registry_id) = RegistryProjectId::parse(record.project_id.clone()) else {
                continue;
            };
            let sessions = {
                let Ok(projects) = self.projects.lock() else {
                    continue;
                };
                let Ok(root) = projects.resolve_root_for_cleanup(&registry_id) else {
                    continue;
                };
                SessionStore::new(&self.config.session_dir, root.as_path())
            };

            if !record.committed {
                match sessions.session_file_exists(session_id.as_str()) {
                    Ok(true) => {
                        let rolled_back = sessions
                            .rollback_permanent_delete(session_id.as_str())
                            .is_ok();
                        let rebound = rolled_back
                            && self.projects.lock().is_ok_and(|mut projects| {
                                projects
                                    .bind_session(session_id.as_str(), &registry_id)
                                    .is_ok()
                            });
                        if rebound
                            && remove_pending_session_deletion(
                                &self.serve_state_dir,
                                session_id.as_str(),
                            )
                            .is_ok()
                        {
                            continue;
                        }
                        crate::output::stderr_line(format!(
                            "warning: pre-commit permanent deletion rollback for session {} remains pending",
                            session_id.as_str()
                        ));
                        continue;
                    }
                    Ok(false) => {
                        record.committed = true;
                        let _ = write_pending_session_deletion(&self.serve_state_dir, &record);
                    }
                    Err(_) => {
                        crate::output::stderr_line(format!(
                            "warning: pre-commit permanent deletion for session {} could not inspect its transcript and remains pending",
                            session_id.as_str()
                        ));
                        continue;
                    }
                }
            }

            let primary_clean = sessions
                .finish_permanent_delete(session_id.as_str())
                .is_ok();
            let unbound = self
                .projects
                .lock()
                .is_ok_and(|mut projects| projects.unbind_session(session_id.as_str()).is_ok());
            let sidecars_clean = self.cleanup_session_sidecars(&project_id, &session_id);
            if primary_clean && unbound && sidecars_clean {
                let _ = remove_pending_session_deletion(&self.serve_state_dir, session_id.as_str());
            } else {
                crate::output::stderr_line(format!(
                    "warning: permanent deletion cleanup for session {} remains pending",
                    session_id.as_str()
                ));
            }
        }
    }

    pub(super) fn cached_pull_request(&self, session_id: &SessionId) -> Option<PullRequestSummary> {
        self.pull_requests
            .lock()
            .ok()
            .and_then(|pull_requests| pull_requests.summary(session_id))
    }

    pub(super) fn stored_session_summary(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionSummary, ServiceError> {
        let context = self.storage_context_for_session(session_id)?;
        let catalog = context
            .sessions
            .catalog_by_id(session_id.as_str())
            .map_err(|_| ServiceError::InvalidSeed)?;
        let meta = catalog.meta.as_ref().ok_or(ServiceError::NotFound)?;
        let selection = advertised_selection_from_catalog_entry(
            &catalog,
            &self.catalog,
            &context.config,
            &self.models,
        )
        .map_or_else(|| self.default_selection(), Ok)?;
        let mut summary = summary_from_meta(meta, Some(context.project_id), selection)?;
        summary.pull_request = self.cached_pull_request(session_id);
        Ok(summary)
    }

    pub(super) fn default_selection(&self) -> Result<ModelSelection, ServiceError> {
        let summary = self
            .config
            .model
            .as_ref()
            .and_then(|model_id| self.models.iter().find(|summary| summary.id == model_id.0))
            .or_else(|| self.models.first())
            .ok_or(ServiceError::InvalidSeed)?;
        Ok(selection_from_summary(summary))
    }

    pub(super) fn project_context(
        &self,
        requested: Option<&ProjectId>,
    ) -> Result<ProjectContext, ServiceError> {
        let projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
        let registry_id = match requested {
            Some(project_id) => registry_project_id(project_id)?,
            None => {
                let default = projects.default_project().map(|project| project.id);
                let mut candidates = default.into_iter().collect::<Vec<_>>();
                let launch_project_id = registry_project_id(&self.launch_project_id)?;
                if !candidates.contains(&launch_project_id) {
                    candidates.push(launch_project_id);
                }
                for project_id in projects.list().into_iter().map(|project| project.id) {
                    if !candidates.contains(&project_id) {
                        candidates.push(project_id);
                    }
                }
                candidates
                    .into_iter()
                    .find(|project_id| projects.resolve_trusted_root(project_id).is_ok())
                    .ok_or(ServiceError::Unauthorized)?
            }
        };
        let root = projects
            .resolve_trusted_root(&registry_id)
            .map_err(project_registry_service_error)?;
        let project_id =
            ProjectId::new(registry_id.as_str()).map_err(|_| ServiceError::Internal)?;
        let mut config = self.config.clone();
        config.workspace = root.as_path().to_owned();
        config.invocation_cwd = root.as_path().to_owned();
        config.workspace_trusted = true;
        let sessions = SessionStore::new(&config.session_dir, root.as_path());
        Ok(ProjectContext {
            project_id,
            config,
            sessions,
        })
    }

    pub(super) fn project_context_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<ProjectContext, ServiceError> {
        let project_id = {
            let mut projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
            if projects.project_for_session(session_id.as_str()).is_none() {
                reconcile_session_bindings(&self.config, &mut projects, None)
                    .map_err(project_registry_service_error)?;
            }
            projects
                .project_for_session(session_id.as_str())
                .ok_or(ServiceError::NotFound)?
        };
        let project_id = ProjectId::new(project_id.as_str()).map_err(|_| ServiceError::Internal)?;
        self.project_context(Some(&project_id))
    }

    pub(super) fn storage_context_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<ProjectContext, ServiceError> {
        let projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
        let registry_id = projects
            .project_for_session(session_id.as_str())
            .ok_or(ServiceError::NotFound)?;
        let root = projects
            .resolve_root(&registry_id)
            .map_err(project_registry_service_error)?;
        let project_id =
            ProjectId::new(registry_id.as_str()).map_err(|_| ServiceError::Internal)?;
        let mut config = self.config.clone();
        config.workspace = root.as_path().to_owned();
        config.invocation_cwd = root.as_path().to_owned();
        config.workspace_trusted = false;
        let sessions = SessionStore::new(&config.session_dir, root.as_path());
        Ok(ProjectContext {
            project_id,
            config,
            sessions,
        })
    }

    pub(super) fn authorize_delegated_session(
        &self,
        provenance: &DelegatedSessionProvenance,
        child: &Path,
        project_id: &ProjectId,
        sessions: &SessionStore,
    ) -> Result<SessionId, ServiceError> {
        let parent_session_id = provenance
            .parent_session_id
            .as_deref()
            .ok_or(ServiceError::NotFound)?;
        let principal = provenance
            .extension_principal
            .as_deref()
            .ok_or(ServiceError::NotFound)?;
        let resource_owner = provenance
            .extension_resource_owner
            .as_deref()
            .ok_or(ServiceError::NotFound)?;
        if !octet_agent::extension_delegated_session_matches_owner(principal, resource_owner, child)
        {
            return Err(ServiceError::NotFound);
        }
        let parent_is_bound = self
            .projects
            .lock()
            .map_err(|_| ServiceError::Internal)?
            .project_for_session(parent_session_id)
            .is_some_and(|bound| bound.as_str() == project_id.as_str());
        if !parent_is_bound {
            return Err(ServiceError::NotFound);
        }
        let parent_path = sessions
            .path_by_id(parent_session_id)
            .map_err(|_| ServiceError::NotFound)?;
        let parent_file = octet_agent::secure_fs::open_private_file_for_read(&parent_path)
            .map_err(|_| ServiceError::NotFound)?;
        let parent = Session::open_read_only_with_file(parent_path, parent_file)
            .map_err(|_| ServiceError::NotFound)?;
        if parent.resource_owner_key() != resource_owner {
            return Err(ServiceError::NotFound);
        }
        SessionId::new(parent_session_id).map_err(|_| ServiceError::NotFound)
    }

    pub(super) fn delegated_session_context(
        &self,
        session_id: &SessionId,
    ) -> Result<DelegatedSessionContext, ServiceError> {
        // The opaque digest is only a lookup key. Authorization is separate:
        // the matched child must carry host-written extension provenance that
        // binds its parent session, path-free extension principal, and exact
        // parent resource owner. Native delegation children and forged or
        // incomplete retained records therefore remain undiscoverable here.
        let digest = session_id
            .as_str()
            .strip_prefix(DELEGATED_SESSION_PREFIX)
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(ServiceError::NotFound)?;
        let expected_reference = format!("{DELEGATED_SESSION_PREFIX}{digest}");
        let project_roots = {
            let projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
            projects
                .list()
                .into_iter()
                .filter_map(|project| {
                    let root = projects.resolve_root(&project.id).ok()?;
                    let project_id = ProjectId::new(project.id.as_str()).ok()?;
                    Some((
                        project_id,
                        root.as_path().to_owned(),
                        project.state == RegistryProjectState::Trusted,
                    ))
                })
                .collect::<Vec<_>>()
        };

        let mut matched = None;
        for (project_id, root, trusted) in project_roots {
            let mut config = self.config.clone();
            config.workspace = root.clone();
            config.invocation_cwd = root.clone();
            config.workspace_trusted = trusted;
            let sessions = SessionStore::new(&config.session_dir, &root);
            let delegation_root = sessions.dir().join(".delegation");
            let Ok(metadata) = delegation_root.symlink_metadata() else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let mut teams = std::fs::read_dir(&delegation_root)
                .map_err(|_| ServiceError::Unavailable)?
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_type()
                        .is_ok_and(|file_type| file_type.is_dir() && !file_type.is_symlink())
                })
                .collect::<Vec<_>>();
            if teams.len() > MAX_DELEGATION_TEAM_DIRECTORIES {
                return Err(ServiceError::PayloadTooLarge);
            }
            teams.sort_by_key(std::fs::DirEntry::file_name);
            for team in teams {
                let team_name = team.file_name();
                let Some(team_name) = team_name.to_str() else {
                    continue;
                };
                if !team_name.starts_with("team-")
                    || team_name.len() > 128
                    || !team_name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                {
                    continue;
                }
                let team_path = team.path();
                let mut children = std::fs::read_dir(&team_path)
                    .map_err(|_| ServiceError::Unavailable)?
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .file_type()
                            .is_ok_and(|file_type| file_type.is_file() && !file_type.is_symlink())
                            && entry.path().extension().and_then(|value| value.to_str())
                                == Some("jsonl")
                            && entry.file_name() != "provenance.jsonl"
                    })
                    .collect::<Vec<_>>();
                if children.len() > MAX_DELEGATED_SESSIONS_PER_TEAM {
                    return Err(ServiceError::PayloadTooLarge);
                }
                children.sort_by_key(std::fs::DirEntry::file_name);
                for child in children {
                    let path = child.path();
                    if octet_agent::delegated_session_reference(&path).as_deref()
                        != Some(expected_reference.as_str())
                    {
                        continue;
                    }
                    if matched.is_some() {
                        return Err(ServiceError::CorruptResource);
                    }
                    let provenance = delegated_session_provenance(&team_path, &path);
                    let parent_session_id = self.authorize_delegated_session(
                        &provenance,
                        &path,
                        &project_id,
                        &sessions,
                    )?;
                    let file = octet_agent::secure_fs::open_private_file_for_read(&path)
                        .map_err(|_| ServiceError::NotFound)?;
                    let file_metadata = file.metadata().map_err(|_| ServiceError::InvalidSeed)?;
                    let fingerprint = DelegatedSessionFingerprint::from_metadata(&file_metadata)?;
                    let modified = fingerprint.modified;
                    let session = Session::open_read_only_with_file(path.clone(), file)
                        .map_err(|_| ServiceError::InvalidSeed)?;
                    let fallback_task_name = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .and_then(|name| name.split_once('-').map(|(_, task)| task))
                        .filter(|name| !name.is_empty())
                        .unwrap_or("worker");
                    let task_name = provenance
                        .display_task_name
                        .as_deref()
                        .unwrap_or(fallback_task_name);
                    let title = octet_serve_backend::sanitize_public_text(
                        &format!("parent > {task_name}"),
                        512,
                        false,
                    );
                    matched = Some(DelegatedSessionContext {
                        project_id: project_id.clone(),
                        parent_session_id,
                        config: config.clone(),
                        session,
                        meta: SessionMeta {
                            id: session_id.as_str().to_owned(),
                            path,
                            title,
                            name: None,
                            tags: vec!["subagent".into(), "read-only".into()],
                            pinned: false,
                            archived: false,
                            trashed_at_ms: None,
                            purge_after_ms: None,
                            forked_from_session_id: None,
                            forked_from_entry_id: None,
                            message_count: 0,
                            modified,
                            workspace: None,
                        },
                        fingerprint,
                    });
                }
            }
        }
        matched.ok_or(ServiceError::NotFound)
    }

    pub(super) fn driver_for_delegated_session(
        &self,
        session_id: &SessionId,
    ) -> Result<OctetSessionDriver, ServiceError> {
        let context = self.delegated_session_context(session_id)?;
        let selection = advertised_selection_from_session(
            &context.session,
            &self.catalog,
            &context.config,
            &self.models,
        )
        .map_or_else(|| self.default_selection(), Ok)?;
        let generation = next_actor_generation();
        let refresh = DelegatedInspectionRefresh {
            path: context.meta.path.clone(),
            workspace: context.config.workspace.clone(),
            project_id: context.project_id.clone(),
            model: selection.clone(),
            generation,
            meta: context.meta.clone(),
        };
        let fingerprint = context.fingerprint;
        let parent_session_id = context.parent_session_id.clone();
        let mut seed = seed_from_session(
            &context.session,
            session_id.clone(),
            SessionSeedOptions {
                workspace: &context.config.workspace,
                project_id: Some(context.project_id),
                model: selection,
                authority: AuthorityProfile::ReadOnly,
                generation,
                meta: Some(context.meta),
                attachment_store: None,
                resource_store: None,
            },
        )?;
        seed.summary.live_state = SessionLiveState::Locked;
        seed.summary.owner = ActorOwnerState::ExternallyLocked;
        seed.snapshot.live_state = SessionLiveState::Locked;
        seed.snapshot.delegated_parent_session_id = Some(parent_session_id);
        Ok(OctetSessionDriver::inspect(seed, refresh, fingerprint))
    }

    pub(super) fn driver_for_new(
        &self,
        request: CreateSessionRequest,
    ) -> Result<OctetSessionDriver, ServiceError> {
        if request.authority != self.authority_ceiling() {
            return Err(ServiceError::Unauthorized);
        }
        let context = self.project_context(request.project_id.as_ref())?;
        let model = match request.model {
            Some(model) => model,
            None => self.default_selection()?,
        };
        let summary = self
            .models
            .iter()
            .find(|summary| summary.provider == model.provider && summary.id == model.model)
            .ok_or(ServiceError::InvalidSeed)?;
        if !summary
            .reasoning
            .iter()
            .any(|choice| choice == &model.reasoning)
        {
            return Err(ServiceError::InvalidSeed);
        }
        let resolved = self
            .catalog
            .resolve(&ModelId(model.model.clone()))
            .map_err(|_| ServiceError::InvalidSeed)?;
        let reasoning =
            config::parse_reasoning(&model.reasoning).map_err(|_| ServiceError::InvalidSeed)?;
        let session_path = context.sessions.new_path(&crate::modes::timestamp());
        let session_id = session_id_from_path(&session_path)?;
        {
            let mut projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
            let registry_id = registry_project_id(&context.project_id)?;
            projects
                .bind_session(session_id.as_str(), &registry_id)
                .map_err(project_registry_service_error)?;
        }
        // A named launch is durable before bootstrap, without constructing an
        // App or contacting a provider. Keep the prepared session's lock until
        // the worker takes ownership; unnamed provisional sessions stay lazy.
        let (session_name, prepared_session) = if request.provisional {
            let mut pending_name = self
                .startup_session_name
                .lock()
                .map_err(|_| ServiceError::Internal)?;
            let prepared = if let Some(name) = pending_name.as_deref() {
                let session = crate::app::bootstrap::open_launch_session(
                    &mut None,
                    SessionSelection::CreateNew(session_path.clone()),
                )
                .map_err(|_| ServiceError::Internal)?;
                context
                    .sessions
                    .rename(session_id.as_str(), name)
                    .map_err(|_| ServiceError::Internal)?;
                Some(session)
            } else {
                None
            };
            (pending_name.take(), prepared)
        } else {
            (None, None)
        };
        let launch_session = if prepared_session.is_some() {
            SessionSelection::OpenExisting(session_path)
        } else {
            SessionSelection::CreateNew(session_path)
        };
        let generation = next_actor_generation();
        let selection = selection_for_model(&resolved, &reasoning, &context.config);
        let project_id = Some(context.project_id.clone());
        let mut seed = empty_seed(
            session_id,
            project_id.clone(),
            selection.clone(),
            request.authority,
            generation,
        );
        if let Some(name) = session_name.as_deref() {
            seed.summary.title = name.to_owned();
        }
        let plan = WorkerPlan {
            config: context.config,
            sessions: context.sessions,
            launch: LaunchSelection {
                model: resolved.spec.id.clone(),
                session: launch_session,
                reasoning,
                reasoning_mode: self.config.reasoning_mode,
            },
            prepared_session: Mutex::new(prepared_session),
            authority: request.authority,
            available_models: self.models.clone(),
            actor_generation: generation,
            session_id: seed.summary.id.clone(),
            project_id,
            attachments: self.attachments.clone(),
            documents: self.documents.clone(),
            projects: Arc::clone(&self.projects),
            trusted_files: Arc::clone(&self.trusted_files),
            search_index: Arc::clone(&self.search_index),
            resources: self.resources.clone(),
            goal_store: Some(self.goals.clone()),
            usage: Arc::clone(&self.usage),
            pull_requests: Arc::clone(&self.pull_requests),
            pull_request_projection: Arc::new(Mutex::new(None)),
            pull_request_discovery_enabled: Arc::new(AtomicBool::new(false)),
            pull_request_refresh_requested: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            checkout_hooks: CheckoutTestHooks::default(),
        };
        Ok(OctetSessionDriver::spawn(seed, plan, 0))
    }

    pub(super) fn driver_for_existing(
        &self,
        session_id: &SessionId,
    ) -> Result<OctetSessionDriver, ServiceError> {
        #[cfg(test)]
        self.open_count.fetch_add(1, Ordering::Relaxed);
        let context = self.project_context_for_session(session_id)?;
        let metadata = context
            .sessions
            .load_metadata(session_id.as_str())
            .map_err(|_| ServiceError::InvalidSeed)?;
        if metadata.trashed_at_ms.is_some() {
            return Err(ServiceError::InvalidBoundary);
        }
        let path = context
            .sessions
            .path_by_id(session_id.as_str())
            .map_err(|_| ServiceError::NotFound)?;
        let file = octet_agent::secure_fs::open_regular_file_for_append(&path)
            .map_err(|_| ServiceError::InvalidSeed)?;
        let session =
            Session::open_with_file(path.clone(), file).map_err(|_| ServiceError::InvalidSeed)?;
        let meta = context
            .sessions
            .meta_for_open_session(session_id.as_str(), &session)
            .map_err(|_| ServiceError::InvalidSeed)?;
        let selection =
            advertised_selection_from_session(&session, &self.catalog, &self.config, &self.models)
                .map_or_else(|| self.default_selection(), Ok)?;
        let generation = next_actor_generation();
        let authority = self.authority_ceiling();
        let mut seed = seed_from_session(
            &session,
            session_id.clone(),
            SessionSeedOptions {
                workspace: &context.config.workspace,
                project_id: Some(context.project_id.clone()),
                model: selection.clone(),
                authority,
                generation,
                meta: meta.clone(),
                attachment_store: self.attachments.as_ref(),
                resource_store: self.resources.as_ref(),
            },
        )?;
        seed.summary.pull_request = self.cached_pull_request(session_id);
        let pull_request_discovery_enabled = context.config.sandbox.process_execution_allowed()
            && session
                .entries()
                .iter()
                .any(|entry| matches!(&entry.value, EntryValue::Message(Message::User(_))));
        let reasoning =
            config::parse_reasoning(&selection.reasoning).map_err(|_| ServiceError::InvalidSeed)?;
        let known_entries = session.entries().len();
        let plan = WorkerPlan {
            config: context.config,
            sessions: context.sessions,
            launch: LaunchSelection {
                model: ModelId(selection.model),
                session: SessionSelection::OpenExisting(path),
                reasoning,
                reasoning_mode: self.config.reasoning_mode,
            },
            prepared_session: Mutex::new(Some(session)),
            authority,
            available_models: self.models.clone(),
            actor_generation: generation,
            session_id: session_id.clone(),
            project_id: Some(context.project_id),
            attachments: self.attachments.clone(),
            documents: self.documents.clone(),
            projects: Arc::clone(&self.projects),
            trusted_files: Arc::clone(&self.trusted_files),
            search_index: Arc::clone(&self.search_index),
            resources: self.resources.clone(),
            goal_store: Some(self.goals.clone()),
            usage: Arc::clone(&self.usage),
            pull_requests: Arc::clone(&self.pull_requests),
            pull_request_projection: Arc::new(Mutex::new(seed.summary.pull_request.clone())),
            pull_request_discovery_enabled: Arc::new(AtomicBool::new(
                pull_request_discovery_enabled,
            )),
            pull_request_refresh_requested: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            checkout_hooks: self
                .checkout_hooks
                .lock()
                .map_err(|_| ServiceError::Internal)?
                .pop_front()
                .unwrap_or_default(),
        };
        Ok(OctetSessionDriver::spawn(seed, plan, known_entries))
    }
}

pub(super) fn document_store_service_error(error: DocumentStoreError) -> ServiceError {
    match error {
        DocumentStoreError::InvalidAssociation
        | DocumentStoreError::InvalidDocumentId
        | DocumentStoreError::Ingest(_) => ServiceError::InvalidBoundary,
        DocumentStoreError::QuotaExceeded => ServiceError::Unavailable,
        DocumentStoreError::PromptLimitExceeded => ServiceError::PayloadTooLarge,
        DocumentStoreError::NotFound => ServiceError::NotFound,
        DocumentStoreError::Corrupt => ServiceError::CorruptResource,
        DocumentStoreError::Storage => ServiceError::Internal,
    }
}

pub(super) fn trusted_file_service_error(error: TrustedFileError) -> ServiceError {
    match error {
        TrustedFileError::TrustRequired => ServiceError::Unauthorized,
        TrustedFileError::RootChanged
        | TrustedFileError::ChangedSinceIndex
        | TrustedFileError::Storage => ServiceError::Unavailable,
        TrustedFileError::NotFound => ServiceError::NotFound,
        TrustedFileError::InvalidEntryId
        | TrustedFileError::InvalidSearch
        | TrustedFileError::NotText => ServiceError::InvalidBoundary,
        TrustedFileError::ContextLimitExceeded => ServiceError::PayloadTooLarge,
    }
}

pub(super) fn repository_context_service_error(error: RepositoryContextError) -> ServiceError {
    match error {
        RepositoryContextError::TrustRequired => ServiceError::Unauthorized,
        RepositoryContextError::RootChanged => ServiceError::Unavailable,
    }
}

pub(super) fn transcript_search_service_error(error: SearchError) -> ServiceError {
    match error {
        SearchError::EmptyQuery
        | SearchError::TooLarge
        | SearchError::InvalidText
        | SearchError::InvalidLimit
        | SearchError::InvalidLimits => ServiceError::InvalidBoundary,
        SearchError::Capacity => ServiceError::Unavailable,
    }
}

pub(super) fn search_document_for_item(
    session_id: &SessionId,
    session_title: &str,
    fallback_timestamp_ms: u64,
    item: &SessionItem,
) -> Option<SearchDocument> {
    if item.lifecycle != ItemLifecycle::Committed {
        return None;
    }
    let (kind, text, timestamp_ms) = match &item.payload {
        ItemPayload::UserMessage {
            text,
            attachments,
            documents,
            project_files,
            ..
        } => {
            let mut visible = Vec::new();
            if !text.trim().is_empty() {
                visible.push(text.clone());
            }
            visible.extend(
                attachments
                    .iter()
                    .map(|attachment| attachment.display_name.clone()),
            );
            visible.extend(
                documents
                    .iter()
                    .map(|document| document.display_name.clone()),
            );
            visible.extend(project_files.iter().map(|file| file.relative_path.clone()));
            (
                SearchDocumentKind::User,
                visible.join("\n"),
                fallback_timestamp_ms,
            )
        }
        ItemPayload::AssistantMessage { text } => (
            SearchDocumentKind::Assistant,
            text.clone(),
            fallback_timestamp_ms,
        ),
        ItemPayload::ToolCall(activity) => {
            let text = [
                Some(activity.title.as_str()),
                activity.summary.as_deref(),
                activity.target.as_deref(),
                activity.output_summary.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
            (
                if activity.status == ToolActivityStatus::Failed {
                    SearchDocumentKind::Error
                } else {
                    SearchDocumentKind::Tool
                },
                text,
                activity.completed_at_ms.unwrap_or(activity.started_at_ms),
            )
        }
        ItemPayload::ToolResult(result) => (
            if result.status == ToolActivityStatus::Failed {
                SearchDocumentKind::Error
            } else {
                SearchDocumentKind::Tool
            },
            [
                Some(result.summary.as_str()),
                result.output_summary.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
            result.completed_at_ms,
        ),
        ItemPayload::RunOutcome {
            outcome: octet_serve_backend::RunOutcome::Failed,
            message,
            ..
        } => (
            SearchDocumentKind::Error,
            message
                .clone()
                .unwrap_or_else(|| "The run failed.".to_owned()),
            fallback_timestamp_ms,
        ),
        ItemPayload::Source(source) if source.kind == SourceKind::Attachment => (
            SearchDocumentKind::Attachment,
            source.title.clone(),
            source.consulted_at_ms,
        ),
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(SearchDocument {
        session_id: session_id.as_str().to_owned(),
        item_id: item.id.as_str().to_owned(),
        kind,
        session_title: bounded_text(session_title, 512),
        text: bounded_text(&text, octet_serve_backend::MAX_SEARCH_DOCUMENT_TEXT_BYTES),
        timestamp_ms,
    })
}

pub(super) fn search_documents_for_seed(seed: &SessionSeed) -> Vec<SearchDocument> {
    seed.snapshot
        .items
        .iter()
        .filter_map(|item| {
            search_document_for_item(
                &seed.snapshot.session_id,
                &seed.summary.title,
                seed.summary.modified_at_ms,
                item,
            )
        })
        .collect()
}

pub(super) fn with_trusted_project_files<T>(
    projects: &Arc<Mutex<ProjectRegistry>>,
    trusted_files: &Arc<Mutex<HashMap<String, TrustedProjectFiles>>>,
    project_id: &ProjectId,
    operation: impl FnOnce(&TrustedProjectFiles, &ProjectRegistry) -> Result<T, TrustedFileError>,
) -> Result<T, ServiceError> {
    let registry_id = registry_project_id(project_id)?;
    let projects = projects.lock().map_err(|_| ServiceError::Internal)?;
    let service = {
        let mut services = trusted_files.lock().map_err(|_| ServiceError::Internal)?;
        match services.get(registry_id.as_str()) {
            Some(service) => service.clone(),
            None => {
                let service = TrustedProjectFiles::open(&projects, &registry_id)
                    .map_err(trusted_file_service_error)?;
                services.insert(registry_id.as_str().to_owned(), service.clone());
                service
            }
        }
    };
    operation(&service, &projects).map_err(trusted_file_service_error)
}

pub(super) fn with_project_file_system<T>(
    projects: &Arc<Mutex<ProjectRegistry>>,
    project_id: &ProjectId,
    operation: impl FnOnce(&ProjectRegistry, &RegistryProjectId) -> Result<T, ProjectFileSystemError>,
) -> Result<T, ProjectFileSystemError> {
    let registry_id = RegistryProjectId::parse(project_id.as_str())
        .map_err(|_| ProjectFileSystemError::InvalidPath)?;
    let projects = projects
        .lock()
        .map_err(|_| ProjectFileSystemError::Storage)?;
    operation(&projects, &registry_id)
}

pub(super) fn public_project_summary(
    registry: &ProjectRegistry,
    project: octet_serve_backend::RegistryProjectSummary,
) -> Result<ProjectSummary, ServiceError> {
    let session_count = registry
        .sessions_for_project(&project.id)
        .len()
        .min(u32::MAX as usize) as u32;
    Ok(ProjectSummary {
        id: ProjectId::new(project.id.as_str()).map_err(|_| ServiceError::Internal)?,
        name: octet_serve_backend::sanitize_public_text(&project.display_name, 256, false),
        trusted: project.state == RegistryProjectState::Trusted,
        archived: project.state == RegistryProjectState::Archived,
        available: project.available,
        is_default: project.is_default,
        session_count,
        live_session_count: 0,
    })
}
