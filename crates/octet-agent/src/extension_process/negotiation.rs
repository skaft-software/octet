//! Negotiating an extension's contributions with the host's offered services.

use super::*;

pub(super) fn negotiate_api_v03_contributions(
    manifest: &ExtensionManifest,
    offer: &api_v03::ContractOffer,
    response: api_v03::InitializeResponse,
    provider_registry_available: bool,
) -> Result<
    (
        ExtensionContributions,
        ExtensionNegotiatedProtocol,
        api_v03::NegotiatedContract,
    ),
    ExtensionRuntimeError,
> {
    if manifest.api_version != EXTENSION_API_VERSION_0_3 {
        return Err(ExtensionRuntimeError::UnsupportedApiVersion {
            extension: response.api_version,
            host: manifest.api_version.clone(),
        });
    }
    api_v03::validate_initialize_response(&response).map_err(api_v03_protocol_error)?;
    let contract = api_v03::negotiate(offer, &response.contract).map_err(api_v03_protocol_error)?;
    let provider_capabilities = ["provider_catalog", "provider_stream", "provider_auth"];
    let provider_methods = [
        methods::PROVIDERS_COMPLETE,
        methods::PROVIDERS_REGISTER,
        methods::PROVIDERS_UPDATE,
        methods::PROVIDERS_UNREGISTER,
        methods::PROVIDER_STREAM,
        methods::PROVIDER_EVENT,
        methods::PROVIDER_CANCEL,
        methods::PROVIDER_AUTH_REQUEST,
        methods::PROVIDER_AUTH_REVOKE,
    ];
    let selects_provider = contract
        .capabilities
        .iter()
        .any(|capability| provider_capabilities.contains(&capability.as_str()))
        || contract
            .methods
            .iter()
            .any(|method| provider_methods.contains(&method.as_str()));
    if selects_provider && !manifest.contributes.providers {
        return Err(ExtensionRuntimeError::Protocol(
            "API 0.3 provider methods require contributes.providers = true".into(),
        ));
    }
    if selects_provider && !provider_registry_available {
        return Err(ExtensionRuntimeError::Protocol(
            "API 0.3 provider methods are unavailable because no host provider registry is configured"
                .into(),
        ));
    }
    let selects_provider_stream = contract.methods.contains(methods::PROVIDER_STREAM)
        || contract.methods.contains(methods::PROVIDER_EVENT)
        || contract.methods.contains(methods::PROVIDER_CANCEL);
    if selects_provider_stream && !contract.capabilities.contains("provider_catalog") {
        return Err(ExtensionRuntimeError::Protocol(
            "API 0.3 provider streaming requires the provider_catalog capability".into(),
        ));
    }
    if selects_provider_stream
        && ![
            methods::PROVIDER_STREAM,
            methods::PROVIDER_EVENT,
            methods::PROVIDER_CANCEL,
        ]
        .iter()
        .all(|method| contract.methods.contains(*method))
    {
        return Err(ExtensionRuntimeError::Protocol(
            "API 0.3 provider streaming requires stream, event, and cancellation methods".into(),
        ));
    }
    if contract.methods.contains(methods::PROVIDER_AUTH_REQUEST)
        && !contract.capabilities.contains("provider_catalog")
    {
        return Err(ExtensionRuntimeError::Protocol(
            "API 0.3 provider authorization requires the provider_catalog capability".into(),
        ));
    }
    let declares_session_hooks = manifest
        .contributes
        .hooks
        .contains(&ExtensionHook::SessionStart)
        && manifest
            .contributes
            .hooks
            .contains(&ExtensionHook::SessionEnd);
    let selected_lifecycle_events = contract
        .capabilities
        .contains(EXTENSION_FEATURE_LIFECYCLE_EVENTS);
    let selected_hook_run = contract.methods.contains(methods::HOOK_RUN);
    if declares_session_hooks != selected_lifecycle_events
        || declares_session_hooks != selected_hook_run
    {
        return Err(ExtensionRuntimeError::Protocol(
            "API 0.3 declared session hooks and lifecycle_events/hook/run negotiation must agree"
                .into(),
        ));
    }
    if !manifest.contributes.commands.is_empty()
        || manifest
            .contributes
            .hooks
            .iter()
            .any(|hook| !hook.is_session_hook())
        || !manifest.contributes.ui.is_empty()
        || manifest.contributes.context
        || !manifest.contributes.tool_renderers.is_empty()
        || manifest.contributes.notifications
        || manifest.contributes.confirmations
        || manifest.contributes.presentation
        || manifest.contributes.menu
        || !manifest.contributes.shortcuts.is_empty()
    {
        return Err(ExtensionRuntimeError::Protocol(
            "API 0.3 received deferred manifest contributions after validation".into(),
        ));
    }
    let tools = response
        .tools
        .into_iter()
        .map(|tool| ToolDefinition {
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
            output_schema: tool.output_schema,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
            operation: None,
            composition: None,
            constrained_sampling: None,
        })
        .collect::<Vec<_>>();
    let protocol = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_3.to_owned(),
        features: contract.capabilities.clone(),
        max_concurrent_requests: contract.limits.max_concurrent_requests,
        lifecycle_events: BTreeSet::new(),
    };

    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    ensure_same_contributions("tools", &manifest.contributes.tools, &tool_names)?;
    validate_tool_definitions(&tools, &manifest.api_version)?;

    Ok((
        ExtensionContributions {
            tools,
            providers: manifest.contributes.providers
                && contract.capabilities.contains("provider_catalog"),
            hooks: manifest.contributes.hooks.clone(),
            ..ExtensionContributions::default()
        },
        protocol,
        contract,
    ))
}

