//! Slash commands and session mutations handled while no run is active.

use super::*;

pub(super) enum SlashInvocationOutcome {
    Start(RunPromptInput),
    Immediate(Box<DriverCommandOutcome>),
}

impl SlashInvocationOutcome {
    pub(super) fn immediate(outcome: DriverCommandOutcome) -> Self {
        Self::Immediate(Box::new(outcome))
    }
}

pub(super) fn self_help_prompt(topic: Option<&str>) -> String {
    let subject = topic
        .map(|topic| format!("the octet command or topic `{topic}`"))
        .unwrap_or_else(|| "octet's commands and workflow".to_owned());
    format!(
        "Give a concise self-help answer about {subject}. If this workspace is a octet source checkout, consult its README.md, docs/, examples/, and relevant Rust crates with the available tools before answering. Include practical details and mention how a user can inspect or extend octet when relevant."
    )
}

/// Executes one slash invocation at an idle worker boundary. The command is
/// parsed from the same grammar as the TUI, but only durable/session-safe
/// outcomes cross the graphical protocol boundary.
pub(super) async fn invoke_idle_slash_command(
    app: App,
    invocation: SlashCommandInvocation,
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
) -> (Option<App>, Result<SlashInvocationOutcome, ServiceError>) {
    let parsed = commands::parse(&invocation.invocation);
    match parsed {
        commands::Command::Changelog => (Some(app), Err(ServiceError::InvalidBoundary)),
        commands::Command::Help(topic) => (
            Some(app),
            Ok(SlashInvocationOutcome::Start(RunPromptInput::New(
                PromptInput {
                    text: self_help_prompt(topic.as_deref()),
                    attachments: Vec::new(),
                    document_ids: Vec::new(),
                    project_file_ids: Vec::new(),
                },
            ))),
        ),
        commands::Command::Compact => {
            let mut app = app;
            let original_keep_recent_tokens = app.config.compaction.keep_recent_tokens;
            app.config.compaction.keep_recent_tokens = 1;
            let result = attempt_compaction(&mut app).await;
            app.config.compaction.keep_recent_tokens = original_keep_recent_tokens;
            // Failed/cancelled compaction may still have provider accounting.
            // Reconcile it even when no conversation entries were appended.
            let outcome = finish_idle_compaction(&app, plan, projection, result.is_ok());
            (Some(app), outcome)
        }
        commands::Command::Model(Some(model)) => {
            let supported = plan
                .available_models
                .iter()
                .any(|summary| summary.id == model && summary.available);
            if !supported {
                return (Some(app), Err(ServiceError::InvalidBoundary));
            }
            apply_slash_reconfiguration(app, Reconfig::Model(ModelId(model)), plan, projection)
        }
        commands::Command::Thinking(Some(reasoning)) => {
            let level = match config::ThinkingLevel::parse(&reasoning) {
                Ok(level) => level,
                Err(_) => return (Some(app), Err(ServiceError::InvalidBoundary)),
            };
            let reasoning = match crate::app::thinking_to_reasoning_with_subagents(
                level,
                &app.model,
                app.subagents_available(),
            ) {
                Ok(reasoning) => reasoning,
                Err(_) => return (Some(app), Err(ServiceError::InvalidBoundary)),
            };
            apply_slash_reconfiguration(app, Reconfig::Thinking(reasoning), plan, projection)
        }
        commands::Command::Reload
        | commands::Command::Extensions(commands::ExtensionsSubcommand::Reload)
        | commands::Command::Skills(commands::SkillsSubcommand::Reload) => {
            reload_slash_resources(app, plan, projection)
        }
        commands::Command::Skills(subcommand) => {
            let mut app = app;
            let outcome = execute_slash_skills_command(&mut app, subcommand, plan, projection)
                .map(SlashInvocationOutcome::immediate);
            (Some(app), outcome)
        }
        commands::Command::Prompt(Some(invocation)) => {
            let mut app = app;
            let outcome = match slash_name_and_arguments(&invocation) {
                Some((name, arguments)) => start_prompt_template(&mut app, name, arguments)
                    .map(SlashInvocationOutcome::Start),
                None => Err(ServiceError::InvalidBoundary),
            };
            (Some(app), outcome)
        }
        commands::Command::Unknown(invocation) => {
            let mut app = app;
            let outcome = invoke_dynamic_slash_command(&mut app, &invocation).await;
            (Some(app), outcome)
        }
        commands::Command::Name(Some(title)) => (
            Some(app),
            rename_session_outcome(plan, &title).map(SlashInvocationOutcome::immediate),
        ),
        commands::Command::Name(None)
        | commands::Command::Prompt(None)
        | commands::Command::Extensions(
            commands::ExtensionsSubcommand::Menu | commands::ExtensionsSubcommand::Status,
        ) => (
            Some(app),
            Ok(SlashInvocationOutcome::immediate(
                DriverCommandOutcome::default(),
            )),
        ),
        _ => (Some(app), Err(ServiceError::InvalidBoundary)),
    }
}

