//! Project ownership and accounting projections.
//!
//! Project registration is the authority boundary for Serve sessions. This
//! module keeps discovery, binding reconciliation, usage projection, and the
//! translation of registry failures out of transport handlers.

use super::*;

pub(super) fn reconcile_session_bindings(
    config: &Config,
    projects: &mut ProjectRegistry,
    include_untrusted: Option<&RegistryProjectId>,
) -> Result<(), ProjectRegistryError> {
    let eligible = projects
        .list()
        .into_iter()
        .filter_map(|project| {
            let explicitly_included = include_untrusted == Some(&project.id);
            (project.state == RegistryProjectState::Trusted || explicitly_included)
                .then_some((project.id, explicitly_included))
        })
        .collect::<Vec<_>>();
    let mut candidates = BTreeMap::<String, RegistryProjectId>::new();
    let mut ambiguous = BTreeSet::new();

    for (project_id, explicitly_included) in &eligible {
        let root = if *explicitly_included {
            projects.resolve_root(project_id)
        } else {
            projects.resolve_trusted_root(project_id)
        };
        let Ok(root) = root else {
            continue;
        };
        let sessions = SessionStore::new(&config.session_dir, root.as_path());
        for session_id in sessions.session_file_ids() {
            if projects.project_for_session(&session_id).is_some()
                || SessionId::new(session_id.clone()).is_err()
                || ambiguous.contains(&session_id)
            {
                continue;
            }
            match candidates.get(&session_id) {
                Some(existing) if existing != project_id => {
                    candidates.remove(&session_id);
                    ambiguous.insert(session_id);
                }
                Some(_) => {}
                None => {
                    candidates.insert(session_id, project_id.clone());
                }
            }
        }
    }

    for (project_id, _) in eligible {
        let session_ids = candidates
            .iter()
            .filter_map(|(session_id, candidate)| {
                (candidate == &project_id).then_some(session_id.as_str())
            })
            .collect::<Vec<_>>();
        projects.bind_sessions(&project_id, session_ids)?;
    }
    Ok(())
}

pub(super) fn backfill_usage_store(
    config: &Config,
    projects: &ProjectRegistry,
    usage: &mut InferenceRequestStore,
) -> anyhow::Result<()> {
    for project in projects.list() {
        let session_ids = projects.sessions_for_project(&project.id);
        if session_ids.is_empty() {
            continue;
        }
        // Archived projects still own accounting evidence. Unavailable roots
        // are not proof that their separately stored transcripts were deleted.
        let Ok(root) = projects.resolve_root_for_cleanup(&project.id) else {
            usage.mark_backfill_incomplete();
            continue;
        };
        let sessions = SessionStore::new(&config.session_dir, root.as_path());
        for session_id in session_ids {
            let inspection = match sessions.inspect_by_id(&session_id) {
                Ok(inspection) => inspection,
                Err(_) => {
                    if !matches!(sessions.session_file_exists(&session_id), Ok(false)) {
                        usage.mark_backfill_incomplete();
                    }
                    continue;
                }
            };
            if !inspection.usage_uncertainty_records.is_empty() {
                usage.record_uncertainty(&session_id)?;
            }
            usage.record_all(project_catalog_usage(
                &session_id,
                &inspection.usage_records,
            )?)?;
        }
    }
    Ok(())
}

pub(super) fn project_session_usage(
    session_id: &str,
    session: &Session,
) -> Result<Vec<InferenceRequest>, UsageStoreError> {
    session
        .usage_records()
        .iter()
        .enumerate()
        .map(|(ordinal, record)| {
            let request_ordinal =
                u64::try_from(ordinal).map_err(|_| UsageStoreError::InvalidRecord)?;
            Ok(InferenceRequest {
                session_id: session_id.to_owned(),
                request_ordinal,
                provider: record
                    .endpoint
                    .as_ref()
                    .map_or("unknown", |endpoint| endpoint.0.as_str())
                    .to_owned(),
                model: record
                    .model
                    .as_ref()
                    .map_or("unknown", |model| model.0.as_str())
                    .to_owned(),
                timestamp_ms: record.completed_at_unix_ms.unwrap_or_default(),
                prompt_tokens: record.usage.input_tokens,
                completion_tokens: record.usage.output_tokens,
                cache_read_tokens: record.usage.cache_read_tokens,
                cache_write_tokens: record.usage.cache_write_tokens,
                cache_write_1h_tokens: record.usage.cache_write_1h_tokens,
                reasoning_tokens: record.usage.reasoning_tokens,
                total_tokens: record.usage.total_tokens,
            })
        })
        .collect()
}

