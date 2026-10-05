//! Prompt composition from extension context, presentation formatting and model views.

use super::*;

pub(super) async fn notify_lifecycle_all(
    processes: &[ExtensionProcess],
    event: ExtensionLifecycleEvent,
) -> Vec<String> {
    futures_util::future::join_all(processes.iter().map(|process| {
        let process = process.clone();
        let event = event.clone();
        async move {
            match tokio::time::timeout(
                LIFECYCLE_NOTIFY_DEADLINE,
                process.notify_lifecycle(&event),
            )
            .await
            {
                Err(_) => Some(format!(
                    "warning: extension {:?} lifecycle notification exceeded {LIFECYCLE_NOTIFY_DEADLINE:?}",
                    process.descriptor().manifest.name
                )),
                Ok(Err(error)) => Some(format!(
                    "warning: extension {:?} lifecycle notification failed: {error}",
                    process.descriptor().manifest.name
                )),
                Ok(Ok(())) => None,
            }
        }
    }))
    .await
    .into_iter()
    .flatten()
    .collect()
}

pub(super) async fn start_session_hooks_all(
    processes: &[ExtensionProcess],
    resource_owner: &str,
) -> Vec<String> {
    futures_util::future::join_all(
        processes
            .iter()
            .filter(|process| process.declares_session_hooks())
            .map(|process| {
                let process = process.clone();
                let resource_owner = resource_owner.to_owned();
                async move {
                    match tokio::time::timeout(
                        LIFECYCLE_NOTIFY_DEADLINE,
                        process.start_session_hook_binding(resource_owner),
                    )
                    .await
                    {
                        Ok(Ok(())) => None,
                        Err(_) => Some(format!(
                            "warning: extension {:?} session_start hook exceeded {LIFECYCLE_NOTIFY_DEADLINE:?}",
                            process.descriptor().manifest.name
                        )),
                        Ok(Err(_)) => Some(format!(
                            "warning: extension {:?} session_start hook failed",
                            process.descriptor().manifest.name
                        )),
                    }
                }
            }),
    )
    .await
    .into_iter()
    .flatten()
    .collect()
}

pub(super) async fn settle_session_hooks_all(
    processes: &[ExtensionProcess],
    resource_owner: &str,
    outcome: ExtensionLifecycleOutcome,
) -> Vec<String> {
    futures_util::future::join_all(
        processes
            .iter()
            .filter(|process| process.declares_session_hooks())
            .map(|process| {
                let process = process.clone();
                let resource_owner = resource_owner.to_owned();
                async move {
                    match process
                        .settle_session_hook_binding(&resource_owner, outcome)
                        .await
                    {
                        Ok(()) => None,
                        Err(_) => Some(format!(
                            "warning: extension {:?} session_end hook failed",
                            process.descriptor().manifest.name
                        )),
                    }
                }
            }),
    )
    .await
    .into_iter()
    .flatten()
    .collect()
}

pub(super) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn clip_lifecycle_reason(reason: &str, limit: usize) -> String {
    if reason.len() <= limit {
        return reason.to_owned();
    }
    let marker = "[… truncated …]";
    let mut end = limit.saturating_sub(marker.len());
    while !reason.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{marker}", &reason[..end])
}

pub struct ExtensionPromptComposition {
    pub custom_messages: Vec<octet_agent::session::CustomMessage>,
    pub system: String,
    pub prompt: String,
    pub notifications: Vec<String>,
    pub pending_context_count: usize,
}

pub fn assistant_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

pub fn latest_assistant_text(session: &Session) -> String {
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(&id) else {
            break;
        };
        if let octet_agent::EntryValue::Message(Message::Assistant(message)) = &entry.value {
            return assistant_text(message);
        }
        cursor = entry.parent.clone();
    }
    String::new()
}