pub(super) fn apply_slash_reconfiguration(
    app: App,
    reconfig: Reconfig,
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
) -> (Option<App>, Result<SlashInvocationOutcome, ServiceError>) {
    match crate::app::apply_reconfig(app, reconfig) {
        Ok(rebuilt) => {
            plan.launch.model = rebuilt.model.spec.id.clone();
            plan.launch.reasoning = rebuilt.reasoning.clone();
            plan.launch.session =
                SessionSelection::OpenExisting(rebuilt.agent.session().path().to_owned());
            let selection = selection_for_model(&rebuilt.model, &rebuilt.reasoning, &plan.config);
            let outcome =
                reconfiguration_outcome(&rebuilt, plan, projection, selection, plan.authority)
                    .map(SlashInvocationOutcome::immediate);
            (Some(rebuilt), outcome)
        }
        Err(_) => (build_worker_app(plan).ok(), Err(ServiceError::Internal)),
    }
}

pub(super) fn reload_slash_resources(
    app: App,
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
) -> (Option<App>, Result<SlashInvocationOutcome, ServiceError>) {
    let mut app = app;
    let system = match compose_instructions(&app.config) {
        Ok(system) => system,
        Err(_) => return (Some(app), Err(ServiceError::Internal)),
    };
    app.system_tokens = crate::compaction::estimate_text_tokens(&system);
    app.system = system;
    match rebuild_app(app, None, None, None, None) {
        Ok(rebuilt) => {
            plan.launch.model = rebuilt.model.spec.id.clone();
            plan.launch.reasoning = rebuilt.reasoning.clone();
            plan.launch.session =
                SessionSelection::OpenExisting(rebuilt.agent.session().path().to_owned());
            let outcome = idle_mutation_outcome(&rebuilt, plan, projection)
                .map(SlashInvocationOutcome::immediate);
            (Some(rebuilt), outcome)
        }
        Err(_) => (build_worker_app(plan).ok(), Err(ServiceError::Internal)),
    }
}

pub(super) fn execute_slash_skills_command(
    app: &mut App,
    subcommand: commands::SkillsSubcommand,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
) -> Result<DriverCommandOutcome, ServiceError> {
    match subcommand {
        commands::SkillsSubcommand::Load(id) => {
            let loaded = app
                .skills
                .load(&id)
                .map_err(|_| ServiceError::InvalidBoundary)?;
            validate_skill_requirements(&loaded.descriptor, &app.agent.registered_tool_names())
                .map_err(|_| ServiceError::InvalidBoundary)?;
            app.agent
                .session_mut()
                .append(EntryValue::SkillActivated {
                    descriptor: loaded.descriptor,
                    instructions_hash: loaded.content_hash,
                    instructions: loaded.instructions,
                })
                .map_err(|_| ServiceError::Internal)?;
            idle_mutation_outcome(app, plan, projection)
        }
        commands::SkillsSubcommand::Off(id) => {
            let activation_id = app
                .agent
                .session()
                .head_ref()
                .and_then(|head| app.agent.session().resolve_active_skills(head).ok())
                .and_then(|state| {
                    state
                        .active_skills
                        .into_iter()
                        .find(|skill| skill.descriptor.id == id)
                        .map(|skill| skill.activation_id)
                })
                .ok_or(ServiceError::InvalidBoundary)?;
            app.agent
                .session_mut()
                .append(EntryValue::SkillDeactivated {
                    activation_id,
                    skill_id: id,
                })
                .map_err(|_| ServiceError::Internal)?;
            idle_mutation_outcome(app, plan, projection)
        }
        commands::SkillsSubcommand::List
        | commands::SkillsSubcommand::Show(_)
        | commands::SkillsSubcommand::Active
        | commands::SkillsSubcommand::Search(_) => Ok(DriverCommandOutcome::default()),
        commands::SkillsSubcommand::Reload => Err(ServiceError::InvalidBoundary),
    }
}