pub(super) fn negotiate_contributions_with_host_services(
    manifest: &ExtensionManifest,
    response: InitializeResponse,
    host_max_concurrent_requests: usize,
    offered_host_services: OfferedHostServices,
) -> Result<(ExtensionContributions, ExtensionNegotiatedProtocol), ExtensionRuntimeError> {
    if response.api_version != manifest.api_version {
        return Err(ExtensionRuntimeError::UnsupportedApiVersion {
            extension: response.api_version,
            host: manifest.api_version.clone(),
        });
    }

    let protocol = match manifest.api_version.as_str() {
        EXTENSION_API_VERSION_0_1 => {
            if response.protocol.is_some() {
                return Err(ExtensionRuntimeError::Protocol(
                    "API 0.1 initialize response must not include protocol negotiation".into(),
                ));
            }
            ExtensionNegotiatedProtocol::api_0_1(host_max_concurrent_requests)
        }
        EXTENSION_API_VERSION_0_2 | EXTENSION_API_VERSION_0_4 => {
            let negotiated = response.protocol.clone().ok_or_else(|| {
                ExtensionRuntimeError::Protocol(format!(
                    "API {} initialize response requires protocol negotiation",
                    manifest.api_version
                ))
            })?;
            if negotiated.version != manifest.api_version {
                return Err(ExtensionRuntimeError::UnsupportedApiVersion {
                    extension: negotiated.version,
                    host: manifest.api_version.clone(),
                });
            }
            if negotiated.limits.max_concurrent_requests == 0 {
                return Err(ExtensionRuntimeError::Protocol(
                    "negotiated max_concurrent_requests must be greater than zero".into(),
                ));
            }
            let features = negotiated.features.into_iter().collect::<BTreeSet<_>>();
            if features.len()
                != response
                    .protocol
                    .as_ref()
                    .map_or(0, |protocol| protocol.features.len())
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "negotiated features contain duplicates".into(),
                ));
            }
            let mut allowed = API_0_2_REQUIRED_FEATURES
                .iter()
                .chain(API_0_2_OPTIONAL_FEATURES)
                .copied()
                .collect::<BTreeSet<_>>();
            if manifest.api_version == EXTENSION_API_VERSION_0_4 {
                allowed.insert(EXTENSION_FEATURE_RESOURCE_REFS_V1);
                allowed.insert(EXTENSION_FEATURE_OPERATION_DESCRIPTORS_V1);
                allowed.insert(EXTENSION_FEATURE_TOOL_PROMPT_METADATA);
                allowed.insert(EXTENSION_FEATURE_AUTOCOMPLETE_EDIT_V1);
            }
            if negotiated.limits.resource_refs_v1.is_some_and(|limits| {
                manifest.api_version != EXTENSION_API_VERSION_0_4
                    || limits != ResourceProtocolLimits::default()
            }) {
                return Err(ExtensionRuntimeError::Protocol(
                    "unsupported resource registry limits".into(),
                ));
            }
            if manifest.api_version == EXTENSION_API_VERSION_0_4
                && manifest
                    .contributes
                    .hooks
                    .contains(&ExtensionHook::CompactionStrategy)
            {
                allowed.insert(EXTENSION_FEATURE_COMPACTION_STRATEGY);
            }
            if manifest.api_version == EXTENSION_API_VERSION_0_4
                && manifest
                    .contributes
                    .hooks
                    .contains(&ExtensionHook::CacheWarmingDecision)
            {
                allowed.insert(EXTENSION_FEATURE_CACHE_WARMING_DECISION);
            }
            if manifest
                .contributes
                .hooks
                .iter()
                .any(|hook| hook.is_session_operation())
                && !features.contains(EXTENSION_FEATURE_SESSION_ENTRIES)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "session operation hooks require negotiated session_entries".into(),
                ));
            }
            if manifest.api_version == EXTENSION_API_VERSION_0_4
                && offered_host_services.provider_pipeline
                && manifest
                    .contributes
                    .hooks
                    .iter()
                    .any(|hook| hook.is_provider_pipeline())
            {
                allowed.insert(EXTENSION_FEATURE_PIPELINE_HOOKS_V1);
            }
            if manifest
                .contributes
                .hooks
                .iter()
                .any(|hook| hook.is_provider_pipeline())
                && !features.contains(EXTENSION_FEATURE_PIPELINE_HOOKS_V1)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "provider pipeline hooks require a negotiated pipeline_hooks_v1 consumer"
                        .into(),
                ));
            }
            if manifest.api_version == EXTENSION_API_VERSION_0_4
                && offered_host_services.session_lifecycle
            {
                allowed.insert(EXTENSION_FEATURE_SESSION_CONTROL_V1);
            }
            if offered_host_services.resource_paths
                && manifest.api_version == EXTENSION_API_VERSION_0_4
                && manifest
                    .contributes
                    .hooks
                    .contains(&ExtensionHook::ResourcesDiscover)
            {
                allowed.insert(EXTENSION_FEATURE_RESOURCE_PATHS);
            }
            if manifest
                .contributes
                .hooks
                .contains(&ExtensionHook::ResourcesDiscover)
                && !features.contains(EXTENSION_FEATURE_RESOURCE_PATHS)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "resources_discover requires a negotiated resource_paths_v1 consumer".into(),
                ));
            }
            if offered_host_services.bulk_objects
                && manifest.api_version == EXTENSION_API_VERSION_0_4
            {
                allowed.insert(EXTENSION_FEATURE_BULK_OBJECTS_V1);
            }
            if offered_host_services.tool_composition
                && manifest.api_version == EXTENSION_API_VERSION_0_4
            {
                allowed.insert(EXTENSION_FEATURE_TOOL_COMPOSITION);
            }
            if manifest.api_version == EXTENSION_API_VERSION_0_4 && offered_host_services.remote_ui
            {
                allowed.insert(EXTENSION_FEATURE_REMOTE_UI);
            }
            if offered_host_services.agent_sessions {
                allowed.insert(EXTENSION_FEATURE_AGENT_SESSIONS);
                allowed.insert(EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1);
                if manifest.name == "octet-subagents" {
                    allowed.insert(EXTENSION_FEATURE_DELEGATION_TELEMETRY);
                }
            }
            if offered_host_services.approvals {
                allowed.insert(EXTENSION_FEATURE_APPROVALS);
            }
            if offered_host_services.secrets {
                allowed.insert(EXTENSION_FEATURE_SECRETS);
            }
            if manifest.capabilities.system_prompt {
                allowed.insert(EXTENSION_FEATURE_SYSTEM_PROMPT_READ);
            }
            if let Some(feature) = features
                .iter()
                .find(|feature| !allowed.contains(feature.as_str()))
            {
                return Err(ExtensionRuntimeError::Protocol(format!(
                    "extension advertised unknown feature `{feature}`"
                )));
            }
            if let Some(feature) = API_0_2_REQUIRED_FEATURES
                .iter()
                .find(|feature| !features.contains(**feature))
            {
                return Err(ExtensionRuntimeError::Protocol(format!(
                    "extension is missing required feature `{feature}`"
                )));
            }
            if manifest
                .contributes
                .hooks
                .contains(&ExtensionHook::CompactionStrategy)
                && !features.contains(EXTENSION_FEATURE_COMPACTION_STRATEGY)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "compaction_strategy hook requires negotiated compaction_strategy feature"
                        .into(),
                ));
            }
            if manifest
                .contributes
                .hooks
                .contains(&ExtensionHook::CacheWarmingDecision)
                && !features.contains(EXTENSION_FEATURE_CACHE_WARMING_DECISION)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "cache_warming_decision hook requires negotiated cache_warming_decision feature"
                        .into(),
                ));
            }
            if manifest
                .contributes
                .hooks
                .contains(&ExtensionHook::ProviderContext)
                && !features.contains(EXTENSION_FEATURE_SESSION_ENTRIES)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "provider_context hook requires negotiated session_entries feature".into(),
                ));
            }
            if offered_host_services.agent_sessions
                && manifest.name == "octet-subagents"
                && !features.contains(EXTENSION_FEATURE_DELEGATION_TELEMETRY)
            {
                return Err(ExtensionRuntimeError::Protocol(format!(
                    "first-party octet-subagents requires `{EXTENSION_FEATURE_DELEGATION_TELEMETRY}`; reinstall the current workspace bundle"
                )));
            }
            if features.contains(EXTENSION_FEATURE_AUTOCOMPLETE_EDIT_V1)
                && !features.contains(EXTENSION_FEATURE_AUTOCOMPLETE)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "autocomplete_edit_v1 negotiation requires autocomplete".into(),
                ));
            }
            if features.contains(EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1)
                && !features.contains(EXTENSION_FEATURE_AGENT_SESSIONS)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "agent_model_selection_v1 negotiation requires agent_sessions".into(),
                ));
            }
            if features.contains(EXTENSION_FEATURE_APPROVALS)
                && !features.contains(EXTENSION_FEATURE_POLICY_INTENTS)
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "approvals negotiation requires policy_intents".into(),
                ));
            }
            let all_lifecycle = [
                methods::SESSION_STARTED,
                methods::SESSION_SETTLED,
                methods::TURN_STARTED,
                methods::TURN_SETTLED,
                methods::TOOL_STARTED,
                methods::TOOL_SETTLED,
            ]
            .into_iter()
            .collect::<BTreeSet<_>>();
            let lifecycle_events = if features.contains(EXTENSION_FEATURE_LIFECYCLE_EVENTS) {
                if negotiated.lifecycle_events.is_empty() {
                    all_lifecycle
                        .iter()
                        .map(|method| (*method).to_owned())
                        .collect()
                } else {
                    let subscribed = negotiated
                        .lifecycle_events
                        .into_iter()
                        .collect::<BTreeSet<_>>();
                    if let Some(method) = subscribed
                        .iter()
                        .find(|method| !all_lifecycle.contains(method.as_str()))
                    {
                        return Err(ExtensionRuntimeError::Protocol(format!(
                            "unknown lifecycle subscription `{method}`"
                        )));
                    }
                    subscribed
                }
            } else {
                if !negotiated.lifecycle_events.is_empty() {
                    return Err(ExtensionRuntimeError::Protocol(
                        "lifecycle subscriptions require lifecycle_events".into(),
                    ));
                }
                BTreeSet::new()
            };
            ExtensionNegotiatedProtocol {
                version: manifest.api_version.clone(),
                features,
                max_concurrent_requests: negotiated
                    .limits
                    .max_concurrent_requests
                    .min(host_max_concurrent_requests),
                lifecycle_events,
            }
        }
        _ => unreachable!("manifest validation accepts only API 0.1, 0.2, or 0.4"),
    };

    let tool_names = response
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    if !protocol.supports(EXTENSION_FEATURE_DYNAMIC_TOOLS) {
        ensure_same_contributions("tools", &manifest.contributes.tools, &tool_names)?;
    }
    validate_tool_definitions_for_protocol(&response.tools, &protocol)?;

    if response.shortcuts != manifest.contributes.shortcuts {
        return Err(ExtensionRuntimeError::Protocol(
            "initialized shortcuts do not match manifest declarations".into(),
        ));
    }
    validate_shortcut_definitions(&response.shortcuts).map_err(ExtensionRuntimeError::Protocol)?;

    let command_names = response
        .commands
        .iter()
        .map(|command| command.name.clone())
        .collect::<Vec<_>>();
    if !protocol.supports(EXTENSION_FEATURE_RUNTIME_COMMANDS) {
        ensure_same_contributions("commands", &manifest.contributes.commands, &command_names)?;
    }
    if response.commands.len() > MAX_EXTENSION_COMMANDS {
        return Err(ExtensionRuntimeError::Protocol(format!(
            "command catalog contains {} commands; limit is {MAX_EXTENSION_COMMANDS}",
            response.commands.len()
        )));
    }
    let mut unique_commands = BTreeSet::new();
    for command in &response.commands {
        if !unique_commands.insert(command.name.as_str()) {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "duplicate command definition `{}`",
                command.name
            )));
        }
        validate_identifier("command", &command.name, true)?;
        if command.description.trim().is_empty() {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "command `{}` has an empty description",
                command.name
            )));
        }
    }

    let tool_renderers = if protocol.supports(EXTENSION_FEATURE_DYNAMIC_TOOL_RENDERERS) {
        if response.tool_renderers.len() > MAX_DYNAMIC_EXTENSION_TOOLS {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "tool renderer catalog contains {} entries; limit is {MAX_DYNAMIC_EXTENSION_TOOLS}",
                response.tool_renderers.len()
            )));
        }
        let mut unique_renderers = BTreeSet::new();
        for renderer in &response.tool_renderers {
            if !unique_renderers.insert(renderer.as_str()) {
                return Err(ExtensionRuntimeError::Protocol(format!(
                    "tool renderer catalog contains duplicate `{renderer}`"
                )));
            }
            validate_identifier("tool renderer", renderer, true)?;
        }
        response.tool_renderers
    } else {
        if !response.tool_renderers.is_empty()
            && response.tool_renderers != manifest.contributes.tool_renderers
        {
            return Err(ExtensionRuntimeError::Protocol(
                "runtime tool renderers require dynamic_tool_renderers negotiation".into(),
            ));
        }
        manifest.contributes.tool_renderers.clone()
    };

    Ok((
        ExtensionContributions {
            tools: response.tools,
            commands: response.commands,
            shortcuts: response.shortcuts,
            hooks: manifest.contributes.hooks.clone(),
            context: manifest.contributes.context,
            ui: manifest.contributes.ui.clone(),
            tool_renderers,
            notifications: manifest.contributes.notifications,
            confirmations: manifest.contributes.confirmations,
            presentation: manifest.contributes.presentation,
            menu: manifest.contributes.menu,
            providers: false,
        },
        protocol,
    ))
}

