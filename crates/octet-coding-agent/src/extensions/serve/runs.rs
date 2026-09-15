//! Serialized Serve worker execution and its admission plan.
//!
//! This module owns the command mailbox contract and the long-lived worker loop.
//! It deliberately borrows projection, recovery, and transport operations from
//! the parent composition module so the worker remains the single run boundary.

use super::*;

pub(super) struct WorkerCommand {
    pub(super) command: SessionCommand,
    pub(super) response: oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>,
}

pub(super) enum WorkerMessage {
    Command(WorkerCommand),
    CommandDiscovery {
        response: oneshot::Sender<Result<CommandDiscovery, ServiceError>>,
    },
}

pub(super) struct WorkerPlan {
    pub(super) config: Config,
    pub(super) sessions: SessionStore,
    pub(super) launch: LaunchSelection,
    pub(super) prepared_session: Mutex<Option<Session>>,
    pub(super) authority: AuthorityProfile,
    pub(super) available_models: Vec<ModelSummary>,
    pub(super) actor_generation: u64,
    pub(super) session_id: SessionId,
    pub(super) project_id: Option<ProjectId>,
    pub(super) attachments: Option<AttachmentStore>,
    pub(super) documents: Option<DocumentStore>,
    pub(super) projects: Arc<Mutex<ProjectRegistry>>,
    pub(super) trusted_files: Arc<Mutex<HashMap<String, TrustedProjectFiles>>>,
    pub(super) search_index: Arc<Mutex<TranscriptSearchIndex>>,
    pub(super) resources: Option<octet_serve_backend::ResourceStore>,
    pub(super) goal_store: Option<GoalStore>,
    pub(super) usage: Arc<Mutex<InferenceRequestStore>>,
    pub(super) pull_requests: Arc<Mutex<PullRequestStore>>,
    pub(super) pull_request_projection: Arc<Mutex<Option<PullRequestSummary>>>,
    pub(super) pull_request_discovery_enabled: Arc<AtomicBool>,
    pub(super) pull_request_refresh_requested: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    pub(super) checkout_hooks: CheckoutTestHooks,
}

