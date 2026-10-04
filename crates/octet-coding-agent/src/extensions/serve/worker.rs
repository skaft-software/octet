//! The per-session worker app and its command discovery.

use super::*;

pub(super) async fn shutdown_worker_app(app: &mut Option<Box<App>>) {
    if let Some(mut app) = app.take() {
        app.executable_extensions.shutdown().await;
    }
}

pub(super) fn serve_runtime_manager(plan: &WorkerPlan) -> anyhow::Result<ExtensionRuntimeManager> {
    // Serve never reuses the ordinary-host partition. Hashing both stable
    // project identity and the finite authority profile provides an explicit,
    // path-free trust partition while the runtime domain independently binds
    // the canonical workspace.
    let project = plan
        .project_id
        .as_ref()
        .map(|project| project.as_str())
        .unwrap_or("unbound");
    let project_digest = format!("{:x}", Sha256::digest(project.as_bytes()));
    let authority = format!("{:?}", plan.authority);
    let authority_digest = format!("{:x}", Sha256::digest(authority.as_bytes()));
    let trust = ExtensionTrustDomain::new(format!(
        "serve-{}-{}",
        &project_digest[..32],
        &authority_digest[..32]
    ))
    .map_err(anyhow::Error::msg)?;
    let domain =
        ExtensionRuntimeDomain::serve(&plan.config.workspace, trust).map_err(anyhow::Error::msg)?;
    Ok(ExtensionRuntimeManager::new(domain))
}

pub(super) fn build_worker_app(plan: &mut WorkerPlan) -> anyhow::Result<Box<App>> {
    anyhow::ensure!(
        plan.authority == authority_ceiling_from_sandbox(&plan.config.sandbox),
        "Serve session authority must match the immutable host policy"
    );
    let mut config = plan.config.clone();
    config.resume = match &plan.launch.session {
        SessionSelection::CreateNew(_) => crate::config::ResumeSelector::New,
        SessionSelection::OpenExisting(path) => crate::config::ResumeSelector::Resume(
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned),
        ),
    };
    let mut boot = crate::app::bootstrap::bootstrap(config)?;
    let system = compose_instructions(&boot.config)?;
    if let Some(session) = plan
        .prepared_session
        .get_mut()
        .map_err(|_| anyhow::anyhow!("prepared session lock poisoned"))?
        .take()
    {
        boot.set_prepared_session(session);
    }
    let app = build_app_with_runtime_manager(
        boot,
        plan.launch.clone(),
        system,
        Some(serve_runtime_manager(plan)?),
    )?;
    Ok(Box::new(app))
}

pub(super) fn command_name_is_claimed_by_builtin(name: &str) -> bool {
    !matches!(
        commands::parse(&format!("/{name}")),
        commands::Command::Unknown(_)
    )
}

pub(super) fn extension_command_presentation(
    name: &str,
    declared_usage: Option<String>,
) -> (String, Option<String>) {
    let default_usage = format!("/{name}");
    let Some(declared_usage) = declared_usage else {
        return (default_usage, None);
    };
    let usage = octet_serve_backend::sanitize_public_text(declared_usage.trim(), 512, false);
    let Some(suffix) = usage.strip_prefix(&default_usage) else {
        return (default_usage, None);
    };
    if !suffix.is_empty()
        && !matches!(suffix.chars().next(), Some(character) if character.is_whitespace())
    {
        return (default_usage, None);
    }
    let argument_hint = suffix.trim();
    let argument_hint = (!argument_hint.is_empty()).then(|| argument_hint.to_owned());
    (usage, argument_hint)
}

