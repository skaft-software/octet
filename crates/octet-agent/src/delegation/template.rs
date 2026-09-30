//! Agent identity, runtime settings and the template a child agent is built from.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct AgentIdentity {
    pub(super) id: String,
    pub(super) path: String,
    pub(super) depth: usize,
}

#[derive(Clone)]
pub(crate) struct DelegationRuntimeSettings {
    pub(crate) compaction_model: Option<octet_ai::Model>,
    pub(crate) auto_compaction_mode: AgentCompactionMode,
    pub(crate) auto_compaction_threshold: f64,
    pub(crate) compaction_keep_recent_tokens: u64,
    pub(crate) completion_policy: CompletionPolicy,
    pub(crate) output_modalities: octet_ai::OutputModalities,
    pub(crate) max_output_tokens: u64,
    pub(crate) tool_schema_budget_bytes: usize,
    pub(crate) max_session_tokens: Option<u64>,
    pub(crate) max_session_cost_microdollars: Option<u64>,
    pub(crate) provider_retries_enabled: bool,
    pub(crate) max_network_wait: Option<Duration>,
}

pub(crate) struct DelegationTemplate {
    pub(crate) model_resolver: RwLock<Option<Arc<dyn AgentModelResolver>>>,
    pub(crate) client: octet_ai::AiClient,
    pub(crate) model: octet_ai::Model,
    pub(crate) base_system: RwLock<String>,
    pub(crate) sandbox: crate::SandboxConfig,
    pub(crate) effect_broker: crate::EffectBroker,
    pub(crate) extensions: ExtensionHost,
    pub(crate) max_turns: Option<u64>,
    pub(crate) reasoning: RwLock<octet_ai::ReasoningConfig>,
    pub(crate) reasoning_mode: octet_ai::ReasoningMode,
    pub(crate) cache_retention: octet_ai::CacheRetention,
    pub(crate) runtime: RwLock<DelegationRuntimeSettings>,
}

pub(super) fn lower_child_reasoning(mut resolved: ResolvedAgentModel) -> ResolvedAgentModel {
    // Astra's host-side Ultra tier enables V2 collaboration, but the
    // observed child-run wire contract is xhigh. Keep root and generic
    // Ultra lowering unchanged by translating only this child boundary.
    let reasoning = if resolved.model.spec.api_name == "gpt-6-astra"
        && resolved.model.spec.capabilities.agent_delegation == Some(octet_ai::AgentDelegation::V2)
        && resolved
            .model
            .spec
            .capabilities
            .reasoning
            .as_ref()
            .is_some_and(|reasoning| reasoning.max_effort == octet_ai::ReasoningEffort::Ultra)
        && resolved.reasoning == octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra)
    {
        octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Xhigh)
    } else {
        resolved.reasoning.clone()
    };
    if reasoning != resolved.reasoning {
        resolved.reasoning = reasoning;
        resolved.metadata.reasoning = "xhigh".into();
    }
    resolved
}

impl DelegationTemplate {
    pub(super) fn resolve_model(
        &self,
        policy: Option<&ExtensionAgentSessionPolicy>,
    ) -> Result<ResolvedAgentModel, String> {
        // Existing workers keep their host-pinned effort across parent changes.
        // Only a new inherited worker reads the parent's current selection.
        let reasoning = policy
            .and_then(|p| p.resolved_reasoning.clone())
            .unwrap_or_else(|| {
                self.reasoning
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
            });
        let reasoning_label = match &reasoning {
            octet_ai::ReasoningConfig::Off => "off".into(),
            octet_ai::ReasoningConfig::On => "on".into(),
            octet_ai::ReasoningConfig::Effort(effort) => format!("{effort:?}").to_lowercase(),
            octet_ai::ReasoningConfig::Budget(n) => format!("budget={n}"),
        };
        if policy
            .and_then(|p| p.resolved_model.as_ref())
            .is_some_and(|pinned| pinned.reasoning != reasoning_label)
        {
            return Err("unsupported_reasoning: saved worker reasoning changed".into());
        }
        let mut selection = policy
            .and_then(|p| p.resolved_model.as_ref().or(p.model_selection.as_ref()))
            .cloned()
            .unwrap_or_default();
        // A newly inherited worker must retain the parent's exact binding, not
        // re-select updated catalog metadata merely because admission pinned IDs.
        if policy.is_some_and(|p| {
            p.model_selection
                .as_ref()
                .is_none_or(|s| s == &AgentModelSelection::default())
                && p.resolved_model
                    .as_ref()
                    .is_some_and(|m| m.model == self.model.spec.id.0)
                && p.resolved_reasoning.as_ref() == Some(&reasoning)
        }) {
            selection = AgentModelSelection::default();
        }
        if let Some(resolver) = self
            .model_resolver
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let resolved =
                lower_child_reasoning(resolver.resolve(&selection, &self.model, &reasoning)?);
            if let Some(policy) = policy {
                if policy
                    .resolved_model
                    .as_ref()
                    .is_some_and(|pinned| pinned != &resolved.metadata)
                    || policy
                        .resolved_reasoning
                        .as_ref()
                        .is_some_and(|pinned| pinned != &resolved.reasoning)
                {
                    return Err(
                        "unsupported_model: saved worker selection no longer resolves exactly"
                            .into(),
                    );
                }
            }
            return Ok(resolved);
        }
        let metadata = AgentModelSelection {
            provider: self.model.spec.endpoint.0.clone(),
            model: self.model.spec.id.0.clone(),
            reasoning: reasoning_label,
        };
        let resolved = lower_child_reasoning(ResolvedAgentModel {
            model: self.model.clone(),
            reasoning: reasoning.clone(),
            metadata,
        });
        if (selection.provider != "inherit" && selection.provider != resolved.metadata.provider)
            || (selection.model != "inherit" && selection.model != resolved.metadata.model)
        {
            return Err("unsupported_model: no configured model resolver".into());
        }
        if selection.reasoning != "inherit" && selection.reasoning != resolved.metadata.reasoning {
            return Err("unsupported_reasoning: no configured reasoning resolver".into());
        }
        if policy
            .and_then(|p| p.resolved_reasoning.as_ref())
            .is_some_and(|pinned| pinned != &resolved.reasoning)
        {
            return Err("unsupported_reasoning: saved worker reasoning changed".into());
        }
        Ok(resolved)
    }
}
