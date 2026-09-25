//! Product-owned worker routing through the same configured catalog as `/model`.
//!
//! The kernel owns execution and persistence; this adapter owns provider naming
//! and the coding product's reasoning normalization. No endpoint or auth data
//! crosses the extension boundary.

use octet_agent::delegation::{
    AgentModelDescriptor, AgentModelResolver, AgentModelSelection, ResolvedAgentModel,
};
use octet_ai::{Model, ModelCatalog, ReasoningConfig};

use super::{
    level_from_reasoning, normalize_reasoning_for_model, reasoning_label,
    supported_levels_with_subagents, thinking_to_reasoning,
};

pub(crate) struct CodingAgentModelResolver {
    catalog: ModelCatalog,
}

impl CodingAgentModelResolver {
    pub(crate) fn new(mut catalog: ModelCatalog) -> Self {
        catalog.retain_configured_models();
        Self { catalog }
    }

    fn selected_model(&self, selection: &AgentModelSelection) -> Result<Model, String> {
        // A canonical catalog ID wins over aliases, just as it does in `/model`.
        if let Ok(model) = self
            .catalog
            .resolve(&octet_ai::ModelId(selection.model.clone()))
        {
            if (selection.provider == "inherit" || selection.provider == provider_id(&model))
                && model.endpoint.auth.is_configured()
            {
                return Ok(model);
            }
        }
        let mut matches = self.catalog.models().filter_map(|spec| {
            let model = self.catalog.resolve(&spec.id).ok()?;
            if !model.endpoint.auth.is_configured() {
                return None;
            }
            let provider = provider_id(&model);
            if selection.provider != "inherit" && selection.provider != provider {
                return None;
            }
            let reference = &selection.model;
            let matches = reference == &spec.id.0
                || reference == &format!("{provider}/{}", spec.id.0)
                || reference == &format!("{provider}/{}", spec.api_name)
                || (selection.provider != "inherit" && reference == &spec.api_name);
            matches.then_some(model)
        });
        let model = matches.next().ok_or_else(|| {
            "unsupported_model: route unavailable; use subagent_models for configured model IDs"
                .to_owned()
        })?;
        if matches.next().is_some() {
            return Err(
                "unsupported_model: ambiguous route; use a canonical model ID from subagent_models"
                    .into(),
            );
        }
        Ok(model)
    }
}

/// Provider identity is derived from the host binding, never a caller's prefix.
fn provider_id(model: &Model) -> String {
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
    if crate::auth::custom::is_endpoint_id(endpoint) {
        if endpoint == crate::auth::custom::ENDPOINT_ID {
            return endpoint.to_owned();
        }
        // Custom catalog IDs preserve both the registry key and upstream model
        // namespace (e.g. custom/home/Qwen/model).
        if let Some(rest) = model.spec.id.0.strip_prefix("custom/") {
            if let Some((key, _)) = rest.split_once('/') {
                return format!("custom/{key}");
            }
        }
    }
    model
        .spec
        .id
        .0
        .split_once('/')
        .map_or_else(|| endpoint.to_owned(), |(provider, _)| provider.to_owned())
}

impl AgentModelResolver for CodingAgentModelResolver {
    fn resolve(
        &self,
        selection: &AgentModelSelection,
        parent_model: &Model,
        parent_reasoning: &ReasoningConfig,
    ) -> Result<ResolvedAgentModel, String> {
        if selection.provider != "inherit" && selection.model == "inherit" {
            return Err("unsupported_model: provider requires an explicit model".into());
        }
        let model = if selection.model == "inherit" {
            // Revalidate availability without changing the parent's exact binding.
            if !super::catalog_route_matches_active_model(&self.catalog, parent_model) {
                return Err(
                    "unsupported_model: parent route changed or is no longer configured".into(),
                );
            }
            parent_model.clone()
        } else {
            self.selected_model(selection)?
        };
        if !model.endpoint.auth.is_configured() {
            return Err("unsupported_model: route credentials are unavailable".into());
        }
        let same_model =
            model.spec.id == parent_model.spec.id && model.endpoint.id == parent_model.endpoint.id;
        let reasoning = if selection.reasoning == "inherit" && same_model {
            parent_reasoning.clone()
        } else {
            let requested = if selection.reasoning == "inherit" {
                // Portable effort/budget mappings follow `/model`. A custom
                // untranslatable budget is refused rather than silently reset.
                let level = level_from_reasoning(parent_reasoning, parent_model)
                    .map_err(|_| "unsupported_reasoning: inherited budget cannot translate; choose reasoning explicitly".to_owned())?;
                thinking_to_reasoning(level, &model)
            } else {
                crate::config::parse_reasoning(&selection.reasoning)
                    .and_then(|requested| normalize_reasoning_for_model(&requested, &model))
            };
            requested.map_err(|_| {
                "unsupported_reasoning: selection is incompatible with the configured model"
                    .to_owned()
            })?
        };
        Ok(ResolvedAgentModel {
            metadata: AgentModelSelection {
                provider: provider_id(&model),
                model: model.spec.id.0.clone(),
                reasoning: reasoning_label(&reasoning),
            },
            model,
            reasoning,
        })
    }

