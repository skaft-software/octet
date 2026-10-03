//! Session catalog metadata and titles.

use super::*;

#[cfg(test)]
pub(super) fn session_meta_for_id(
    store: &SessionStore,
    session_id: &SessionId,
) -> Option<SessionMeta> {
    store.get_by_id(session_id.as_str()).ok().flatten()
}

pub(super) fn session_meta_for_open_session(
    store: &SessionStore,
    session_id: &SessionId,
    session: &Session,
) -> Option<SessionMeta> {
    store
        .meta_for_open_session(session_id.as_str(), session)
        .ok()
        .flatten()
}

#[cfg(test)]
pub(super) fn changed_session_title(
    store: &SessionStore,
    session_id: &SessionId,
    previous: Option<&str>,
) -> Option<String> {
    let title = session_meta_for_id(store, session_id)?.title;
    (title != "(empty session)" && !title.trim().is_empty() && previous != Some(title.as_str()))
        .then_some(title)
}

pub(super) fn summary_from_meta(
    meta: &SessionMeta,
    project_id: Option<ProjectId>,
    model: ModelSelection,
) -> Result<SessionSummary, ServiceError> {
    let id = SessionId::new(meta.id.clone()).map_err(|_| ServiceError::InvalidSeed)?;
    let modified_at_ms = system_time_ms(meta.modified);
    let (lifecycle, retention, forked_from) = session_catalog_metadata(meta, &id)?;
    Ok(SessionSummary {
        id,
        project_id,
        title: bounded_text(&meta.title, 512),
        tags: meta.tags.iter().map(|tag| bounded_text(tag, 64)).collect(),
        created_at_ms: modified_at_ms,
        modified_at_ms,
        pinned: meta.pinned,
        archived: meta.archived,
        lifecycle,
        retention,
        forked_from,
        provisional: false,
        live_state: SessionLiveState::Idle,
        attention: AttentionState::None,
        pull_request: None,
        owner: ActorOwnerState::Inactive,
        model,
    })
}

pub(super) fn session_catalog_metadata(
    meta: &SessionMeta,
    session_id: &SessionId,
) -> Result<
    (
        SessionCatalogState,
        Option<SessionRetention>,
        Option<ConversationBranchProvenance>,
    ),
    ServiceError,
> {
    let (lifecycle, retention) = match (meta.trashed_at_ms, meta.purge_after_ms) {
        (Some(trashed_at_ms), Some(purge_after_ms)) => (
            SessionCatalogState::Trash,
            Some(SessionRetention {
                trashed_at_ms,
                purge_after_ms,
                permanent_delete_requires_confirmation: true,
            }),
        ),
        (None, None) if meta.archived => (SessionCatalogState::Archived, None),
        (None, None) => (SessionCatalogState::Active, None),
        _ => return Err(ServiceError::InvalidSeed),
    };
    let forked_from = match (
        meta.forked_from_session_id.as_deref(),
        meta.forked_from_entry_id.as_deref(),
    ) {
        (None, None) => None,
        (Some(source_session_id), Some(source_entry_id)) => Some(ConversationBranchProvenance {
            operation: ConversationBranchOperation::ForkSession,
            source_session_id: SessionId::new(source_session_id.to_owned())
                .map_err(|_| ServiceError::InvalidSeed)?,
            source_entry_id: DurableEntryId::new(source_entry_id.to_owned())
                .map_err(|_| ServiceError::InvalidSeed)?,
            originating_user_entry_id: None,
            model_override: None,
            external_effects_preserved: true,
            warning: EXTERNAL_EFFECTS_WARNING.to_owned(),
        }),
        _ => return Err(ServiceError::InvalidSeed),
    };
    let _ = session_id;
    Ok((lifecycle, retention, forked_from))
}

pub(super) fn session_id_from_path(path: &Path) -> Result<SessionId, ServiceError> {
    SessionId::new(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or(ServiceError::InvalidSeed)?
            .to_owned(),
    )
    .map_err(|_| ServiceError::InvalidSeed)
}