pub(super) fn project_catalog_usage(
    session_id: &str,
    records: &[SessionUsageRecord],
) -> Result<Vec<InferenceRequest>, UsageStoreError> {
    records
        .iter()
        .enumerate()
        .map(|(ordinal, record)| {
            let request_ordinal =
                u64::try_from(ordinal).map_err(|_| UsageStoreError::InvalidRecord)?;
            Ok(InferenceRequest {
                session_id: session_id.to_owned(),
                request_ordinal,
                provider: record.endpoint.as_deref().unwrap_or("unknown").to_owned(),
                model: record.model.as_deref().unwrap_or("unknown").to_owned(),
                timestamp_ms: record.completed_at_unix_ms.unwrap_or_default(),
                prompt_tokens: record.input_tokens,
                completion_tokens: record.output_tokens,
                cache_read_tokens: record.cache_read_tokens,
                cache_write_tokens: record.cache_write_tokens,
                cache_write_1h_tokens: record.cache_write_1h_tokens,
                reasoning_tokens: record.reasoning_tokens,
                total_tokens: record.total_tokens,
            })
        })
        .collect()
}

pub(super) fn sync_session_usage(
    usage: &Arc<Mutex<InferenceRequestStore>>,
    session_id: &SessionId,
    session: &Session,
) -> Result<(), ServiceError> {
    let requests =
        project_session_usage(session_id.as_str(), session).map_err(usage_store_service_error)?;
    let mut usage = usage.lock().map_err(|_| ServiceError::Internal)?;
    if session.has_uncertain_usage() {
        usage
            .record_uncertainty(session_id.as_str())
            .map_err(usage_store_service_error)?;
    }
    usage
        .record_all(requests)
        .map_err(usage_store_service_error)?;
    Ok(())
}

pub(super) fn usage_store_service_error(error: UsageStoreError) -> ServiceError {
    match error {
        UsageStoreError::QuotaExceeded => ServiceError::Unavailable,
        UsageStoreError::InvalidRecord
        | UsageStoreError::Conflict
        | UsageStoreError::Corrupt
        | UsageStoreError::Storage => ServiceError::Internal,
    }
}

pub(super) fn registry_project_id(
    project_id: &ProjectId,
) -> Result<RegistryProjectId, ServiceError> {
    RegistryProjectId::parse(project_id.as_str()).map_err(project_registry_service_error)
}

pub(super) fn project_registry_service_error(error: ProjectRegistryError) -> ServiceError {
    match error {
        ProjectRegistryError::ProjectNotFound => ServiceError::NotFound,
        ProjectRegistryError::ProjectUntrusted => ServiceError::Unauthorized,
        ProjectRegistryError::ProjectArchived => ServiceError::InvalidBoundary,
        ProjectRegistryError::RootUnavailable
        | ProjectRegistryError::RootIdentityChanged
        | ProjectRegistryError::RootSymlink
        | ProjectRegistryError::RootNotDirectory => ServiceError::Unavailable,
        ProjectRegistryError::RelativePath
        | ProjectRegistryError::PathTraversal
        | ProjectRegistryError::InvalidProjectId
        | ProjectRegistryError::ProjectLimitReached
        | ProjectRegistryError::InvalidDisplayName
        | ProjectRegistryError::InvalidCanonicalRoot
        | ProjectRegistryError::RootOverlapsState
        | ProjectRegistryError::DuplicateRoot
        | ProjectRegistryError::InvalidSessionId
        | ProjectRegistryError::SessionAlreadyBound
        | ProjectRegistryError::SessionBindingLimitReached => ServiceError::InvalidBoundary,
        ProjectRegistryError::StateParentUnavailable
        | ProjectRegistryError::UnsafeStatePath
        | ProjectRegistryError::UnsafePermissions
        | ProjectRegistryError::StateTooLarge
        | ProjectRegistryError::CorruptState
        | ProjectRegistryError::UnsupportedStateVersion
        | ProjectRegistryError::RevisionExhausted
        | ProjectRegistryError::RandomnessUnavailable
        | ProjectRegistryError::Storage(_) => ServiceError::Internal,
    }
}