pub(super) fn context_contribution_bytes(
    contribution: &ContextContribution,
) -> Result<usize, String> {
    if contribution.label.len() > MAX_CONTEXT_LABEL_BYTES {
        return Err(format!(
            "label exceeds the {MAX_CONTEXT_LABEL_BYTES} byte limit"
        ));
    }
    if contribution.content.len() > MAX_CONTEXT_CONTRIBUTION_BYTES {
        return Err(format!(
            "content exceeds the {MAX_CONTEXT_CONTRIBUTION_BYTES} byte limit"
        ));
    }
    if contribution.label.contains('\0') || contribution.content.contains('\0') {
        return Err("label or content contains NUL".into());
    }
    let quoted_label = format!("{:?}", contribution.label);
    Ok("<octet-extension-context label="
        .len()
        .saturating_add(quoted_label.len())
        .saturating_add(">\n".len())
        .saturating_add(contribution.content.len())
        .saturating_add("\n</octet-extension-context>".len())
        // `join_around` separates every non-empty block from its neighbor.
        .saturating_add("\n\n".len()))
}

pub(super) fn compose_context(
    base_system: &str,
    prompt: String,
    contributions: Vec<ContextContribution>,
) -> anyhow::Result<(String, String)> {
    if contributions.len() > MAX_PENDING_CONTEXT_ITEMS {
        anyhow::bail!(
            "extension context exceeds the {} contribution limit",
            MAX_PENDING_CONTEXT_ITEMS
        );
    }
    let mut total = 0usize;
    let mut system_prefix = Vec::new();
    let mut system_suffix = Vec::new();
    let mut prompt_prefix = Vec::new();
    let mut prompt_suffix = Vec::new();
    for contribution in contributions {
        let contribution_bytes = context_contribution_bytes(&contribution).map_err(|error| {
            anyhow::anyhow!("extension context {:?}: {error}", contribution.label)
        })?;
        total = total.saturating_add(contribution_bytes);
        if total > MAX_EXTENSION_CONTEXT_BYTES {
            anyhow::bail!(
                "extension context exceeds the {} byte aggregate limit",
                MAX_EXTENSION_CONTEXT_BYTES
            );
        }
        let block = format!(
            "<octet-extension-context label={:?}>\n{}\n</octet-extension-context>",
            contribution.label, contribution.content
        );
        match contribution.placement {
            ContextPlacement::SystemPrefix => system_prefix.push(block),
            ContextPlacement::SystemSuffix => system_suffix.push(block),
            ContextPlacement::PromptPrefix => prompt_prefix.push(block),
            ContextPlacement::PromptSuffix => prompt_suffix.push(block),
        }
    }
    let system = join_around(system_prefix, base_system.to_owned(), system_suffix);
    let prompt = join_around(prompt_prefix, prompt, prompt_suffix);
    Ok((system, prompt))
}

pub(super) fn join_around(prefix: Vec<String>, center: String, suffix: Vec<String>) -> String {
    prefix
        .into_iter()
        .chain(std::iter::once(center))
        .chain(suffix)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) fn format_presentation_views(views: &[ExtensionPresentationView]) -> String {
    let mut lines = Vec::new();
    for view in views {
        lines.push(format!(
            "[{}] generation {} · revision {}",
            view.extension, view.generation, view.snapshot.revision
        ));
        if let Some(status) = &view.snapshot.status {
            let state = format!("{:?}", status.state).to_lowercase();
            lines.push(format!("  status: {state} · {}", status.label));
            if let Some(detail) = &status.detail {
                lines.push(format!("    {detail}"));
            }
        }
        for activity in &view.snapshot.activities {
            let state = format!("{:?}", activity.state).to_lowercase();
            let provenance = activity
                .provenance
                .as_deref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default();
            lines.push(format!(
                "  activity {}: {state} · {}{provenance}",
                activity.kind, activity.summary
            ));
            lines.extend(
                activity
                    .references
                    .iter()
                    .map(|reference| format_presentation_reference("    ", reference)),
            );
        }
        if let Some(collection) = &view.snapshot.collection {
            lines.push(format!("  {}:", collection.title));
            let parents = collection
                .nodes
                .iter()
                .map(|node| (node.id.as_str(), node.parent_id.as_deref()))
                .collect::<BTreeMap<_, _>>();
            for node in &collection.nodes {
                let mut depth = 0usize;
                let mut parent = node.parent_id.as_deref();
                while let Some(id) = parent {
                    depth = depth.saturating_add(1);
                    parent = parents.get(id).copied().flatten();
                }
                let state = format!("{:?}", node.state).to_lowercase();
                let secondary = node
                    .secondary
                    .as_deref()
                    .map(|value| format!(" · {value}"))
                    .unwrap_or_default();
                lines.push(format!(
                    "    {}- {} · {state}{secondary}",
                    "  ".repeat(depth),
                    node.label
                ));
                lines.extend(
                    node.references
                        .iter()
                        .map(|reference| format_presentation_reference("      ", reference)),
                );
            }
            if let Some(detail) = &collection.detail {
                lines.push(format!("  detail: {}", detail.title));
                lines.extend(detail.body.lines().map(|line| format!("    {line}")));
                lines.extend(
                    detail
                        .references
                        .iter()
                        .map(|reference| format_presentation_reference("    ", reference)),
                );
            }
        }
        for action in &view.snapshot.actions {
            lines.push(format!(
                "  action {}: /{}{}{}",
                action.label,
                action.command,
                if action.arguments.is_empty() { "" } else { " " },
                action.arguments.join(" ")
            ));
        }
    }
    lines.join("\n")
}

