//! Conversation branch checkout, forks and their rollback.

use super::*;

#[cfg(test)]
#[derive(Clone, Default)]
pub(super) struct CheckoutTestHooks {
    pub(super) rollback_gate: Option<CheckoutRollbackGate>,
    pub(super) corrupt_replacement_identity: bool,
    pub(super) fail_seed_after_checkout: bool,
    pub(super) fail_rollback: bool,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct CheckoutRollbackGate {
    pub(super) entered: Arc<tokio::sync::Barrier>,
    pub(super) release: Arc<tokio::sync::Barrier>,
}

pub(super) fn ensure_durable_session(plan: &WorkerPlan) -> Result<(), ServiceError> {
    match &plan.launch.session {
        SessionSelection::OpenExisting(path) if path.is_file() => Ok(()),
        SessionSelection::CreateNew(_) | SessionSelection::OpenExisting(_) => {
            Err(ServiceError::InvalidBoundary)
        }
    }
}

pub(super) fn restore_session_head(
    path: &std::path::Path,
    head: EntryId,
) -> Result<(), ServiceError> {
    let mut session = Session::open(path).map_err(|_| ServiceError::Internal)?;
    session.checkout(head).map_err(|_| ServiceError::Internal)
}

pub(super) fn checkout_before_user_entry(
    session: &mut Session,
    source_user_entry_id: &EntryId,
) -> Result<(), ServiceError> {
    let source = session
        .entry(source_user_entry_id)
        .ok_or(ServiceError::InvalidBoundary)?;
    if !is_user_authored_entry(source) {
        return Err(ServiceError::InvalidBoundary);
    }
    match source.parent.clone() {
        Some(parent) => session
            .checkout(parent)
            .map_err(|_| ServiceError::InvalidBoundary),
        None => session
            .checkout_root()
            .map_err(|_| ServiceError::InvalidBoundary),
    }
}

pub(super) fn is_user_authored_entry(entry: &Entry) -> bool {
    matches!(
        &entry.value,
        EntryValue::Message(Message::User(message))
            if !message.content.is_empty()
                && message
                    .content
                    .iter()
                    .all(|part| matches!(part, UserPart::Text(_) | UserPart::Media(_)))
    )
}

pub(super) fn retry_originating_user_entry(
    session: &Session,
    source_assistant_entry_id: &EntryId,
) -> Result<EntryId, ServiceError> {
    let assistant = session
        .entry(source_assistant_entry_id)
        .ok_or(ServiceError::InvalidBoundary)?;
    if !matches!(&assistant.value, EntryValue::Message(Message::Assistant(_))) {
        return Err(ServiceError::InvalidBoundary);
    }
    let mut cursor = assistant.parent.as_ref();
    while let Some(id) = cursor {
        let entry = session.entry(id).ok_or(ServiceError::InvalidBoundary)?;
        if is_user_authored_entry(entry) {
            return Ok(entry.id.clone());
        }
        cursor = entry.parent.as_ref();
    }
    Err(ServiceError::InvalidBoundary)
}

pub(super) fn replay_prompt_input(
    session: &Session,
    source_user_entry_id: &EntryId,
    plan: &WorkerPlan,
) -> Result<ResolvedPromptInput, ServiceError> {
    let entry = session
        .entry(source_user_entry_id)
        .ok_or(ServiceError::InvalidBoundary)?;
    let EntryValue::Message(Message::User(message)) = &entry.value else {
        return Err(ServiceError::InvalidBoundary);
    };
    if !is_user_authored_entry(entry) {
        return Err(ServiceError::InvalidBoundary);
    }
    let mut model_text = String::new();
    for part in &message.content {
        if let UserPart::Text(text) = part {
            model_text.push_str(text);
        }
    }
    let display_text = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.display_text.clone())
        .unwrap_or_else(|| model_text.clone());
    let attachments = if message
        .content
        .iter()
        .any(|part| matches!(part, UserPart::Media(_)))
    {
        let store = plan.attachments.as_ref().ok_or(ServiceError::Unavailable)?;
        store
            .refs_for_entry(&plan.session_id, &entry.id.0)
            .map_err(attachment_service_error)?
            .ok_or(ServiceError::InvalidBoundary)?
    } else {
        Vec::new()
    };
    let (documents, project_files) = stored_prompt_context_for_entry(
        session,
        plan.resources.as_ref(),
        &plan.session_id,
        &entry.id.0,
    );
    let document_context_tokens = documents
        .iter()
        .map(|document| document.extracted_text_byte_count)
        .fold(0_u64, u64::saturating_add)
        .div_ceil(4);
    let project_file_context_tokens = project_files
        .iter()
        .map(|file| file.byte_len)
        .fold(0_u64, u64::saturating_add)
        .div_ceil(4);
    Ok(ResolvedPromptInput {
        display_text,
        model_text,
        attachments,
        documents,
        project_files,
        document_context_tokens,
        project_file_context_tokens,
    })
}

