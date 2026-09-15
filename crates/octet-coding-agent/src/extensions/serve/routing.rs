//! Serve request routing and host-side protocol admission.
//!
//! This module owns the `HostService` boundary: capability advertisement,
//! project/session operations, attachment/document access, and conversion of
//! durable state into transport-facing results. Startup and run execution stay
//! behind the host and supervisor boundaries.

use super::*;

#[async_trait]
impl HostService for OctetHost {
    type Driver = OctetSessionDriver;

    fn descriptor(&self) -> HostDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> HostCapabilities {
        let attachment_policy = self.attachments.as_ref().map(AttachmentStore::policy);
        HostCapabilities {
            concurrent_sessions: true,
            opaque_resources: self.resources.is_some(),
            attachments: attachment_policy.is_some(),
            attachment_policy,
            documents: self.documents.is_some(),
            trusted_project_files: cfg!(unix),
            project_file_browser: cfg!(unix),
            project_file_write: cfg!(unix) && self.config.tool_available("write"),
            transcript_search: true,
            previews: false,
            connected_devices: false,
            session_metadata: true,
            session_branches: true,
            conversation_branching: true,
            session_trash: true,
            session_export: true,
            lan_clients: false,
            terminal: self.config.sandbox.process_execution_allowed(),
            child_agents: false,
        }
    }

    fn attachment_policy(&self) -> Option<AttachmentPolicy> {
        self.attachments.as_ref().map(AttachmentStore::policy)
    }

    async fn ingest_attachment(
        &self,
        display_name: &str,
        media_type: &str,
        bytes: bytes::Bytes,
    ) -> Result<AttachmentRef, AttachmentError> {
        self.attachments
            .as_ref()
            .ok_or(AttachmentError::Unavailable)?
            .ingest(display_name, media_type, bytes)
    }

    async fn attachment_content(&self, handle: &str) -> Result<StoredAttachment, AttachmentError> {
        self.attachments
            .as_ref()
            .ok_or(AttachmentError::Unavailable)?
            .content(handle)
    }

    fn document_ingest_supported(&self) -> bool {
        self.documents.is_some()
    }

    async fn ingest_document(
        &self,
        session_id: &SessionId,
        display_name: &str,
        media_type: &str,
        bytes: bytes::Bytes,
    ) -> Result<DocumentReference, ServiceError> {
        let context = self.project_context_for_session(session_id)?;
        let store = self.documents.clone().ok_or(ServiceError::Unavailable)?;
        store
            .ingest_async(
                context.project_id.as_str().to_owned(),
                session_id.as_str().to_owned(),
                display_name.to_owned(),
                media_type.to_owned(),
                bytes,
            )
            .await
            .map_err(document_store_service_error)
    }