pub(super) async fn run_worker(
    mut plan: WorkerPlan,
    mut commands: mpsc::Receiver<WorkerMessage>,
    events: mpsc::Sender<TimestampedEvent>,
    known_entries: usize,
) {
    let mut app: Option<App> = None;
    let mut projection = ProjectionState::new(known_entries);
    let pull_request_refresh = tokio::spawn(run_hosted_pull_request_refresh(
        PullRequestRefreshPlan::from(&plan),
        events.clone(),
    ));
    let goal_driver = plan.goal_store.as_ref().map(|store| {
        GoalDriver::new(
            Arc::new(ServeGoalStore {
                store: store.clone(),
            }),
            plan.session_id.as_str(),
        )
    });
    let mut goal_deadline = match goal_driver.as_ref() {
        Some(driver)
            if current_goal(plan.goal_store.as_ref(), &plan.session_id)
                .ok()
                .flatten()
                .is_some_and(|goal| matches!(goal.status, octet_agent::GoalStatus::Active)) =>
        {
            match driver.turn_settled(GoalTurnSource::User, "", false) {
                Ok(decision) => schedule_goal(Some(decision)),
                Err(_) => {
                    let _ = driver.session_error();
                    None
                }
            }
        }
        _ => None,
    };
    let mut extension_refresh = tokio::time::interval(std::time::Duration::from_millis(250));
    extension_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let message = tokio::select! {
            message = commands.recv() => message,
            _ = extension_refresh.tick() => {
                if let Some(owned_app) = app.as_mut() {
                    let _ = publish_extension_presentations(
                        &mut owned_app.executable_extensions,
                        &mut projection,
                        &events,
                    ).await;
                }
                continue;
            }
            _ = async {
                if let Some(deadline) = goal_deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                goal_deadline = None;
                let Some(driver) = goal_driver.as_ref() else {
                    continue;
                };
                let mut owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = driver.session_error();
                            continue;
                        }
                    },
                };
                let continuation = match driver.fire_continuation() {
                    Ok(continuation) => continuation,
                    Err(_) => {
                        let _ = driver.session_error();
                        None
                    }
                };
                let Some(continuation) = continuation else {
                    app = Some(owned_app);
                    continue;
                };
                if let Ok(goal_event) =
                    current_goal_event(plan.goal_store.as_ref(), &plan.session_id)
                {
                    let _ = events.send(goal_event).await;
                }
                let session_path = owned_app.agent.session().path().to_owned();
                plan.launch.session = SessionSelection::OpenExisting(session_path);
                let input = PromptInput {
                    text: continuation.prompt,
                    attachments: Vec::new(),
                    document_ids: Vec::new(),
                    project_file_ids: Vec::new(),
                };
                match start_and_drive_run(
                    &mut owned_app,
                    RunPromptInput::New(input),
                    None,
                    Some(driver),
                    GoalTurnSource::Continuation,
                    &plan,
                    &mut projection,
                    &mut commands,
                    &events,
                    None,
                )
                .await
                {
                    Ok(RunDriveOutcome::Admitted { goal }) => {
                        goal_deadline = schedule_goal(goal);
                        app = Some(owned_app);
                    }
                    Ok(RunDriveOutcome::Rejected { admission, error }) => {
                        let _ = driver.session_error();
                        if let Some(admission) = admission {
                            let _ = admission.send(Err(error));
                        }
                        app = Some(owned_app);
                    }
                    Err(_) => {
                        let _ = driver.session_error();
                        let _ = events
                            .send(event(EventPayload::SessionStateChanged {
                                state: SessionLiveState::Failed,
                                active_run_id: None,
                            }))
                            .await;
                        app = Some(owned_app);
                    }
                }
                continue;
            }
        };
        let Some(message) = message else {
            break;
        };
        let message = match message {
            WorkerMessage::Command(message) => message,
            WorkerMessage::CommandDiscovery { response } => {
                let result = match app.as_ref() {
                    Some(app) => build_command_discovery(app),
                    None => match build_worker_app(&mut plan) {
                        Ok(owned_app) => {
                            let discovery = build_command_discovery(&owned_app);
                            app = Some(owned_app);
                            discovery
                        }
                        Err(_) => Err(ServiceError::Internal),
                    },
                };
                let _ = response.send(result);
                continue;
            }
        };
        match message.command {
            command @ (SessionCommand::SetGoal { .. }
            | SessionCommand::PauseGoal
            | SessionCommand::ResumeGoal
            | SessionCommand::ClearGoal) => {
                let prior_goal_deadline = goal_deadline;
                let outcome = goal_mutation_outcome(&plan, command);
                goal_deadline = if outcome.is_ok() {
                    goal_deadline_after_user_change(goal_driver.as_ref()).unwrap_or_default()
                } else {
                    // Rejected mutations must not cancel a continuation that
                    // was already waiting for its grace-period deadline.
                    prior_goal_deadline
                };
                let _ = message.response.send(outcome);
            }
            SessionCommand::SubmitPrompt { input } => {
                goal_deadline = None;
                if let Some(driver) = goal_driver.as_ref() {
                    driver.user_spoke();
                }
                let mut owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let session_path = owned_app.agent.session().path().to_owned();
                plan.launch.session = SessionSelection::OpenExisting(session_path);
                match start_and_drive_run(
                    &mut owned_app,
                    RunPromptInput::New(input),
                    None,
                    goal_driver.as_ref(),
                    GoalTurnSource::User,
                    &plan,
                    &mut projection,
                    &mut commands,
                    &events,
                    Some(message.response),
                )
                .await
                {
                    Ok(RunDriveOutcome::Admitted { goal }) => {
                        goal_deadline = schedule_goal(goal);
                        app = Some(owned_app);
                    }
                    Ok(RunDriveOutcome::Rejected { admission, error }) => {
                        if let Some(admission) = admission {
                            let _ = admission.send(Err(error));
                        }
                        app = Some(owned_app);
                    }
                    Err(_) => {
                        if let Some(driver) = goal_driver.as_ref() {
                            let _ = driver.session_error();
                        }
                        let _ = events
                            .send(event(EventPayload::SessionStateChanged {
                                state: SessionLiveState::Failed,
                                active_run_id: None,
                            }))
                            .await;
                        app = Some(owned_app);
                    }
                }
            }
            SessionCommand::EditUserTurn {
                source_user_entry_id,
                input,
            } => {
                goal_deadline = None;
                if let Some(driver) = goal_driver.as_ref() {
                    driver.user_spoke();
                }
                let owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let source_entry = EntryId(source_user_entry_id.as_str().to_owned());
                if owned_app
                    .agent
                    .session()
                    .entry(&source_entry)
                    .is_none_or(|entry| !is_user_authored_entry(entry))
                {
                    app = Some(owned_app);
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                }
                let provenance = ConversationBranchProvenance {
                    operation: ConversationBranchOperation::EditUserTurn,
                    source_session_id: plan.session_id.clone(),
                    source_entry_id: source_user_entry_id,
                    originating_user_entry_id: None,
                    model_override: None,
                    external_effects_preserved: true,
                    warning: EXTERNAL_EFFECTS_WARNING.to_owned(),
                };
                match drive_sibling_conversation_branch(
                    owned_app,
                    source_entry,
                    RunPromptInput::New(input),
                    provenance,
                    None,
                    goal_driver.as_ref(),
                    &mut plan,
                    &mut projection,
                    &mut commands,
                    &events,
                    message.response,
                )
                .await
                {
                    Ok((owned_app, post_ack_failed, goal)) => {
                        goal_deadline = schedule_goal(goal);
                        app = Some(owned_app);
                        if post_ack_failed {
                            let _ = events
                                .send(event(EventPayload::SessionStateChanged {
                                    state: SessionLiveState::Failed,
                                    active_run_id: None,
                                }))
                                .await;
                        }
                    }
                    Err(_) => {
                        app = None;
                        break;
                    }
                }
            }
            SessionCommand::RetryResponse {
                source_assistant_entry_id,
                model,
            } => {
                goal_deadline = None;
                if let Some(driver) = goal_driver.as_ref() {
                    driver.user_spoke();
                }
                let owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let assistant_entry = EntryId(source_assistant_entry_id.as_str().to_owned());
                let source_user_entry =
                    match retry_originating_user_entry(owned_app.agent.session(), &assistant_entry)
                    {
                        Ok(entry) => entry,
                        Err(error) => {
                            app = Some(owned_app);
                            let _ = message.response.send(Err(error));
                            continue;
                        }
                    };
                let replay =
                    match replay_prompt_input(owned_app.agent.session(), &source_user_entry, &plan)
                    {
                        Ok(replay) => replay,
                        Err(error) => {
                            app = Some(owned_app);
                            let _ = message.response.send(Err(error));
                            continue;
                        }
                    };
                let originating_user_entry_id =
                    match DurableEntryId::new(source_user_entry.0.clone()) {
                        Ok(entry) => entry,
                        Err(_) => {
                            app = Some(owned_app);
                            let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                            continue;
                        }
                    };
                let provenance = ConversationBranchProvenance {
                    operation: ConversationBranchOperation::RetryResponse,
                    source_session_id: plan.session_id.clone(),
                    source_entry_id: source_assistant_entry_id,
                    originating_user_entry_id: Some(originating_user_entry_id),
                    model_override: model.clone(),
                    external_effects_preserved: true,
                    warning: EXTERNAL_EFFECTS_WARNING.to_owned(),
                };
                match drive_sibling_conversation_branch(
                    owned_app,
                    source_user_entry,
                    RunPromptInput::Replay(replay),
                    provenance,
                    model,
                    goal_driver.as_ref(),
                    &mut plan,
                    &mut projection,
                    &mut commands,
                    &events,
                    message.response,
                )
                .await
                {
                    Ok((owned_app, post_ack_failed, goal)) => {
                        goal_deadline = schedule_goal(goal);
                        app = Some(owned_app);
                        if post_ack_failed {
                            let _ = events
                                .send(event(EventPayload::SessionStateChanged {
                                    state: SessionLiveState::Failed,
                                    active_run_id: None,
                                }))
                                .await;
                        }
                    }
                    Err(_) => {
                        app = None;
                        break;
                    }
                }
            }
            SessionCommand::ForkConversation { entry_id } => {
                let owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let sessions = plan.sessions.clone();
                let source_session_id = plan.session_id.clone();
                let project_id = plan.project_id.clone();
                let projects = Arc::clone(&plan.projects);
                let fork = tokio::task::spawn_blocking(move || {
                    let result = create_conversation_fork(
                        &owned_app,
                        &sessions,
                        &source_session_id,
                        project_id.as_ref(),
                        &projects,
                        &entry_id,
                    );
                    (owned_app, result)
                })
                .await;
                let (owned_app, result) = match fork {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        app = None;
                        let _ = message.response.send(Err(ServiceError::Internal));
                        continue;
                    }
                };
                match result {
                    Ok(created_session_id) => {
                        let outcome = DriverCommandOutcome::fork(created_session_id.clone());
                        if message.response.send(Ok(outcome)).is_err() {
                            let _ = rollback_conversation_fork(&plan, &created_session_id);
                        }
                        app = Some(owned_app);
                    }
                    Err(error) => {
                        app = Some(owned_app);
                        let _ = message.response.send(Err(error));
                    }
                }
            }
            SessionCommand::Checkout { entry_id } => {
                let mut owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let path = owned_app.agent.session().path().to_owned();
                let Some(previous_head) = owned_app.agent.session().head() else {
                    app = Some(owned_app);
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                };
                if owned_app
                    .agent
                    .session_mut()
                    .checkout(EntryId(entry_id.as_str().to_owned()))
                    .is_err()
                {
                    app = Some(owned_app);
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                }

                let selection = SessionSelection::OpenExisting(path.clone());
                let rebuilt =
                    match rebuild_app(owned_app, None, None, None, Some(selection.clone())) {
                        Ok(rebuilt) => rebuilt,
                        Err(_) => {
                            match checkout_rejection_after_rollback(
                                restore_checkout_owner(&path, previous_head, &mut plan),
                                ServiceError::Internal,
                            ) {
                                Ok((restored, rejection)) => {
                                    app = Some(restored);
                                    let _ = message.response.send(Err(rejection));
                                    continue;
                                }
                                Err(owner_lost) => {
                                    app = None;
                                    let _ = message.response.send(Err(owner_lost));
                                    break;
                                }
                            }
                        }
                    };
                let model = selection_for_model(&rebuilt.model, &rebuilt.reasoning, &plan.config);
                let mut replacement = seed_from_session(
                    rebuilt.agent.session(),
                    plan.session_id.clone(),
                    SessionSeedOptions {
                        workspace: &plan.config.workspace,
                        project_id: plan.project_id.clone(),
                        model,
                        authority: plan.authority,
                        generation: plan.actor_generation,
                        meta: plan
                            .sessions
                            .meta_for_open_session(
                                plan.session_id.as_str(),
                                rebuilt.agent.session(),
                            )
                            .ok()
                            .flatten(),
                        attachment_store: plan.attachments.as_ref(),
                        resource_store: plan.resources.as_ref(),
                    },
                );
                if let (Ok(seed), Ok(pull_request)) =
                    (replacement.as_mut(), plan.pull_request_projection.lock())
                {
                    // Projection replacement runs on the serialized command
                    // worker. Read its in-memory actor projection rather than
                    // contending with blocking-pool evidence persistence.
                    seed.summary.pull_request = pull_request.clone();
                }
                #[cfg(test)]
                {
                    if plan.checkout_hooks.fail_seed_after_checkout {
                        replacement = Err(ServiceError::InvalidSeed);
                    } else if plan.checkout_hooks.corrupt_replacement_identity {
                        if let Ok(seed) = replacement.as_mut() {
                            let wrong =
                                SessionId::new("test-corrupt-replacement").expect("test ID");
                            seed.summary.id = wrong.clone();
                            seed.snapshot.session_id = wrong;
                        }
                    }
                }
                match replacement {
                    Ok(seed) => {
                        let (outcome, mut finalizer) = DriverCommandOutcome::guarded_replace(seed);
                        let _ = message.response.send(Ok(outcome));
                        match finalizer.decision().await {
                            Ok(FinalizeDecision::Commit) => {
                                plan.launch.model = rebuilt.model.spec.id.clone();
                                plan.launch.reasoning = rebuilt.reasoning.clone();
                                plan.launch.reasoning_mode = rebuilt.reasoning_mode;
                                plan.launch.session = selection;
                                projection.begin_run();
                                projection.known_entries = rebuilt.agent.session().entries().len();
                                app = Some(rebuilt);
                                let _ = finalizer.complete(Ok(FinalizeCompletion::Committed));
                            }
                            Ok(FinalizeDecision::Rollback) => {
                                wait_for_checkout_rollback_gate(&plan).await;
                                match rollback_checkout_candidate(
                                    rebuilt,
                                    &path,
                                    previous_head,
                                    &mut plan,
                                ) {
                                    Ok(restored) => {
                                        app = Some(restored);
                                        let _ =
                                            finalizer.complete(Ok(FinalizeCompletion::RolledBack));
                                    }
                                    Err(_) => {
                                        app = None;
                                        let _ = finalizer.complete(Err(ServiceError::OwnerLost));
                                    }
                                }
                            }
                            Err(_) => {
                                wait_for_checkout_rollback_gate(&plan).await;
                                app = rollback_checkout_candidate(
                                    rebuilt,
                                    &path,
                                    previous_head,
                                    &mut plan,
                                )
                                .ok();
                                if app.is_none() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        match checkout_rejection_after_rollback(
                            rollback_checkout_candidate(rebuilt, &path, previous_head, &mut plan),
                            error,
                        ) {
                            Ok((restored, rejection)) => {
                                app = Some(restored);
                                let _ = message.response.send(Err(rejection));
                            }
                            Err(owner_lost) => {
                                app = None;
                                let _ = message.response.send(Err(owner_lost));
                                break;
                            }
                        }
                    }
                }
            }
            SessionCommand::InvokeExtensionAction {
                extension,
                extension_instance_id,
                generation,
                revision,
                action,
                confirmed,
            } => {
                let mut owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let result = owned_app
                    .executable_extensions
                    .execute_presentation_action_for_serve(
                        &extension,
                        &extension_instance_id,
                        generation,
                        revision,
                        &action,
                        confirmed,
                    )
                    .await
                    .map(|_| DriverCommandOutcome::default())
                    .map_err(|_| ServiceError::InvalidBoundary);
                if result.is_ok() {
                    let _ = publish_extension_presentations(
                        &mut owned_app.executable_extensions,
                        &mut projection,
                        &events,
                    )
                    .await;
                }
                app = Some(owned_app);
                let _ = message.response.send(result);
            }
            SessionCommand::InvokeSlashCommand { invocation } => {
                let owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let (next_app, mut result) =
                    invoke_idle_slash_command(owned_app, invocation, &mut plan, &mut projection)
                        .await;
                if let Some(owned_app) = next_app.as_ref() {
                    projection.usage_uncertain |= owned_app.agent.session().has_uncertain_usage();
                    if let Err(error) =
                        publish_idle_accounting_context(&mut projection, &events).await
                    {
                        result = Err(error);
                    }
                }
                match (next_app, result) {
                    (Some(mut owned_app), Ok(SlashInvocationOutcome::Start(input))) => {
                        let session_path = owned_app.agent.session().path().to_owned();
                        plan.launch.session = SessionSelection::OpenExisting(session_path);
                        match start_and_drive_run(
                            &mut owned_app,
                            input,
                            None,
                            goal_driver.as_ref(),
                            GoalTurnSource::User,
                            &plan,
                            &mut projection,
                            &mut commands,
                            &events,
                            Some(message.response),
                        )
                        .await
                        {
                            Ok(RunDriveOutcome::Admitted { goal }) => {
                                goal_deadline = schedule_goal(goal);
                                app = Some(owned_app);
                            }
                            Ok(RunDriveOutcome::Rejected { admission, error }) => {
                                if let Some(admission) = admission {
                                    let _ = admission.send(Err(error));
                                }
                                app = Some(owned_app);
                            }
                            Err(_) => {
                                let _ = events
                                    .send(event(EventPayload::SessionStateChanged {
                                        state: SessionLiveState::Failed,
                                        active_run_id: None,
                                    }))
                                    .await;
                                app = Some(owned_app);
                            }
                        }
                    }
                    (Some(owned_app), Ok(SlashInvocationOutcome::Immediate(outcome))) => {
                        app = Some(owned_app);
                        let _ = message.response.send(Ok(*outcome));
                    }
                    (Some(owned_app), Err(error)) => {
                        app = Some(owned_app);
                        let _ = message.response.send(Err(error));
                    }
                    (None, _) => {
                        let _ = message.response.send(Err(ServiceError::OwnerLost));
                    }
                }
            }
            SessionCommand::ChangeModel { provider, model } => {
                let Some(summary) = plan
                    .available_models
                    .iter()
                    .find(|summary| summary.provider == provider && summary.id == model)
                    .cloned()
                else {
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                };
                let outcome = if let Some(owned_app) = app.take() {
                    match crate::app::apply_reconfig(
                        owned_app,
                        Reconfig::Model(ModelId(model.clone())),
                    ) {
                        Ok(rebuilt) => {
                            plan.launch.model = rebuilt.model.spec.id.clone();
                            plan.launch.reasoning = rebuilt.reasoning.clone();
                            plan.launch.session = SessionSelection::OpenExisting(
                                rebuilt.agent.session().path().to_owned(),
                            );
                            let selection = selection_for_model(
                                &rebuilt.model,
                                &rebuilt.reasoning,
                                &plan.config,
                            );
                            let outcome = reconfiguration_outcome(
                                &rebuilt,
                                &plan,
                                &mut projection,
                                selection,
                                plan.authority,
                            );
                            app = Some(rebuilt);
                            outcome
                        }
                        Err(_) => {
                            app = build_worker_app(&mut plan).ok();
                            Err(ServiceError::Internal)
                        }
                    }
                } else {
                    let next_reasoning_label = summary
                        .default_reasoning
                        .clone()
                        .or_else(|| summary.reasoning.first().cloned())
                        .unwrap_or_else(|| "off".into());
                    let next_reasoning = config::parse_reasoning(&next_reasoning_label)
                        .unwrap_or(ReasoningConfig::Off);
                    let previous_model = plan.launch.model.clone();
                    let previous_reasoning = plan.launch.reasoning.clone();
                    plan.launch.model = ModelId(model);
                    plan.launch.reasoning = next_reasoning;
                    let selection = ModelSelection {
                        provider,
                        model: plan.launch.model.0.clone(),
                        reasoning: next_reasoning_label,
                    };
                    match persist_idle_selection(&mut plan, &mut projection, selection) {
                        Ok(outcome) => Ok(outcome),
                        Err(error) => {
                            plan.launch.model = previous_model;
                            plan.launch.reasoning = previous_reasoning;
                            Err(error)
                        }
                    }
                };
                let _ = message.response.send(outcome);
            }
            SessionCommand::ChangeReasoning { reasoning } => {
                let Some(summary) = plan
                    .available_models
                    .iter()
                    .find(|summary| summary.id == plan.launch.model.0)
                    .cloned()
                else {
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                };
                if !summary.reasoning.iter().any(|choice| choice == &reasoning) {
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                }
                let provider = summary.provider.clone();
                let parsed = match config::parse_reasoning(&reasoning) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                        continue;
                    }
                };
                let outcome = if let Some(owned_app) = app.take() {
                    match crate::app::apply_reconfig(owned_app, Reconfig::Thinking(parsed.clone()))
                    {
                        Ok(rebuilt) => {
                            plan.launch.model = rebuilt.model.spec.id.clone();
                            plan.launch.reasoning = rebuilt.reasoning.clone();
                            plan.launch.session = SessionSelection::OpenExisting(
                                rebuilt.agent.session().path().to_owned(),
                            );
                            let selection = selection_for_model(
                                &rebuilt.model,
                                &rebuilt.reasoning,
                                &plan.config,
                            );
                            let outcome = reconfiguration_outcome(
                                &rebuilt,
                                &plan,
                                &mut projection,
                                selection,
                                plan.authority,
                            );
                            app = Some(rebuilt);
                            outcome
                        }
                        Err(_) => {
                            app = build_worker_app(&mut plan).ok();
                            Err(ServiceError::Internal)
                        }
                    }
                } else {
                    let previous_reasoning = plan.launch.reasoning.clone();
                    plan.launch.reasoning = parsed;
                    let selection = ModelSelection {
                        provider,
                        model: plan.launch.model.0.clone(),
                        reasoning,
                    };
                    match persist_idle_selection(&mut plan, &mut projection, selection) {
                        Ok(outcome) => Ok(outcome),
                        Err(error) => {
                            plan.launch.reasoning = previous_reasoning;
                            Err(error)
                        }
                    }
                };
                let _ = message.response.send(outcome);
            }
            SessionCommand::Rename { title } => {
                let _ = message.response.send(rename_session_outcome(&plan, &title));
            }
            SessionCommand::SetPinned { pinned } => {
                let _ = message.response.send(pin_session_outcome(&plan, pinned));
            }
            SessionCommand::SetArchived { archived } => {
                let _ = message
                    .response
                    .send(archive_session_outcome(&plan, archived));
            }
            _ => {
                let _ = message.response.send(Err(ServiceError::InvalidBoundary));
            }
        }
    }
    pull_request_refresh.abort();
    let _ = pull_request_refresh.await;
    shutdown_worker_app(&mut app).await;
}
