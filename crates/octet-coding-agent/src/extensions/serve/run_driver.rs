//! Starting and driving a run, and commands that arrive while it is active.

use super::*;

// A run nests the agent's provider stream under the long-lived Serve worker.
// Keep the full run state machine on the heap, rather than adding its poll frame
// to the worker's stack on every prompt.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_and_drive_run<'a>(
    app: &'a mut App,
    input: RunPromptInput,
    branch_provenance: Option<ConversationBranchProvenance>,
    goal_driver: Option<&'a GoalDriver>,
    goal_source: GoalTurnSource,
    plan: &'a WorkerPlan,
    projection: &'a mut ProjectionState,
    commands: &'a mut mpsc::Receiver<WorkerMessage>,
    events: &'a mpsc::Sender<TimestampedEvent>,
    admission: Option<oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<RunDriveOutcome, ServiceError>> + Send + 'a>,
> {
    Box::pin(start_and_drive_run_inner(
        app,
        input,
        branch_provenance,
        goal_driver,
        goal_source,
        plan,
        projection,
        commands,
        events,
        admission,
    ))
}

// Run orchestration keeps its independently borrowed actor state and channels
// visible rather than hiding them behind a mutable catch-all context.
#[allow(clippy::too_many_arguments)]
pub(super) async fn start_and_drive_run_inner(
    app: &mut App,
    input: RunPromptInput,
    branch_provenance: Option<ConversationBranchProvenance>,
    goal_driver: Option<&GoalDriver>,
    goal_source: GoalTurnSource,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
    commands: &mut mpsc::Receiver<WorkerMessage>,
    events: &mpsc::Sender<TimestampedEvent>,
    admission: Option<oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>>,
) -> Result<RunDriveOutcome, ServiceError> {
    if let Some(limit) = app.config.max_cost_microdollars {
        if app.agent.session().total_cost_microdollars() >= limit {
            return Ok(RunDriveOutcome::Rejected {
                admission,
                error: ServiceError::InvalidBoundary,
            });
        }
    }
    let (resolved, replay_exact) = match input {
        RunPromptInput::New(input) => match resolve_prompt_input(plan, input).await {
            Ok(resolved) => (resolved, false),
            Err(error) => {
                return Ok(RunDriveOutcome::Rejected { admission, error });
            }
        },
        RunPromptInput::Replay(resolved) => (resolved, true),
    };
    let ResolvedPromptInput {
        display_text,
        model_text,
        attachments,
        documents,
        project_files,
        document_context_tokens,
        project_file_context_tokens,
    } = resolved;
    let media = match resolve_attachment_media(app, plan, &attachments) {
        Ok(media) => media,
        Err(error) => {
            return Ok(RunDriveOutcome::Rejected { admission, error });
        }
    };
    let prompt = if replay_exact {
        model_text
    } else {
        match crate::prompts::render_configured(app, &model_text) {
            Err(_) => {
                return Ok(RunDriveOutcome::Rejected {
                    admission,
                    error: ServiceError::Internal,
                });
            }
            Ok(Some(rendered)) => rendered.text,
            Ok(None) => model_text,
        }
    };
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let command_discovery = match build_command_discovery(app) {
        Ok(discovery) => discovery,
        Err(error) => return Ok(RunDriveOutcome::Rejected { admission, error }),
    };
    let (pending_context_count, model_prompt, project_instruction_tokens) = if replay_exact {
        let project_instruction_tokens = project_instruction_token_hint(&app.system);
        app.agent.set_system_prompt(app.system.clone());
        (0, prompt, project_instruction_tokens)
    } else {
        let composition = match app
            .executable_extensions
            .compose_prompt(&app.system, prompt.clone())
            .await
        {
            Ok(composition) => composition,
            Err(_) => {
                return Ok(RunDriveOutcome::Rejected {
                    admission,
                    error: ServiceError::Internal,
                });
            }
        };
        let pending_context_count = composition.pending_context_count;
        let model_prompt = composition.prompt;
        let project_instruction_tokens = project_instruction_token_hint(&composition.system);
        app.agent.set_system_prompt(composition.system);
        (
            pending_context_count,
            model_prompt,
            project_instruction_tokens,
        )
    };
    app.agent
        .set_prompt_display_text(Some(display_text.clone()));
    projection.begin_run();
    let run_id = match projection.next_run_id(plan.actor_generation) {
        Ok(run_id) => run_id,
        Err(error) => return Ok(RunDriveOutcome::Rejected { admission, error }),
    };
    let turn_id = match projection.turn_id(&run_id) {
        Ok(turn_id) => turn_id,
        Err(error) => return Ok(RunDriveOutcome::Rejected { admission, error }),
    };
    let user_item_id = match projection.provisional_id(&run_id, "user", 0) {
        Ok(item_id) => item_id,
        Err(error) => return Ok(RunDriveOutcome::Rejected { admission, error }),
    };
    projection
        .item_turns
        .insert(user_item_id.clone(), turn_id.clone());
    let mut input_parts = Vec::with_capacity(1 + media.len());
    if !model_prompt.is_empty() {
        input_parts.push(InputPart::Text(model_prompt));
    }
    input_parts.extend(media.into_iter().map(InputPart::Media));
    let title_before_prompt =
        session_meta_for_open_session(&plan.sessions, &plan.session_id, app.agent.session())
            .map(|metadata| metadata.title);
    projection.usage_uncertain |= app.agent.session().has_uncertain_usage();
    let run_model = app.model.clone();
    let prior_cache_misses = crate::commands::cache_miss_count(&app);
    let mut run = match app.agent.prompt(UserInput::from(input_parts)).await {
        Ok(run) => run,
        Err(_) => {
            return Ok(RunDriveOutcome::Rejected {
                admission,
                error: ServiceError::Internal,
            });
        }
    };
    let extension_turn = app.executable_extensions.begin_turn().await;
    let mut context_projection = RunContextProjection::new(
        project_instruction_tokens,
        document_context_tokens,
        project_file_context_tokens,
    );
    context_projection.usage_uncertain = projection.usage_uncertain;
    if !attachments.is_empty() {
        projection
            .pending_attachments
            .push_back(attachments.clone());
    }
    projection.pending_user_items.push_back(PendingUserItem {
        id: user_item_id.clone(),
        delivery: UserMessageDelivery::Submit,
        turn_id: turn_id.clone(),
        documents: documents.clone(),
        project_files: project_files.clone(),
        document_context_tokens,
        project_file_context_tokens,
        context_attributed: true,
        branch_provenance: branch_provenance.clone(),
    });
    app.executable_extensions
        .commit_prompt_context(pending_context_count);
    let control = run.control();
    let mut immediate = Vec::with_capacity(3);
    let title_after_prompt = title_before_prompt.clone().or_else(|| {
        plan.sessions
            .load_metadata(plan.session_id.as_str())
            .ok()
            .and_then(|metadata| metadata.name)
            .or_else(|| {
                let title = crate::session_store::trim_title(&display_text);
                (!title.trim().is_empty()).then_some(title)
            })
    });
    if let Some(title) = title_after_prompt.filter(|title| {
        title != "(empty session)"
            && !title.trim().is_empty()
            && title_before_prompt.as_deref() != Some(title.as_str())
    }) {
        immediate.push(event(EventPayload::SessionMetadataChanged {
            title: Some(title),
            pinned: None,
            archived: None,
        }));
    }
    immediate.extend([
        event(EventPayload::ItemStarted {
            item: SessionItem {
                id: user_item_id.clone(),
                run_id: Some(run_id.clone()),
                turn_id: Some(turn_id),
                provider_attempt: None,
                lifecycle: ItemLifecycle::Provisional,
                durable_entry_id: None,
                payload: ItemPayload::UserMessage {
                    text: bounded_text(&display_text, MAX_PROMPT_BYTES),
                    attachments: attachments.clone(),
                    documents,
                    project_files,
                    delivery: Some(UserMessageDelivery::Submit),
                    branch_provenance,
                },
            },
        }),
        event(EventPayload::SessionStateChanged {
            state: SessionLiveState::Working,
            active_run_id: Some(run_id.clone()),
        }),
    ]);
    if let Some(admission) = admission {
        if admission
            .send(Ok(DriverCommandOutcome::run(run_id.clone(), immediate)))
            .is_err()
        {
            control.abort();
        }
    }
    if plan.config.sandbox.process_execution_allowed() {
        plan.pull_request_discovery_enabled
            .store(true, Ordering::Release);
        plan.pull_request_refresh_requested.notify_one();
    }

    let mut response_text = String::new();
    let mut extension_refresh = tokio::time::interval(std::time::Duration::from_millis(250));
    extension_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let outcome;
    loop {
        tokio::select! {
            _ = extension_refresh.tick() => {
                publish_extension_presentations(
                    &mut app.executable_extensions,
                    projection,
                    events,
                ).await?;
            }
            event = run.next() => {
                let Some(agent_event) = event else {
                    outcome = HostRunOutcome::stream_lost();
                    break;
                };
                let projected_outcome = project_agent_event(
                    agent_event,
                    &run_id,
                    plan,
                    &run_model,
                    projection,
                    &mut context_projection,
                    events,
                    &mut response_text,
                )
                .await?;
                publish_context_snapshot(
                    run.context_snapshot(),
                    &run_id,
                    &mut context_projection,
                    events,
                )
                .await?;
                projection.last_context = context_projection.last_published.clone();
                if let Some(projected_outcome) = projected_outcome {
                    outcome = projected_outcome;
                    break;
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    control.abort();
                    outcome = HostRunOutcome::shutdown();
                    break;
                };
                match command {
                    WorkerMessage::Command(command) => {
                        handle_active_command(
                            command,
                            &run_id,
                            &control,
                            plan,
                            projection,
                            events,
                        )
                        .await;
                    }
                    WorkerMessage::CommandDiscovery { response } => {
                        let _ = response.send(Ok(command_discovery.clone()));
                    }
                }
            }
        }
    }
    let final_context_snapshot = run.into_context_snapshot();
    if let Some(notice) = crate::commands::cache_miss_notice(&app, prior_cache_misses) {
        crate::output::stderr_line(notice);
    }
    app.executable_extensions
        .settle_turn(extension_turn, &outcome)
        .await;
    publish_extension_presentations(&mut app.executable_extensions, projection, events).await?;
    publish_context_snapshot(
        final_context_snapshot,
        &run_id,
        &mut context_projection,
        events,
    )
    .await?;
    projection.last_context = context_projection.last_published.clone();
    let completed = outcome.allows_after_response();
    let terminal = TerminalProjection::from_host_outcome(&outcome);
    if let Err(error) = sync_session_usage(&plan.usage, &plan.session_id, app.agent.session()) {
        projection.usage_uncertain = true;
        publish_idle_accounting_context(projection, events).await?;
        return Err(error);
    }
    let settled_at_ms = now_ms();
    let unfinished = projection
        .tool_calls
        .iter()
        .filter(|(_, tool)| tool.activity.status == ToolActivityStatus::Running)
        .map(|(tool_call_id, _)| tool_call_id.clone())
        .collect::<Vec<_>>();
    let mut stopped_updates = Vec::new();
    for tool_call_id in unfinished {
        let Some(item_id) = projection.tool_items.get(&tool_call_id).cloned() else {
            continue;
        };
        let progress = projection
            .tool_progress
            .remove(&tool_call_id)
            .unwrap_or_default();
        let Some(tool) = projection.tool_calls.get_mut(&tool_call_id) else {
            continue;
        };
        tool.activity.status = ToolActivityStatus::Stopped;
        tool.activity.summary = Some("Stopped".into());
        tool.activity.completed_at_ms = Some(settled_at_ms.max(tool.activity.started_at_ms));
        tool.activity.duration_ms = Some(settled_at_ms.saturating_sub(tool.activity.started_at_ms));
        tool.activity.output_summary = Some("Tool stopped before completion".into());
        tool.activity.observed_output_bytes = progress.observed_output_bytes;
        tool.activity.dropped_output_bytes = progress.dropped_output_bytes;
        tool.result = Some(ToolResultSummary {
            tool_call_item_id: item_id.clone(),
            status: ToolActivityStatus::Stopped,
            summary: "Stopped".into(),
            output_summary: tool.activity.output_summary.clone(),
            output_handle: None,
            exit_code: None,
            signal: None,
            completed_at_ms: tool.activity.completed_at_ms.unwrap_or(settled_at_ms),
            duration_ms: tool.activity.duration_ms.unwrap_or_default(),
            observed_output_bytes: tool.activity.observed_output_bytes,
            dropped_output_bytes: tool.activity.dropped_output_bytes,
        });
        stopped_updates.push((item_id, tool.activity.clone()));
    }
    for (item_id, activity) in stopped_updates {
        events
            .send(event(EventPayload::ItemDelta {
                item_id,
                delta: ItemDelta::ToolActivity { activity },
            }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    let mut changed_file_item_ids = BTreeSet::new();
    let mut source_ids = BTreeSet::new();
    let mut output_ids = BTreeSet::new();
    if let Some(resources) = plan.resources.as_ref() {
        let completed = std::mem::take(&mut projection.pending_tool_evidence);
        for completed in completed {
            let payloads = project_tool_evidence(
                app.agent.session(),
                &plan.config.workspace,
                resources,
                &plan.session_id,
                &run_id,
                &completed.turn_id,
                &completed.tool_call_id,
                &completed.tool_item_id,
                &completed.tool,
                &completed.output,
            );
            let mut changed_paths = BTreeSet::new();
            let mut linked_sources = BTreeSet::new();
            let mut linked_outputs = BTreeSet::new();
            for payload in &payloads {
                match payload {
                    EventPayload::SourceUpserted { source } => {
                        source_ids.insert(source.id.clone());
                        linked_sources.insert(source.id.clone());
                    }
                    EventPayload::ArtifactUpserted { artifact } => {
                        output_ids.insert(artifact.id.clone());
                        linked_outputs.insert(artifact.id.clone());
                    }
                    EventPayload::ItemCommitted { item } => match &item.payload {
                        ItemPayload::FileChange(change) => {
                            changed_file_item_ids.insert(item.id.clone());
                            changed_paths.insert(change.display_path.clone());
                        }
                        ItemPayload::Source(source) => {
                            source_ids.insert(source.id.clone());
                            linked_sources.insert(source.id.clone());
                        }
                        ItemPayload::Artifact(artifact) => {
                            output_ids.insert(artifact.id.clone());
                            linked_outputs.insert(artifact.id.clone());
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            if let Some(tool) = projection.tool_calls.get_mut(&completed.tool_call_id) {
                tool.activity.changed_paths = changed_paths.into_iter().collect();
                tool.activity.source_ids = linked_sources.into_iter().collect();
                tool.activity.artifact_ids = linked_outputs.into_iter().collect();
                events
                    .send(event(EventPayload::ItemDelta {
                        item_id: completed.tool_item_id.clone(),
                        delta: ItemDelta::ToolActivity {
                            activity: tool.activity.clone(),
                        },
                    }))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            }
            for payload in payloads {
                events
                    .send(event(payload))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            }
        }
    } else {
        projection.pending_tool_evidence.clear();
    }
    let review = build_completion_review(
        &terminal,
        projection.run_started_at_ms,
        settled_at_ms,
        projection,
        changed_file_item_ids,
        source_ids,
        output_ids,
    );
    app.agent.set_system_prompt(app.system.clone());
    app.agent
        .record_run_outcome(SessionRunOutcome {
            status: match terminal.outcome {
                octet_serve_backend::RunOutcome::Completed => SessionRunOutcomeStatus::Completed,
                octet_serve_backend::RunOutcome::Stopped => SessionRunOutcomeStatus::Stopped,
                octet_serve_backend::RunOutcome::Failed => SessionRunOutcomeStatus::Failed,
            },
            message: terminal.message.clone(),
        })
        .map_err(|_| ServiceError::Internal)?;

    let branch_start = projection.known_entries;
    let committed = project_new_entries(
        app.agent.session(),
        &plan.config.workspace,
        projection,
        Some(&run_id),
        Some(&review),
        plan.attachments.as_ref(),
        &plan.session_id,
    )?;
    let search_title =
        session_meta_for_open_session(&plan.sessions, &plan.session_id, app.agent.session())
            .map(|meta| meta.name.unwrap_or(meta.title))
            .unwrap_or_else(|| "Session".to_owned());
    if let Ok(mut search_index) = plan.search_index.lock() {
        for item in &committed {
            if let Some(document) =
                search_document_for_item(&plan.session_id, &search_title, settled_at_ms, item)
            {
                let _ = search_index.upsert_document(document);
            }
        }
    }
    if let Some(resources) = plan.resources.as_ref() {
        persist_run_projection(
            resources,
            &plan.session_id,
            &run_id,
            projection.run_started_at_ms,
            settled_at_ms,
            projection,
            &committed,
            &review,
        )?;
    }
    for item in committed {
        events
            .send(event(EventPayload::ItemCommitted { item }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    for pending in projection.pending_user_items.drain(..) {
        events
            .send(event(EventPayload::ItemRetracted {
                item_id: pending.id,
                provider_attempt: 1,
                reason: "Input was not delivered before the run ended.".into(),
            }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    projection.pending_attachments.clear();
    for branch_event in branch_delta_events(app.agent.session(), branch_start)? {
        events
            .send(branch_event)
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    expire_private_requests(projection, events, plan.actor_generation).await?;
    events
        .send(event(EventPayload::SessionStateChanged {
            state: terminal.state,
            active_run_id: None,
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    if completed {
        let _ = app
            .executable_extensions
            .after_response(&response_text)
            .await;
        publish_extension_presentations(&mut app.executable_extensions, projection, events).await?;
    }
    let goal = match goal_driver {
        Some(driver) if completed => match driver.turn_settled(
            goal_source,
            &response_text,
            !projection.tool_calls.is_empty(),
        ) {
            Ok(goal) => Some(goal),
            Err(_) => {
                let _ = driver.session_error();
                None
            }
        },
        Some(driver) => {
            let _ = driver.session_error();
            None
        }
        None => None,
    };
    if goal_driver.is_some() {
        events
            .send(current_goal_event(
                plan.goal_store.as_ref(),
                &plan.session_id,
            )?)
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    Ok(RunDriveOutcome::Admitted { goal })
}

pub(super) async fn handle_active_command(
    message: WorkerCommand,
    run_id: &RunId,
    control: &RunControl,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) {
    let outcome = match message.command {
        SessionCommand::Steer { input } => match resolve_prompt_input(plan, input).await {
            Ok(resolved) => match resolve_control_input(
                plan,
                resolved.model_text.clone(),
                &resolved.attachments,
            ) {
                Ok(input) => match control.steer(input).await {
                    Ok(()) => {
                        publish_control_user_item(
                            run_id,
                            resolved,
                            UserMessageDelivery::Steer,
                            projection,
                            events,
                        )
                        .await
                    }
                    Err(_) => Err(ServiceError::InvalidBoundary),
                },
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        SessionCommand::FollowUp { input } => match resolve_prompt_input(plan, input).await {
            Ok(resolved) => match resolve_control_input(
                plan,
                resolved.model_text.clone(),
                &resolved.attachments,
            ) {
                Ok(input) => match control.follow_up(input).await {
                    Ok(()) => {
                        publish_control_user_item(
                            run_id,
                            resolved,
                            UserMessageDelivery::FollowUp,
                            projection,
                            events,
                        )
                        .await
                    }
                    Err(_) => Err(ServiceError::InvalidBoundary),
                },
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        command @ (SessionCommand::SetGoal { .. }
        | SessionCommand::PauseGoal
        | SessionCommand::ResumeGoal
        | SessionCommand::ClearGoal) => goal_mutation_outcome(plan, command),
        SessionCommand::Rename { title } => rename_session_outcome(plan, &title),
        SessionCommand::SetPinned { pinned } => pin_session_outcome(plan, pinned),
        SessionCommand::SetArchived { archived } => archive_session_outcome(plan, archived),
        SessionCommand::Abort { run_id: expected }
            if expected.as_ref().is_none_or(|expected| expected == run_id) =>
        {
            control.abort();
            Ok(DriverCommandOutcome::default())
        }
        SessionCommand::AnswerRequest { request_id, answer } => {
            match projection.private_requests.remove(&request_id) {
                Some(PrivateRequest {
                    kind,
                    response: PrivateResponse::Approval(respond),
                }) => {
                    let (allowed, state) = match answer {
                        RequestAnswer::Approval { allowed } => (
                            allowed,
                            if allowed {
                                RequestState::Resolved
                            } else {
                                RequestState::Denied
                            },
                        ),
                        _ => {
                            projection.private_requests.insert(
                                request_id,
                                PrivateRequest {
                                    kind,
                                    response: PrivateResponse::Approval(respond),
                                },
                            );
                            let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                            return;
                        }
                    };
                    respond(allowed);
                    let changed = PendingRequest {
                        id: request_id,
                        actor_generation: projection_actor_generation(run_id),
                        kind,
                        state,
                    };
                    let _ = events
                        .send(event(EventPayload::PendingRequestChanged {
                            request: changed,
                        }))
                        .await;
                    let _ = events
                        .send(event(EventPayload::SessionStateChanged {
                            state: SessionLiveState::Working,
                            active_run_id: Some(run_id.clone()),
                        }))
                        .await;
                    Ok(DriverCommandOutcome::default())
                }
                Some(PrivateRequest {
                    kind,
                    response: PrivateResponse::Input(respond),
                }) => {
                    let answer = match answer {
                        RequestAnswer::Text { text } => text,
                        RequestAnswer::Choice { choice } => choice,
                        _ => {
                            projection.private_requests.insert(
                                request_id,
                                PrivateRequest {
                                    kind,
                                    response: PrivateResponse::Input(respond),
                                },
                            );
                            let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                            return;
                        }
                    };
                    respond(Some(answer.into_bytes()));
                    let changed = PendingRequest {
                        id: request_id,
                        actor_generation: projection_actor_generation(run_id),
                        kind,
                        state: RequestState::Resolved,
                    };
                    let _ = events
                        .send(event(EventPayload::PendingRequestChanged {
                            request: changed,
                        }))
                        .await;
                    let _ = events
                        .send(event(EventPayload::SessionStateChanged {
                            state: SessionLiveState::Working,
                            active_run_id: Some(run_id.clone()),
                        }))
                        .await;
                    Ok(DriverCommandOutcome::default())
                }
                None => Err(ServiceError::InvalidBoundary),
            }
        }
        _ => Err(ServiceError::InvalidBoundary),
    };
    let _ = message.response.send(outcome);
}

pub(super) async fn publish_control_user_item(
    run_id: &RunId,
    resolved: ResolvedPromptInput,
    delivery: UserMessageDelivery,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<DriverCommandOutcome, ServiceError> {
    let ResolvedPromptInput {
        display_text,
        model_text: _,
        attachments,
        documents,
        project_files,
        document_context_tokens,
        project_file_context_tokens,
    } = resolved;
    let item_id = projection.next_user_item_id(run_id)?;
    let turn_id = projection.turn_id(run_id)?;
    projection.pending_user_items.push_back(PendingUserItem {
        id: item_id.clone(),
        delivery,
        turn_id: turn_id.clone(),
        documents: documents.clone(),
        project_files: project_files.clone(),
        document_context_tokens,
        project_file_context_tokens,
        context_attributed: false,
        branch_provenance: None,
    });
    projection
        .item_turns
        .insert(item_id.clone(), turn_id.clone());
    if !attachments.is_empty() {
        projection
            .pending_attachments
            .push_back(attachments.clone());
    }
    events
        .send(event(EventPayload::ItemStarted {
            item: SessionItem {
                id: item_id,
                run_id: Some(run_id.clone()),
                turn_id: Some(turn_id),
                provider_attempt: None,
                lifecycle: ItemLifecycle::Provisional,
                durable_entry_id: None,
                payload: ItemPayload::UserMessage {
                    text: bounded_text(&display_text, MAX_PROMPT_BYTES),
                    attachments,
                    documents,
                    project_files,
                    delivery: Some(delivery),
                    branch_provenance: None,
                },
            },
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    Ok(DriverCommandOutcome::default())
}