pub(super) fn stored_prompt_context_for_entry(
    session: &Session,
    resources: Option<&octet_serve_backend::ResourceStore>,
    session_id: &SessionId,
    durable_entry_id: &str,
) -> (Vec<DocumentReference>, Vec<TrustedFileEntry>) {
    let Some(resources) = resources else {
        return (Vec::new(), Vec::new());
    };
    for entry in session.entries().iter().rev() {
        if entry
            .metadata
            .as_ref()
            .is_none_or(|metadata| metadata.run_outcome.is_none())
        {
            continue;
        }
        let Ok(outcome_entry_id) = DurableEntryId::new(entry.id.0.clone()) else {
            continue;
        };
        let Some(record) = load_stored_run_record(resources, session_id, &outcome_entry_id) else {
            continue;
        };
        if let Some(item) = record
            .items
            .into_iter()
            .find(|item| item.durable_entry_id == durable_entry_id)
        {
            return (item.documents, item.project_files);
        }
    }
    (Vec::new(), Vec::new())
}

// These explicit actor-state and channel borrows document which branch owns
// each mutable subsystem; combining them into a broad context would weaken that boundary.
#[allow(clippy::too_many_arguments)]
pub(super) async fn drive_sibling_conversation_branch(
    mut owned_app: App,
    source_user_entry_id: EntryId,
    input: RunPromptInput,
    provenance: ConversationBranchProvenance,
    model_override: Option<ModelSelection>,
    goal_driver: Option<&GoalDriver>,
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
    commands: &mut mpsc::Receiver<WorkerMessage>,
    events: &mpsc::Sender<TimestampedEvent>,
    admission: oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>,
) -> Result<(App, bool, Option<GoalDecision>), ServiceError> {
    let path = owned_app.agent.session().path().to_owned();
    let previous_head = owned_app
        .agent
        .session()
        .head()
        .ok_or(ServiceError::InvalidBoundary)?;
    let (new_model, new_reasoning) = match model_override.as_ref() {
        Some(selection) => {
            let available = plan.available_models.iter().any(|model| {
                model.available
                    && model.provider == selection.provider
                    && model.id == selection.model
                    && model
                        .reasoning
                        .iter()
                        .any(|reasoning| reasoning == &selection.reasoning)
            });
            if !available {
                let _ = admission.send(Err(ServiceError::InvalidBoundary));
                return Ok((owned_app, false, None));
            }
            let model = match owned_app.catalog.resolve(&ModelId(selection.model.clone())) {
                Ok(model) => model,
                Err(_) => {
                    let _ = admission.send(Err(ServiceError::InvalidBoundary));
                    return Ok((owned_app, false, None));
                }
            };
            let reasoning = match config::parse_reasoning(&selection.reasoning) {
                Ok(reasoning) => reasoning,
                Err(_) => {
                    let _ = admission.send(Err(ServiceError::InvalidBoundary));
                    return Ok((owned_app, false, None));
                }
            };
            (Some(model), Some(reasoning))
        }
        None => (None, None),
    };
    if let Err(error) =
        checkout_before_user_entry(owned_app.agent.session_mut(), &source_user_entry_id)
    {
        let _ = admission.send(Err(error));
        return Ok((owned_app, false, None));
    }
    let selection = SessionSelection::OpenExisting(path.clone());
    let mut candidate = match rebuild_app(
        owned_app,
        new_model,
        new_reasoning,
        None,
        Some(selection.clone()),
    ) {
        Ok(candidate) => candidate,
        Err(_) => {
            let restored = restore_checkout_owner(&path, previous_head, plan)?;
            let _ = admission.send(Err(ServiceError::Internal));
            return Ok((restored, false, None));
        }
    };
    let previous_model = plan.launch.model.clone();
    let previous_reasoning = plan.launch.reasoning.clone();
    let previous_reasoning_mode = plan.launch.reasoning_mode;
    plan.launch.model = candidate.model.spec.id.clone();
    plan.launch.reasoning = candidate.reasoning.clone();
    plan.launch.reasoning_mode = candidate.reasoning_mode;
    plan.launch.session = selection;
    match start_and_drive_run(
        &mut candidate,
        input,
        Some(provenance),
        goal_driver,
        GoalTurnSource::User,
        plan,
        projection,
        commands,
        events,
        Some(admission),
    )
    .await
    {
        Ok(RunDriveOutcome::Admitted { goal }) => Ok((candidate, false, goal)),
        Ok(RunDriveOutcome::Rejected { admission, error }) => {
            plan.launch.model = previous_model;
            plan.launch.reasoning = previous_reasoning;
            plan.launch.reasoning_mode = previous_reasoning_mode;
            let restored = rollback_checkout_candidate(candidate, &path, previous_head, plan)?;
            if let Some(admission) = admission {
                let _ = admission.send(Err(error));
            }
            Ok((restored, false, None))
        }
        Err(_) => Ok((candidate, true, None)),
    }
}