pub(super) fn format_presentation_reference(
    indent: &str,
    reference: &octet_agent::ExtensionPresentationReference,
) -> String {
    let kind = match reference.kind {
        octet_agent::ExtensionPresentationReferenceKind::Session => "session",
        octet_agent::ExtensionPresentationReferenceKind::Artifact => "artifact",
        octet_agent::ExtensionPresentationReferenceKind::Resource => "resource",
        octet_agent::ExtensionPresentationReferenceKind::Url => "source",
    };
    let label = reference
        .label
        .as_deref()
        .map(|label| format!("{label} · "))
        .unwrap_or_default();
    let value = format!("{indent}{kind}: {label}{}", reference.id);
    if reference.kind == octet_agent::ExtensionPresentationReferenceKind::Session {
        format!(
            "{value}\n{indent}  inspect: /extensions inspect {}",
            reference.id
        )
    } else {
        value
    }
}

pub(super) fn format_notification(
    extension: &str,
    notification: &octet_agent::extension_process::ExtensionNotification,
) -> String {
    let title = notification
        .title
        .as_deref()
        .map(|title| format!(" {title}:"))
        .unwrap_or_default();
    format!(
        "[{extension} {:?}]{title} {}",
        notification.level, notification.message
    )
}

pub(super) fn extension_execution_context(
    process: &ExtensionProcess,
    resource_owner: Option<&str>,
) -> octet_agent::extension_process::ExtensionExecutionContext {
    resource_owner.map_or_else(
        || process.current_context(),
        |owner| process.current_context_for_resource_owner(owner.to_owned()),
    )
}

pub(super) fn host_state(
    session: &Session,
    model: &Model,
    reasoning: &ReasoningConfig,
    sessions: &SessionStore,
) -> ExtensionHostState {
    let session_id = session
        .path()
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_owned);
    let session_name = session_id
        .as_deref()
        .and_then(|id| sessions.load_metadata(id).ok())
        .and_then(|metadata| metadata.name);
    let active_skills = session
        .head()
        .and_then(|head| session.resolve_active_skills(&head).ok())
        .map(|state| {
            state
                .active_skills
                .into_iter()
                .map(
                    |skill| octet_agent::extension_process::ExtensionActiveSkill {
                        id: skill.descriptor.id,
                        name: skill.descriptor.name,
                        version: skill.descriptor.version,
                    },
                )
                .collect()
        })
        .unwrap_or_default();
    ExtensionHostState {
        session_id,
        session_name,
        model: Some(model.spec.id.0.clone()),
        model_view: extension_model_view(model),
        reasoning: pi_thinking_level(model, reasoning).map(serde_json::Value::String),
        pi_models: None,
        active_skills,
    }
}