/// Whether `version` speaks the stateful legacy protocol generation
/// (cancellation, progress, lifecycle events, agent sessions, UI, presentation,
/// resource-owner fences). API `0.4` folds API `0.2` and `0.3` into one version,
/// so every version above `0.1` is stateful: only the frozen API `0.1` text
/// contract is restricted.
pub(super) fn is_stateful_api(version: &str) -> bool {
    version != EXTENSION_API_VERSION_0_1
}

/// Whether `version` may use the API `0.2`-generation capabilities directly.
/// API `0.4` rides the API `0.2` feature-negotiation wire as the union of every
/// earlier capability, so both API `0.2` and API `0.4` take that generation's
/// paths; API `0.3` keeps its canonical wire.
pub(super) fn uses_api_0_2_capabilities(version: &str) -> bool {
    version == EXTENSION_API_VERSION_0_2 || version == EXTENSION_API_VERSION_0_4
}

/// Whether `version` speaks the canonical (schema-generated) API `0.3` wire.
pub(super) fn is_canonical_api(version: &str) -> bool {
    version == EXTENSION_API_VERSION_0_3
}

// Counts the complete prospective catalog's input AND output schema bytes.
// Individual register frames are bounded independently; many small mutations
// must not accumulate an arbitrarily large live catalog.
pub(super) const MAX_TOOL_CATALOG_SCHEMA_BYTES: usize = 4 * 1024 * 1024;