    async fn list_documents(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<DocumentReference>, ServiceError> {
        let context = self.project_context_for_session(session_id)?;
        self.documents
            .as_ref()
            .ok_or(ServiceError::Unavailable)?
            .list_for_session(context.project_id.as_str(), session_id.as_str())
            .map_err(document_store_service_error)
    }

    fn trusted_project_files_supported(&self) -> bool {
        cfg!(unix)
    }

    async fn trusted_file_index(
        &self,
        project_id: &ProjectId,
    ) -> Result<TrustedFileIndexSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let trusted_files = Arc::clone(&self.trusted_files);
        let project_id = project_id.clone();
        tokio::task::spawn_blocking(move || {
            with_trusted_project_files(
                &projects,
                &trusted_files,
                &project_id,
                |service, registry| service.summary(registry),
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn list_trusted_files(
        &self,
        project_id: &ProjectId,
        limit: usize,
    ) -> Result<Vec<TrustedFileEntry>, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let trusted_files = Arc::clone(&self.trusted_files);
        let project_id = project_id.clone();
        tokio::task::spawn_blocking(move || {
            with_trusted_project_files(
                &projects,
                &trusted_files,
                &project_id,
                |service, registry| service.list(registry, limit),
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn search_trusted_files(
        &self,
        project_id: &ProjectId,
        query: &str,
        limit: usize,
    ) -> Result<TrustedFileSearchResult, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let trusted_files = Arc::clone(&self.trusted_files);
        let project_id = project_id.clone();
        let query = query.to_owned();
        tokio::task::spawn_blocking(move || {
            with_trusted_project_files(
                &projects,
                &trusted_files,
                &project_id,
                |service, registry| service.search(registry, &query, limit),
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn read_trusted_file(
        &self,
        project_id: &ProjectId,
        entry_id: &FileEntryId,
    ) -> Result<TrustedFileRead, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let trusted_files = Arc::clone(&self.trusted_files);
        let project_id = project_id.clone();
        let entry_id = entry_id.clone();
        tokio::task::spawn_blocking(move || {
            with_trusted_project_files(
                &projects,
                &trusted_files,
                &project_id,
                |service, registry| service.read(registry, &entry_id),
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    fn project_file_browser_supported(&self) -> bool {
        cfg!(unix)
    }

    fn project_file_write_supported(&self) -> bool {
        cfg!(unix) && self.config.tool_available("write")
    }

    async fn project_file_tree(
        &self,
        project_id: &ProjectId,
        path: &str,
    ) -> Result<ProjectFileTree, ProjectFileSystemError> {
        if !self.project_file_browser_supported() {
            return Err(ProjectFileSystemError::Unavailable);
        }
        let projects = Arc::clone(&self.projects);
        let project_id = project_id.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            with_project_file_system(&projects, &project_id, |registry, registry_id| {
                ProjectFileSystem::tree(registry, registry_id, &path)
            })
        })
        .await
        .map_err(|_| ProjectFileSystemError::Storage)?
    }

    async fn read_project_file(
        &self,
        project_id: &ProjectId,
        path: &str,
        start_line: Option<u32>,
        end_line: Option<u32>,
    ) -> Result<ProjectFileRead, ProjectFileSystemError> {
        if !self.project_file_browser_supported() {
            return Err(ProjectFileSystemError::Unavailable);
        }
        let projects = Arc::clone(&self.projects);
        let project_id = project_id.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            with_project_file_system(&projects, &project_id, |registry, registry_id| {
                ProjectFileSystem::read(registry, registry_id, &path, start_line, end_line)
            })
        })
        .await
        .map_err(|_| ProjectFileSystemError::Storage)?
    }

    async fn search_project_files(
        &self,
        project_id: &ProjectId,
        query: &str,
    ) -> Result<ProjectFileSearchResult, ProjectFileSystemError> {
        if !self.project_file_browser_supported() {
            return Err(ProjectFileSystemError::Unavailable);
        }
        let projects = Arc::clone(&self.projects);
        let project_id = project_id.clone();
        let query = query.to_owned();
        tokio::task::spawn_blocking(move || {
            with_project_file_system(&projects, &project_id, |registry, registry_id| {
                ProjectFileSystem::search(registry, registry_id, &query)
            })
        })
        .await
        .map_err(|_| ProjectFileSystemError::Storage)?
    }

    async fn write_project_file(
        &self,
        project_id: &ProjectId,
        path: &str,
        content: &str,
        expected_sha256: &str,
        force: bool,
    ) -> Result<ProjectFileWrite, ProjectFileSystemError> {
        if !self.project_file_write_supported() {
            return Err(ProjectFileSystemError::WriteUnavailable);
        }
        let projects = Arc::clone(&self.projects);
        let project_id = project_id.clone();
        let path = path.to_owned();
        let content = content.to_owned();
        let expected_sha256 = expected_sha256.to_owned();
        tokio::task::spawn_blocking(move || {
            with_project_file_system(&projects, &project_id, |registry, registry_id| {
                ProjectFileSystem::write(
                    registry,
                    registry_id,
                    &path,
                    &content,
                    &expected_sha256,
                    force,
                )
            })
        })
        .await
        .map_err(|_| ProjectFileSystemError::Storage)?
    }

    fn transcript_search_supported(&self) -> bool {
        true
    }

    async fn search_transcripts(
        &self,
        request: &TranscriptSearchRequest,
    ) -> Result<TranscriptSearchResult, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let search_index = Arc::clone(&self.search_index);
        let search_index_initialized = Arc::clone(&self.search_index_initialized);
        let base_config = self.config.clone();
        let catalog = self.catalog.clone();
        let fallback = self.default_selection()?;
        let attachments = self.attachments.clone();
        let resources = self.resources.clone();
        let request = request.clone();
        let authority = self.authority_ceiling();
        tokio::task::spawn_blocking(move || {
            // Hold the index lock while the one-time historical rebuild runs so
            // a concurrent run completion or deletion cannot be overwritten by
            // the snapshot being installed. Subsequent searches never acquire
            // the project registry lock or reopen session transcripts.
            let mut search_index = search_index.lock().map_err(|_| ServiceError::Internal)?;
            if !search_index_initialized.load(Ordering::Acquire) {
                let projects = projects.lock().map_err(|_| ServiceError::Internal)?;
                let mut rebuilt = TranscriptSearchIndex::new();
                for project in projects.list() {
                    let Ok(root) = projects.resolve_trusted_root(&project.id) else {
                        continue;
                    };
                    let sessions = SessionStore::new(&base_config.session_dir, root.as_path());
                    let bound = projects.sessions_for_project(&project.id);
                    let public_project_id =
                        ProjectId::new(project.id.as_str()).map_err(|_| ServiceError::Internal)?;
                    let mut project_config = base_config.clone();
                    project_config.workspace = root.as_path().to_owned();
                    project_config.invocation_cwd = root.as_path().to_owned();
                    project_config.workspace_trusted = true;
                    for session_id_text in
                        sessions.session_ids_newest_first(bound.iter().map(String::as_str))
                    {
                        let Ok(session_id) = SessionId::new(session_id_text.clone()) else {
                            continue;
                        };
                        let Ok(path) = sessions.path_by_id(&session_id_text) else {
                            continue;
                        };
                        let Ok(session) = Session::open_read_only(&path) else {
                            continue;
                        };
                        let Ok(Some(meta)) =
                            sessions.meta_for_open_session(&session_id_text, &session)
                        else {
                            continue;
                        };
                        let selection = selection_from_session(&session, &catalog, &project_config)
                            .unwrap_or_else(|_| fallback.clone());
                        let seed = seed_from_session(
                            &session,
                            session_id.clone(),
                            SessionSeedOptions {
                                workspace: &project_config.workspace,
                                project_id: Some(public_project_id.clone()),
                                model: selection,
                                authority,
                                generation: 1,
                                meta: Some(meta),
                                attachment_store: attachments.as_ref(),
                                resource_store: resources.as_ref(),
                            },
                        )?;
                        rebuilt
                            .replace_session(session_id.as_str(), search_documents_for_seed(&seed))
                            .map_err(transcript_search_service_error)?;
                    }
                }
                *search_index = rebuilt;
                search_index_initialized.store(true, Ordering::Release);
            }
            search_index
                .search_request(&request)
                .map_err(transcript_search_service_error)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    fn repository_context_supported(&self) -> bool {
        cfg!(unix)
    }

    async fn repository_context(
        &self,
        project_id: &ProjectId,
    ) -> Result<RepositoryContextSnapshot, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        tokio::task::spawn_blocking(move || {
            let projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            refresh_repository_context(&projects, &project_id)
                .map_err(repository_context_service_error)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn resource_content(
        &self,
        session_id: &SessionId,
        handle: &str,
    ) -> Result<StoredResource, ServiceError> {
        self.resources
            .as_ref()
            .ok_or(ServiceError::Unavailable)?
            .content(session_id, handle)
            .map_err(resource_store_service_error)
    }

    async fn session_export(&self, session_id: &SessionId) -> Result<bytes::Bytes, ServiceError> {
        if session_id.as_str().starts_with(DELEGATED_SESSION_PREFIX) {
            let context = self.delegated_session_context(session_id)?;
            let path = context.meta.path;
            let fingerprint = context.fingerprint;
            let workspace = context.config.workspace;
            let session_id = session_id.clone();
            let serve_state_dir = self.serve_state_dir.clone();
            return tokio::task::spawn_blocking(move || {
                export_delegated_session_bytes(
                    &path,
                    fingerprint,
                    &session_id,
                    &workspace,
                    &serve_state_dir,
                    MAX_GRAPHICAL_SESSION_EXPORT_BYTES,
                )
            })
            .await
            .map_err(|_| ServiceError::Internal)?;
        }
        let sessions = self.project_context_for_session(session_id)?.sessions;
        let session_id = session_id.clone();
        let serve_state_dir = self.serve_state_dir.clone();
        tokio::task::spawn_blocking(move || {
            export_session_bytes(
                &sessions,
                &session_id,
                &serve_state_dir,
                MAX_GRAPHICAL_SESSION_EXPORT_BYTES,
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn usage_stats(&self, period: UsagePeriod) -> Result<UsageStats, ServiceError> {
        Ok(self
            .usage
            .lock()
            .map_err(|_| ServiceError::Internal)?
            .stats(period))
    }

    async fn usage_lifetime(&self) -> Result<LifetimeUsage, ServiceError> {
        Ok(self
            .usage
            .lock()
            .map_err(|_| ServiceError::Internal)?
            .lifetime())
    }

    async fn usage_activity(&self) -> Result<UsageActivity, ServiceError> {
        Ok(self
            .usage
            .lock()
            .map_err(|_| ServiceError::Internal)?
            .activity())
    }

    fn authority_ceiling(&self) -> AuthorityProfile {
        authority_ceiling_from_sandbox(&self.config.sandbox)
    }

    fn authority_profiles(&self) -> Vec<AuthorityProfile> {
        authority_profiles_from_sandbox(&self.config.sandbox)
    }

    fn model_catalog(&self) -> Vec<ModelSummary> {
        self.models.clone()
    }

    fn theme_catalog(&self) -> Vec<ThemeOption> {
        self.themes.clone()
    }

    fn selected_theme_id(&self) -> ThemeId {
        self.selected_theme_id.clone()
    }

    async fn list_projects(&self) -> Result<Vec<ProjectSummary>, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            reconcile_session_bindings(&config, &mut projects, None)
                .map_err(project_registry_service_error)?;
            projects
                .list()
                .into_iter()
                .map(|project| public_project_summary(&projects, project))
                .collect()
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    fn project_lifecycle_mutations_supported(&self) -> bool {
        cfg!(unix)
    }

    fn project_import_supported(&self) -> bool {
        false
    }

    async fn import_project(
        &self,
        _candidate_id: &str,
        display_name: Option<&str>,
    ) -> Result<ProjectSummary, ServiceError> {
        let _ = display_name;
        // The browser transport has no native folder picker. Real roots are
        // imported from the trusted launch/CLI workspace; this command remains
        // unavailable until a host UI can mint one-use opaque candidates.
        Err(ServiceError::Unavailable)
    }

    async fn rename_project(
        &self,
        project_id: &ProjectId,
        display_name: &str,
    ) -> Result<ProjectSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        let display_name = display_name.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            let project = projects
                .update_display_name(&project_id, &display_name)
                .map_err(project_registry_service_error)?;
            public_project_summary(&projects, project)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn set_default_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<ProjectSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            let project = projects
                .set_default(&project_id)
                .map_err(project_registry_service_error)?;
            public_project_summary(&projects, project)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn clear_default_project(&self) -> Result<(), ServiceError> {
        let projects = Arc::clone(&self.projects);
        tokio::task::spawn_blocking(move || {
            projects
                .lock()
                .map_err(|_| ServiceError::Internal)?
                .clear_default()
                .map_err(project_registry_service_error)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn set_project_trust(
        &self,
        project_id: &ProjectId,
        trusted: bool,
    ) -> Result<ProjectSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        let launch_project_id = self.launch_project_id.clone();
        let launch_workspace = self.config.workspace.clone();
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            let project = if trusted {
                // A replaced checkout at the exact launch path can be restored
                // only by an explicit trust action. Never rebind another
                // project or accept a browser-supplied filesystem path.
                if project_id.as_str() == launch_project_id.as_str() {
                    projects
                        .rebind_root(&project_id, &launch_workspace)
                        .map_err(project_registry_service_error)?;
                }
                projects.grant_trust(&project_id)
            } else {
                projects.revoke_trust(&project_id)
            }
            .map_err(project_registry_service_error)?;
            if trusted {
                reconcile_session_bindings(&config, &mut projects, None)
                    .map_err(project_registry_service_error)?;
            }
            public_project_summary(&projects, project)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn archive_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<ProjectSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            let project = projects
                .archive(&project_id)
                .map_err(project_registry_service_error)?;
            public_project_summary(&projects, project)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    fn session_trash_supported(&self) -> bool {
        true
    }

    async fn set_session_lifecycle(
        &self,
        session_id: &SessionId,
        lifecycle: SessionCatalogState,
        changed_at_ms: u64,
    ) -> Result<SessionSummary, ServiceError> {
        let context = self.storage_context_for_session(session_id)?;
        let storage_lifecycle = match lifecycle {
            SessionCatalogState::Active => SessionStorageLifecycle::Active,
            SessionCatalogState::Archived => SessionStorageLifecycle::Archived,
            SessionCatalogState::Trash => SessionStorageLifecycle::Trash,
        };
        context
            .sessions
            .set_lifecycle(session_id.as_str(), storage_lifecycle, changed_at_ms)
            .map_err(|_| ServiceError::Internal)?;
        self.stored_session_summary(session_id)
    }

    async fn delete_session_permanently(
        &self,
        session_id: &SessionId,
        confirmation: &PermanentDeleteConfirmation,
    ) -> Result<(), ServiceError> {
        if &confirmation.session_id != session_id
            || confirmation.phrase != format!("permanently delete {}", session_id.as_str())
        {
            return Err(ServiceError::InvalidBoundary);
        }
        // Distinct idempotency keys may execute concurrently. Serialize the
        // destructive state machine so one request cannot overwrite or remove
        // another request's recovery journal.
        let _deletion_guard = self.session_deletion_lock.lock().await;
        let context = self.storage_context_for_session(session_id)?;
        if self.attachments.is_none() || self.documents.is_none() || self.resources.is_none() {
            return Err(ServiceError::Unavailable);
        }
        // The supervisor has quiesced the session owner. Preserve every last
        // ledger row and uncertainty marker before deleting their only source.
        let inspection = context
            .sessions
            .inspect_by_id(session_id.as_str())
            .map_err(|_| ServiceError::Unavailable)?;
        {
            let mut usage = self.usage.lock().map_err(|_| ServiceError::Internal)?;
            usage
                .ensure_available()
                .map_err(|_| ServiceError::Unavailable)?;
            if !inspection.usage_uncertainty_records.is_empty() {
                usage
                    .record_uncertainty(session_id.as_str())
                    .map_err(|_| ServiceError::Unavailable)?;
            }
            usage
                .record_all(
                    project_catalog_usage(session_id.as_str(), &inspection.usage_records)
                        .map_err(|_| ServiceError::Unavailable)?,
                )
                .map_err(|_| ServiceError::Unavailable)?;
        }
        let mut deletion = PendingSessionDeletion::new(
            session_id,
            &context.project_id,
            confirmation.trashed_at_ms,
        );
        write_pending_session_deletion(&self.serve_state_dir, &deletion)
            .map_err(|_| ServiceError::Internal)?;

        let delete_result = context
            .sessions
            .delete_permanently(session_id.as_str(), confirmation.trashed_at_ms);
        if delete_result.is_err() {
            match context.sessions.session_file_exists(session_id.as_str()) {
                Ok(true) => {
                    context
                        .sessions
                        .rollback_permanent_delete(session_id.as_str())
                        .map_err(|_| ServiceError::Internal)?;
                    remove_pending_session_deletion(&self.serve_state_dir, session_id.as_str())
                        .map_err(|_| ServiceError::Internal)?;
                    return Err(ServiceError::InvalidBoundary);
                }
                Ok(false) => {}
                Err(_) => {
                    // Preserve the durable intent. Startup recovery must not
                    // infer commitment from a transcript it could not inspect.
                    return Err(ServiceError::Internal);
                }
            }
        }

        // The JSONL disappearance is the irreversible commit boundary. Every
        // later step is idempotent and journaled so interruption cannot turn a
        // completed user-visible delete into permanently leaked sidecars.
        deletion.committed = true;
        let marker_committed =
            write_pending_session_deletion(&self.serve_state_dir, &deletion).is_ok();
        let primary_clean = context
            .sessions
            .finish_permanent_delete(session_id.as_str())
            .is_ok();
        let unbound = self
            .projects
            .lock()
            .is_ok_and(|mut projects| projects.unbind_session(session_id.as_str()).is_ok());
        let sidecars_clean = self.cleanup_session_sidecars(&context.project_id, session_id);
        if marker_committed && primary_clean && unbound && sidecars_clean {
            let _ = remove_pending_session_deletion(&self.serve_state_dir, session_id.as_str());
        } else {
            crate::output::stderr_line(format!(
                "warning: permanent deletion cleanup for session {} will retry on startup",
                session_id.as_str()
            ));
        }
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, ServiceError> {
        let fallback = self.default_selection()?;
        let projects = Arc::clone(&self.projects);
        let pull_requests = Arc::clone(&self.pull_requests);
        let catalog = self.catalog.clone();
        let models = self.models.clone();
        let base_config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            reconcile_session_bindings(&base_config, &mut projects, None)
                .map_err(project_registry_service_error)?;
            let mut summaries = Vec::new();
            for project in projects.list() {
                if summaries.len() >= 2_000 || project.state == RegistryProjectState::Archived {
                    continue;
                }
                let Ok(root) = projects.resolve_root(&project.id) else {
                    continue;
                };
                let sessions = SessionStore::new(&base_config.session_dir, root.as_path());
                let bound = projects.sessions_for_project(&project.id);
                let public_project_id =
                    ProjectId::new(project.id.as_str()).map_err(|_| ServiceError::Internal)?;
                let mut project_config = base_config.clone();
                project_config.workspace = root.as_path().to_owned();
                project_config.invocation_cwd = root.as_path().to_owned();
                project_config.workspace_trusted = project.state == RegistryProjectState::Trusted;
                let session_ids = sessions
                    .session_ids_newest_first(bound.iter().map(String::as_str))
                    .into_iter()
                    .take(2_000)
                    .collect::<Vec<_>>();
                let catalog_entries = sessions
                    .catalog_by_ids(session_ids.iter().map(String::as_str))
                    .unwrap_or_default();
                for (_session_id, catalog_entry) in catalog_entries {
                    if summaries.len() >= 2_000 {
                        break;
                    }
                    let Some(meta) = catalog_entry.meta.as_ref() else {
                        continue;
                    };
                    let selection = advertised_selection_from_catalog_entry(
                        &catalog_entry,
                        &catalog,
                        &project_config,
                        &models,
                    )
                    .unwrap_or_else(|| fallback.clone());
                    if let Ok(summary) =
                        summary_from_meta(meta, Some(public_project_id.clone()), selection)
                    {
                        summaries.push(summary);
                    }
                }
            }
            drop(projects);
            // Snapshot evidence only after the blocking inventory scan, without
            // holding its mutex across transcript I/O or waiting on the async
            // runtime when a persistence transaction is finishing.
            let pull_requests = pull_requests
                .lock()
                .map_err(|_| ServiceError::Internal)?
                .summaries();
            for summary in &mut summaries {
                summary.pull_request = pull_requests.get(summary.id.as_str()).cloned();
            }
            Ok(summaries)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<Self::Driver, ServiceError> {
        self.driver_for_new(request)
    }

    async fn open_session(&self, session_id: &SessionId) -> Result<Self::Driver, ServiceError> {
        if session_id.as_str().starts_with(DELEGATED_SESSION_PREFIX) {
            self.driver_for_delegated_session(session_id)
        } else {
            self.driver_for_existing(session_id)
        }
    }
}