/// Pi's wire-API name for one octet protocol.
///
/// Every octet protocol has an exact Pi `KnownApi` spelling, so this projection
/// never invents a name.
pub(super) fn pi_api_name(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiChat => "openai-completions",
        Protocol::OpenAiResponses => "openai-responses",
        Protocol::AnthropicMessages => "anthropic-messages",
        Protocol::BedrockConverse => "bedrock-converse-stream",
        Protocol::GoogleGenerativeAi => "google-generative-ai",
        Protocol::MistralConversations => "mistral-conversations",
        Protocol::PiMessages => "pi-messages",
    }
}

/// The Pi provider identity that owns a model's route.
///
/// Pi reports the provider rather than the wire route, so a multi-route
/// declaration (`opencode-anthropic`) reports its declaring provider. Providers
/// octet registers itself namespace their model id as `provider/model`, and an
/// endpoint with no declaration falls back to the endpoint identity octet
/// already knows.
pub(super) fn pi_provider_id(model: &Model) -> String {
    let endpoint = model.endpoint.id.0.as_str();
    if let Some(declaration) =
        crate::providers::ALL_PROVIDER_DECLARATIONS
            .iter()
            .find(|declaration| {
                declaration
                    .routes
                    .iter()
                    .any(|route| route.endpoint_id == endpoint)
            })
    {
        return declaration.id.to_owned();
    }
    match model.spec.id.0.split_once('/') {
        Some((provider, _)) if !provider.is_empty() => provider.to_owned(),
        _ => endpoint.to_owned(),
    }
}

/// Whether one model-view field fits the bounded wire width.
pub(super) fn model_field_fits(value: &str) -> bool {
    value.len() <= octet_agent::extension_process::MAX_EXTENSION_MODEL_FIELD_BYTES
}

/// Project one resolved model into Pi's `Model` shape.
///
/// Only fields octet can state truthfully are projected. The endpoint base URL
/// and credentials stay host-owned, so `baseUrl` is absent rather than
/// fabricated: a Pi extension observes `undefined`, never a URL octet did not
/// disclose. A model whose identifier, name, or provider exceeds the bounded
/// field width yields no view at all, so an extension never receives a
/// truncated identifier.
pub(super) fn extension_model_view(
    model: &Model,
) -> Option<octet_agent::extension_process::ExtensionModelView> {
    let spec = &model.spec;
    let provider = pi_provider_id(model);
    let name = spec
        .display_name
        .clone()
        .or_else(|| octet_ai::model_metadata::model_display_name(&spec.id.0))
        .unwrap_or_else(|| spec.api_name.clone());
    if !model_field_fits(&spec.api_name) || !model_field_fits(&provider) || !model_field_fits(&name)
    {
        return None;
    }
    let mut input = vec!["text".to_owned()];
    if spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image)
    {
        input.push("image".to_owned());
    }
    Some(octet_agent::extension_process::ExtensionModelView {
        id: spec.api_name.clone(),
        name: Some(name),
        base_url: None,
        api: pi_api_name(&spec.protocol).to_owned(),
        provider,
        reasoning: spec.capabilities.reasoning.is_some(),
        input,
        cost: spec.pricing.as_ref().map(|pricing| {
            octet_agent::extension_process::ExtensionModelCost {
                input: pricing.input.0,
                output: pricing.output.0,
                cache_read: pricing.cache_read.0,
                cache_write: pricing.cache_write_5m.0,
            }
        }),
        context_window: spec.limits.context_window,
        max_tokens: spec.limits.max_output_tokens,
    })
}

pub(super) fn block_on_runtime<F>(future: F) -> anyhow::Result<F::Output>
where
    F: Future + Send,
    F::Output: Send,
{
    let handle = Handle::try_current()
        .map_err(|_| anyhow::anyhow!("executable extensions require the octet Tokio runtime"))?;
    if handle.runtime_flavor() != RuntimeFlavor::MultiThread {
        anyhow::bail!("executable extensions require octet's multi-thread runtime");
    }
    Ok(tokio::task::block_in_place(|| handle.block_on(future)))
}