pub(super) fn create_conversation_fork(
    app: &App,
    sessions: &SessionStore,
    source_session_id: &SessionId,
    project_id: Option<&ProjectId>,
    projects: &Arc<Mutex<ProjectRegistry>>,
    source_entry_id: &DurableEntryId,
) -> Result<SessionId, ServiceError> {
    let source_entry = EntryId(source_entry_id.as_str().to_owned());
    let entry = app
        .agent
        .session()
        .entry(&source_entry)
        .ok_or(ServiceError::InvalidBoundary)?;
    if !matches!(
        &entry.value,
        EntryValue::Message(Message::User(_))
            | EntryValue::Message(Message::Assistant(_))
            | EntryValue::Compaction { .. }
    ) {
        return Err(ServiceError::InvalidBoundary);
    }
    let project_id = project_id
        .ok_or(ServiceError::InvalidBoundary)
        .and_then(registry_project_id)?;
    let destination = sessions.new_path(&crate::modes::timestamp());
    let created_session_id = session_id_from_path(&destination)?;
    let forked = app
        .agent
        .session()
        .fork_to(&destination, Some(&source_entry))
        .map_err(|_| ServiceError::Internal)?;
    drop(forked);
    if sessions
        .set_fork_provenance(
            created_session_id.as_str(),
            source_session_id.as_str(),
            source_entry_id.as_str(),
        )
        .is_err()
    {
        let _ = sessions.discard_unacknowledged(created_session_id.as_str());
        return Err(ServiceError::Internal);
    }
    let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
    if let Err(error) = projects.bind_session(created_session_id.as_str(), &project_id) {
        drop(projects);
        let _ = sessions.discard_unacknowledged(created_session_id.as_str());
        return Err(project_registry_service_error(error));
    }
    Ok(created_session_id)
}

pub(super) fn rollback_conversation_fork(
    plan: &WorkerPlan,
    created_session_id: &SessionId,
) -> Result<(), ServiceError> {
    let previous_project = {
        let mut projects = plan.projects.lock().map_err(|_| ServiceError::Internal)?;
        projects
            .unbind_session(created_session_id.as_str())
            .map_err(project_registry_service_error)?
    };
    if let Err(error) = plan
        .sessions
        .discard_unacknowledged(created_session_id.as_str())
    {
        if let Some(project_id) = previous_project {
            let mut projects = plan.projects.lock().map_err(|_| ServiceError::Internal)?;
            projects
                .bind_session(created_session_id.as_str(), &project_id)
                .map_err(project_registry_service_error)?;
        }
        let _ = error;
        return Err(ServiceError::Internal);
    }
    Ok(())
}

pub(super) fn rollback_checkout_candidate(
    mut candidate: App,
    path: &Path,
    previous_head: EntryId,
    plan: &mut WorkerPlan,
) -> Result<App, ServiceError> {
    candidate.executable_extensions.shutdown_blocking();
    drop(candidate);
    restore_checkout_owner(path, previous_head, plan)
}

pub(super) fn restore_checkout_owner(
    path: &Path,
    previous_head: EntryId,
    plan: &mut WorkerPlan,
) -> Result<App, ServiceError> {
    #[cfg(test)]
    if plan.checkout_hooks.fail_rollback {
        return Err(ServiceError::Internal);
    }
    restore_session_head(path, previous_head)?;
    build_worker_app(plan).map_err(|_| ServiceError::Internal)
}

#[cfg(test)]
pub(super) async fn wait_for_checkout_rollback_gate(plan: &WorkerPlan) {
    if let Some(gate) = &plan.checkout_hooks.rollback_gate {
        gate.entered.wait().await;
        gate.release.wait().await;
    }
}

#[cfg(not(test))]
pub(super) async fn wait_for_checkout_rollback_gate(_plan: &WorkerPlan) {}

pub(super) fn checkout_rejection_after_rollback<T>(
    rollback: Result<T, ServiceError>,
    rejection: ServiceError,
) -> Result<(T, ServiceError), ServiceError> {
    rollback
        .map(|owner| (owner, rejection))
        .map_err(|_| ServiceError::OwnerLost)
}

pub(super) fn session_metadata_outcome(
    title: Option<String>,
    pinned: Option<bool>,
    archived: Option<bool>,
) -> DriverCommandOutcome {
    DriverCommandOutcome::with_events(vec![event(EventPayload::SessionMetadataChanged {
        title,
        pinned,
        archived,
    })])
}