pub(super) fn finish_idle_compaction(
    app: &App,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
    succeeded: bool,
) -> Result<SlashInvocationOutcome, ServiceError> {
    projection.usage_uncertain |= app.agent.session().has_uncertain_usage();
    if let Err(error) = sync_session_usage(&plan.usage, &plan.session_id, app.agent.session()) {
        projection.usage_uncertain = true;
        return Err(error);
    }
    if !succeeded {
        return Err(ServiceError::Internal);
    }
    idle_mutation_outcome(app, plan, projection).map(SlashInvocationOutcome::immediate)
}

pub(super) async fn publish_idle_accounting_context(
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    if !projection.usage_uncertain
        || projection
            .last_context
            .as_ref()
            .is_some_and(|context| context.usage_uncertain)
    {
        return Ok(());
    }
    let mut context = projection.last_context.clone().unwrap_or_default();
    context.usage_uncertain = true;
    events
        .send(event(EventPayload::ContextUpdated {
            context: context.clone(),
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    projection.last_context = Some(context);
    Ok(())
}

pub(super) fn idle_mutation_outcome(
    app: &App,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
) -> Result<DriverCommandOutcome, ServiceError> {
    let branch_start = projection.known_entries;
    let items = project_new_entries(
        app.agent.session(),
        &plan.config.workspace,
        projection,
        None,
        None,
        plan.attachments.as_ref(),
        &plan.session_id,
    )?;
    if projection.known_entries == branch_start {
        return Ok(DriverCommandOutcome::default());
    }
    let mut events = items
        .into_iter()
        .map(|item| event(EventPayload::ItemCommitted { item }))
        .collect::<Vec<_>>();
    events.extend(branch_delta_events(app.agent.session(), branch_start)?);
    Ok(DriverCommandOutcome::with_events(events))
}

pub(super) fn slash_name_and_arguments(invocation: &str) -> Option<(&str, &str)> {
    let invocation = invocation.trim().trim_start_matches('/');
    let end = invocation
        .find(char::is_whitespace)
        .unwrap_or(invocation.len());
    let name = &invocation[..end];
    (!name.is_empty()).then(|| (name, invocation[end..].trim_start()))
}

pub(super) fn start_prompt_template(
    app: &mut App,
    name: &str,
    arguments: &str,
) -> Result<RunPromptInput, ServiceError> {
    if !app.prompts.contains(name) {
        return Err(ServiceError::InvalidBoundary);
    }
    let prompts = app.prompts.clone();
    let workspace = app.config.workspace.clone();
    let rendered = crate::prompts::render_and_record(
        &prompts,
        app.agent.session_mut(),
        &workspace,
        name,
        arguments,
        None,
    )
    .map_err(|_| ServiceError::InvalidBoundary)?;
    if rendered.text.len() > MAX_PROMPT_BYTES {
        return Err(ServiceError::InvalidBoundary);
    }
    Ok(RunPromptInput::New(PromptInput {
        text: rendered.text,
        attachments: Vec::new(),
        document_ids: Vec::new(),
        project_file_ids: Vec::new(),
    }))
}

pub(super) async fn invoke_dynamic_slash_command(
    app: &mut App,
    invocation: &str,
) -> Result<SlashInvocationOutcome, ServiceError> {
    let (name, arguments) =
        slash_name_and_arguments(invocation).ok_or(ServiceError::InvalidBoundary)?;
    let extension_arguments = arguments
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match app
        .executable_extensions
        .execute_command_without_confirmation(name, extension_arguments)
        .await
    {
        Ok(Some(_)) => Ok(SlashInvocationOutcome::immediate(
            DriverCommandOutcome::default(),
        )),
        Ok(None) => start_prompt_template(app, name, arguments).map(SlashInvocationOutcome::Start),
        Err(_) => Err(ServiceError::InvalidBoundary),
    }
}

pub(super) fn reconfiguration_outcome(
    app: &App,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
    selection: ModelSelection,
    authority: AuthorityProfile,
) -> Result<DriverCommandOutcome, ServiceError> {
    let mut events = vec![event(EventPayload::SessionSettingsChanged {
        model: selection,
        authority,
    })];
    let branch_start = projection.known_entries;
    for item in project_new_entries(
        app.agent.session(),
        &plan.config.workspace,
        projection,
        None,
        None,
        plan.attachments.as_ref(),
        &plan.session_id,
    )? {
        events.push(event(EventPayload::ItemCommitted { item }));
    }
    events.extend(branch_delta_events(app.agent.session(), branch_start)?);
    Ok(DriverCommandOutcome::with_events(events))
}

pub(super) fn persist_idle_selection(
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
    selection: ModelSelection,
) -> Result<DriverCommandOutcome, ServiceError> {
    let branch_start = projection.known_entries;
    let (path, session, newly_created) = match &plan.launch.session {
        SessionSelection::CreateNew(path) => (
            path.clone(),
            Session::create(path).map_err(|_| ServiceError::Internal)?,
            true,
        ),
        SessionSelection::OpenExisting(path) => (
            path.clone(),
            Session::open(path).map_err(|_| ServiceError::Internal)?,
            false,
        ),
    };
    let mut session = session;
    let append = session.append(EntryValue::Config {
        model: Some(plan.launch.model.0.clone()),
        reasoning: Some(reasoning_label(&plan.launch.reasoning)),
        reasoning_mode: Some(
            match plan.launch.reasoning_mode {
                octet_ai::ReasoningMode::Standard => "standard",
                octet_ai::ReasoningMode::Pro => "pro",
            }
            .to_owned(),
        ),
    });
    if append.is_err() {
        drop(session);
        if newly_created {
            let _ = std::fs::remove_file(&path);
        }
        return Err(ServiceError::Internal);
    }
    projection.known_entries = session.entries().len();
    plan.launch.session = SessionSelection::OpenExisting(path);
    let mut events = vec![event(EventPayload::SessionSettingsChanged {
        model: selection,
        authority: plan.authority,
    })];
    events.extend(branch_delta_events(&session, branch_start)?);
    Ok(DriverCommandOutcome::with_events(events))
}

pub(super) fn rename_session_outcome(
    plan: &WorkerPlan,
    title: &str,
) -> Result<DriverCommandOutcome, ServiceError> {
    ensure_durable_session(plan)?;
    let metadata = plan
        .sessions
        .rename(plan.session_id.as_str(), title)
        .map_err(|_| ServiceError::InvalidBoundary)?;
    let title = metadata.name.ok_or(ServiceError::InvalidBoundary)?;
    if let Ok(mut search_index) = plan.search_index.lock() {
        let _ = search_index.update_session_title(plan.session_id.as_str(), &title);
    }
    Ok(session_metadata_outcome(Some(title), None, None))
}

pub(super) fn pin_session_outcome(
    plan: &WorkerPlan,
    pinned: bool,
) -> Result<DriverCommandOutcome, ServiceError> {
    ensure_durable_session(plan)?;
    plan.sessions
        .set_pinned(plan.session_id.as_str(), pinned)
        .map_err(|_| ServiceError::Internal)?;
    Ok(session_metadata_outcome(None, Some(pinned), None))
}

pub(super) fn archive_session_outcome(
    plan: &WorkerPlan,
    archived: bool,
) -> Result<DriverCommandOutcome, ServiceError> {
    ensure_durable_session(plan)?;
    plan.sessions
        .set_archived(plan.session_id.as_str(), archived)
        .map_err(|_| ServiceError::Internal)?;
    Ok(session_metadata_outcome(None, None, Some(archived)))
}