    fn models(
        &self,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AgentModelDescriptor>, String> {
        let query = query.unwrap_or_default().to_lowercase();
        Ok(self
            .catalog
            .models()
            .filter_map(|spec| {
                let model = self.catalog.resolve(&spec.id).ok()?;
                if !model.endpoint.auth.is_configured() {
                    return None;
                }
                let provider = provider_id(&model);
                if !query.is_empty()
                    && !spec.id.0.to_lowercase().contains(&query)
                    && !provider.to_lowercase().contains(&query)
                    && !spec
                        .display_name
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&query)
                {
                    return None;
                }
                Some(AgentModelDescriptor {
                    model: spec.id.0.clone(),
                    provider,
                    display_name: spec.display_name.clone(),
                    reasoning: supported_levels_with_subagents(&model, true)
                        .into_iter()
                        .map(|level| level.label().to_owned())
                        .collect(),
                    context_window: spec.limits.context_window,
                    max_output_tokens: spec.limits.max_output_tokens,
                })
            })
            .take(limit)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octet_ai::{Auth, ModelId, ReasoningEffort};
    use std::sync::Arc;

    fn fixture() -> (CodingAgentModelResolver, Model) {
        let builtin = ModelCatalog::builtin().unwrap();
        let mut catalog = ModelCatalog::default();
        let mut parent = None;
        for id in ["gpt-5.4-mini-responses", "claude-sonnet-4-6"] {
            let mut model = builtin.resolve(&ModelId(id.into())).unwrap();
            Arc::make_mut(&mut model.endpoint).auth = Auth::None;
            catalog
                .register_endpoint((*model.endpoint).clone())
                .unwrap();
            catalog.register_model((*model.spec).clone()).unwrap();
            if parent.is_none() {
                parent = Some(model);
            }
        }
        (CodingAgentModelResolver::new(catalog), parent.unwrap())
    }

    #[test]
    fn inherits_exact_parent_and_routes_explicit_provider_without_changing_parent() {
        let (resolver, parent) = fixture();
        let reasoning = ReasoningConfig::Effort(ReasoningEffort::High);
        let inherited = resolver
            .resolve(&AgentModelSelection::default(), &parent, &reasoning)
            .unwrap();
        assert!(Arc::ptr_eq(&inherited.model.spec, &parent.spec));
        assert_eq!(inherited.reasoning, reasoning);
        for (provider, model) in [
            ("inherit", "claude-sonnet-4-6"),
            ("anthropic", "claude-sonnet-4-6"),
            ("inherit", "anthropic/claude-sonnet-4-6"),
        ] {
            let selected = resolver
                .resolve(
                    &AgentModelSelection {
                        provider: provider.into(),
                        model: model.into(),
                        reasoning: "inherit".into(),
                    },
                    &parent,
                    &reasoning,
                )
                .unwrap();
            assert_eq!(selected.model.endpoint.id.0, "anthropic");
            assert_eq!(selected.metadata.model, "claude-sonnet-4-6");
            assert_eq!(selected.metadata.provider, "anthropic");
        }
        assert_eq!(parent.endpoint.id.0, "openai");
    }

    #[test]
    fn refuses_unknown_mismatched_and_incomplete_routes() {
        let (resolver, parent) = fixture();
        for (provider, model) in [
            ("inherit", "unknown"),
            ("openai", "claude-sonnet-4-6"),
            ("anthropic", "inherit"),
        ] {
            let result = resolver.resolve(
                &AgentModelSelection {
                    provider: provider.into(),
                    model: model.into(),
                    reasoning: "inherit".into(),
                },
                &parent,
                &ReasoningConfig::Off,
            );
            assert!(result.err().unwrap().starts_with("unsupported_model:"));
        }
    }

    #[test]
    fn discovery_is_filtered_bounded_and_secret_free() {
        let (resolver, _) = fixture();
        assert_eq!(resolver.models(None, 1).unwrap().len(), 1);
        let models = resolver.models(Some("ANTHROPIC"), 10).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "claude-sonnet-4-6");
        let public = serde_json::to_string(&models).unwrap();
        for private in ["base_url", "api_key", "headers", "auth", "https://"] {
            assert!(!public.contains(private));
        }
        assert!(resolver.models(Some("absent"), 10).unwrap().is_empty());
    }

    #[test]
    fn namespaced_routes_preserve_upstream_slashes_and_provider_identity() {
        let (mut resolver, parent) = fixture();
        for (id, api, endpoint, provider) in [
            (
                "openrouter/deepseek/model",
                "deepseek/model",
                "openrouter",
                "openrouter",
            ),
            (
                "custom/home/Qwen/model",
                "Qwen/model",
                "custom-provider-4-home",
                "custom/home",
            ),
            ("codex/gpt-test", "gpt-test", "openai-codex", "codex"),
            (
                "cloudflare-workers-ai/@cf/openai/model",
                "@cf/openai/model",
                "cloudflare-workers-ai",
                "cloudflare-workers-ai",
            ),
        ] {
            let mut model = parent.clone();
            Arc::make_mut(&mut model.endpoint).id = octet_ai::EndpointId(endpoint.into());
            let spec = Arc::make_mut(&mut model.spec);
            spec.endpoint = model.endpoint.id.clone();
            spec.id = ModelId(id.into());
            spec.api_name = api.into();
            resolver
                .catalog
                .register_endpoint((*model.endpoint).clone())
                .unwrap();
            resolver
                .catalog
                .register_model((*model.spec).clone())
                .unwrap();
            for (requested_provider, requested_model) in
                [("inherit", id), (provider, api), (provider, id)]
            {
                let selected = resolver
                    .resolve(
                        &AgentModelSelection {
                            provider: requested_provider.into(),
                            model: requested_model.into(),
                            reasoning: "inherit".into(),
                        },
                        &parent,
                        &ReasoningConfig::Off,
                    )
                    .unwrap();
                assert_eq!(selected.metadata.provider, provider);
                assert_eq!(selected.metadata.model, id);
                assert_eq!(selected.model.spec.api_name, api);
            }
        }
    }

    #[test]
    fn unavailable_credentials_and_changed_parent_are_not_selectable() {
        let (mut resolver, parent) = fixture();
        let mut unavailable = parent.clone();
        Arc::make_mut(&mut unavailable.endpoint).id =
            octet_ai::EndpointId("unavailable-fixture".into());
        // No environment mutation or ambient credentials are needed: this name
        // is unique to the fixture and is never set by the test.
        Arc::make_mut(&mut unavailable.endpoint).auth =
            Auth::bearer_env("OCTET_TEST_ROUTING_ABSENT_CREDENTIAL_71CD869F");
        let spec = Arc::make_mut(&mut unavailable.spec);
        spec.id = ModelId("unavailable-fixture/model".into());
        spec.endpoint = unavailable.endpoint.id.clone();
        resolver
            .catalog
            .register_endpoint((*unavailable.endpoint).clone())
            .unwrap();
        resolver
            .catalog
            .register_model((*unavailable.spec).clone())
            .unwrap();
        let selection = AgentModelSelection {
            model: unavailable.spec.id.0.clone(),
            ..Default::default()
        };
        assert!(resolver
            .resolve(&selection, &parent, &ReasoningConfig::Off)
            .is_err());
        assert!(resolver
            .models(Some("unavailable-fixture"), 10)
            .unwrap()
            .is_empty());
        let mut changed = (*parent.spec).clone();
        changed.api_name = "changed-route".into();
        resolver
            .catalog
            .remove_model_if_endpoint(&changed.id, &changed.endpoint);
        resolver.catalog.register_model(changed).unwrap();
        assert!(resolver
            .resolve(
                &AgentModelSelection::default(),
                &parent,
                &ReasoningConfig::Off
            )
            .is_err());
    }

    #[test]
    fn reasoning_uses_target_capability_and_rejects_untranslatable_budget() {
        let (resolver, parent) = fixture();
        let selection = AgentModelSelection {
            model: "claude-sonnet-4-6".into(),
            reasoning: "ultra".into(),
            ..Default::default()
        };
        let target = resolver
            .resolve(&selection, &parent, &ReasoningConfig::Off)
            .unwrap();
        assert_ne!(
            target.reasoning,
            ReasoningConfig::Effort(ReasoningEffort::Ultra)
        );
        let inherited = AgentModelSelection {
            reasoning: "inherit".into(),
            ..selection
        };
        assert!(resolver
            .resolve(&inherited, &parent, &ReasoningConfig::Budget(12345))
            .err()
            .unwrap()
            .starts_with("unsupported_reasoning:"));
    }
}