pub(super) fn build_command_discovery(app: &App) -> Result<CommandDiscovery, ServiceError> {
    const MAX_SUGGESTIONS: usize = 512;

    let mut commands = Vec::new();
    let mut command_names = BTreeSet::new();
    let mut push_command = |suggestion: CommandSuggestion| {
        if commands.len() >= MAX_SUGGESTIONS || !command_names.insert(suggestion.name.clone()) {
            return;
        }
        if suggestion.validate().is_ok() {
            commands.push(suggestion);
        } else {
            command_names.remove(&suggestion.name);
        }
    };

    for command in commands::slash_commands() {
        push_command(CommandSuggestion {
            name: command.name.to_owned(),
            usage: command.usage.to_owned(),
            description: command.description.to_owned(),
            argument_hint: None,
            accepts_argument: command.accepts_argument,
            kind: CommandSuggestionKind::BuiltIn,
        });
    }
    let extension_commands = app.executable_extensions.command_suggestions_with_usage();
    let extension_command_names = extension_commands
        .iter()
        .map(|(name, _, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    for template in app.prompts.descriptors().iter() {
        // `commands::parse` accepts unambiguous built-in prefixes. A dynamic
        // name claimed that way would execute the built-in instead.
        if command_name_is_claimed_by_builtin(&template.name) {
            continue;
        }
        // Dynamic dispatch gives executable extensions precedence over prompt
        // templates. Do not advertise a colliding template that would invoke
        // an extension instead.
        if extension_command_names.contains(template.name.as_str()) {
            continue;
        }
        push_command(CommandSuggestion {
            name: template.name.clone(),
            usage: format!("/{}", template.name),
            description: format!("prompt · {}", template.description),
            argument_hint: template.argument_hint.clone(),
            accepts_argument: true,
            kind: CommandSuggestionKind::Prompt,
        });
    }
    for (name, description, declared_usage) in extension_commands {
        if command_name_is_claimed_by_builtin(&name) {
            continue;
        }
        let (usage, argument_hint) = extension_command_presentation(&name, declared_usage);
        push_command(CommandSuggestion {
            usage,
            name,
            description: format!("extension · {description}"),
            argument_hint,
            accepts_argument: true,
            kind: CommandSuggestionKind::Extension,
        });
    }

    let active_skill_ids = app
        .agent
        .session()
        .head_ref()
        .and_then(|head| app.agent.session().resolve_active_skills(head).ok())
        .map(|state| {
            state
                .active_skills
                .into_iter()
                .map(|skill| skill.descriptor.id)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut skill_ids = BTreeSet::new();
    let mut skills = Vec::new();
    for descriptor in app.skills.descriptors().iter() {
        if skills.len() >= MAX_SUGGESTIONS || !skill_ids.insert(descriptor.id.clone()) {
            continue;
        }
        let suggestion = SkillSuggestion {
            id: descriptor.id.clone(),
            name: descriptor.name.clone(),
            description: descriptor.description.clone(),
            active: active_skill_ids.contains(&descriptor.id),
        };
        if suggestion.validate().is_ok() {
            skills.push(suggestion);
        } else {
            skill_ids.remove(&descriptor.id);
        }
    }

    let mut discovery = CommandDiscovery {
        protocol: PROTOCOL_VERSION,
        commands,
        skills,
    };
    trim_command_discovery_to_transport_bounds(&mut discovery);
    discovery.validate().map_err(|_| ServiceError::Internal)?;
    Ok(discovery)
}

pub(super) fn trim_command_discovery_to_transport_bounds(discovery: &mut CommandDiscovery) {
    while discovery.validate().is_err() {
        if discovery.skills.pop().is_some() || discovery.commands.pop().is_some() {
            continue;
        }
        break;
    }
}

// Move large App values only at these synchronous ownership boundaries, not in
// the long-lived worker poll frame that also polls the agent/provider stream.
pub(super) fn reconfigure_worker_app(
    app: Box<App>,
    reconfig: Reconfig,
) -> anyhow::Result<Box<App>> {
    crate::app::apply_reconfig(*app, reconfig).map(Box::new)
}

pub(super) fn rebuild_worker_app(
    app: Box<App>,
    new_model: Option<octet_ai::Model>,
    new_reasoning: Option<ReasoningConfig>,
    new_reasoning_mode: Option<octet_ai::ReasoningMode>,
    selection: Option<SessionSelection>,
) -> anyhow::Result<Box<App>> {
    rebuild_app(
        *app,
        new_model,
        new_reasoning,
        new_reasoning_mode,
        selection,
    )
    .map(Box::new)
}
