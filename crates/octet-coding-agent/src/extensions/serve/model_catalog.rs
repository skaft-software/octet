//! The graphical model catalog and model selection.

use super::*;

pub(super) fn graphical_input_pricing(
    pricing: Option<&octet_ai::Pricing>,
) -> Option<ModelInputPricing> {
    pricing.map(|pricing| ModelInputPricing {
        base_microdollars_per_million_tokens: pricing.input.0,
        tiers: pricing
            .tiers
            .iter()
            .filter_map(|tier| {
                tier.input.map(|rate| ModelInputPricingTier {
                    min_input_tokens: tier.min_input_tokens,
                    microdollars_per_million_tokens: rate.0,
                })
            })
            .take(MAX_MODEL_INPUT_PRICING_TIERS)
            .collect(),
    })
}

pub(super) fn graphical_model_catalog(
    catalog: &ModelCatalog,
    config: &Config,
) -> Vec<ModelSummary> {
    let subagents_available = subagents_extension_activation_configured(config);
    let models = catalog
        .models()
        .filter_map(|spec| catalog.resolve(&spec.id).ok())
        .map(|model| {
            let reasoning = supported_levels_with_subagents(&model, subagents_available)
                .into_iter()
                .map(thinking_label)
                .collect::<Vec<_>>();
            let preference = config
                .reasoning
                .clone()
                .unwrap_or_else(|| crate::app::default_reasoning_for_model(&model));
            let requested_default = selection_for_model(&model, &preference, config).reasoning;
            let default_reasoning = reasoning
                .iter()
                .find(|choice| choice.as_str() == requested_default.as_str())
                .cloned()
                .or_else(|| reasoning.first().cloned());
            let mut input_modalities = vec![InputModality::Text];
            if model
                .spec
                .capabilities
                .input_modalities
                .contains(Modality::Image)
            {
                input_modalities.push(InputModality::Image);
            }
            if model
                .spec
                .capabilities
                .input_modalities
                .contains(Modality::Audio)
            {
                input_modalities.push(InputModality::Audio);
            }
            ModelSummary {
                id: model.spec.id.0.clone(),
                name: model
                    .spec
                    .display_name
                    .clone()
                    .unwrap_or_else(|| model.spec.id.0.clone()),
                provider: model.endpoint.id.0.clone(),
                local: model
                    .endpoint
                    .base_url
                    .host_str()
                    .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1")),
                available: true,
                reasoning,
                default_reasoning,
                input_pricing: graphical_input_pricing(model.spec.pricing.as_ref()),
                input_modalities,
            }
        })
        .collect();
    bound_graphical_models(models, config.model.as_ref())
}

pub(super) fn bound_graphical_models(
    mut models: Vec<ModelSummary>,
    configured_model: Option<&ModelId>,
) -> Vec<ModelSummary> {
    let compare = |left: &ModelSummary, right: &ModelSummary| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    };
    models.sort_by(compare);
    if models.len() <= MAX_GRAPHICAL_MODELS {
        return models;
    }

    let configured = configured_model.and_then(|configured| {
        models
            .iter()
            .position(|summary| summary.id == configured.0)
            .filter(|index| *index >= MAX_GRAPHICAL_MODELS)
            .map(|index| models.remove(index))
    });
    models.truncate(MAX_GRAPHICAL_MODELS - usize::from(configured.is_some()));
    if let Some(configured) = configured {
        models.push(configured);
        models.sort_by(compare);
    }
    models
}

pub(super) fn selection_from_summary(summary: &ModelSummary) -> ModelSelection {
    ModelSelection {
        provider: summary.provider.clone(),
        model: summary.id.clone(),
        reasoning: summary
            .default_reasoning
            .clone()
            .or_else(|| summary.reasoning.first().cloned())
            .unwrap_or_else(|| "off".into()),
    }
}

pub(super) fn selection_for_model(
    model: &Model,
    reasoning: &ReasoningConfig,
    config: &Config,
) -> ModelSelection {
    let normalized = crate::app::normalize_reasoning_selection_for_model_with_subagents(
        reasoning,
        octet_ai::ReasoningMode::Standard,
        model,
        subagents_extension_activation_configured(config),
    )
    .map(|(reasoning, _, _)| reasoning)
    .unwrap_or(ReasoningConfig::Off);
    let portable = crate::app::level_from_reasoning(&normalized, model)
        .map(thinking_label)
        .unwrap_or_else(|_| reasoning_label(&normalized));
    let choices =
        supported_levels_with_subagents(model, subagents_extension_activation_configured(config))
            .into_iter()
            .map(thinking_label)
            .collect::<Vec<_>>();
    let portable = choices
        .iter()
        .find(|choice| choice.as_str() == portable.as_str())
        .cloned()
        .or_else(|| choices.first().cloned())
        .unwrap_or_else(|| "off".into());
    let _ = config;
    ModelSelection {
        provider: model.endpoint.id.0.clone(),
        model: model.spec.id.0.clone(),
        reasoning: portable,
    }
}

pub(super) fn selection_from_session(
    session: &Session,
    catalog: &ModelCatalog,
    config: &Config,
) -> Result<ModelSelection, ServiceError> {
    let mut model = None;
    let mut reasoning = None;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let entry = session.entry(id).ok_or(ServiceError::InvalidSeed)?;
        if let EntryValue::Config {
            model: persisted_model,
            reasoning: persisted_reasoning,
            ..
        } = &entry.value
        {
            if model.is_none() {
                model = persisted_model.clone();
            }
            if reasoning.is_none() {
                reasoning = persisted_reasoning.clone();
            }
            if model.is_some() && reasoning.is_some() {
                break;
            }
        }
        cursor = entry.parent.as_ref();
    }
    selection_from_persisted_config(model, reasoning, catalog, config)
}

pub(super) fn selection_from_catalog_entry(
    entry: &SessionCatalogEntry,
    catalog: &ModelCatalog,
    config: &Config,
) -> Result<ModelSelection, ServiceError> {
    selection_from_persisted_config(
        entry.configured_model.clone(),
        entry.configured_reasoning.clone(),
        catalog,
        config,
    )
}

pub(super) fn selection_from_persisted_config(
    model: Option<String>,
    reasoning: Option<String>,
    catalog: &ModelCatalog,
    config: &Config,
) -> Result<ModelSelection, ServiceError> {
    let model_id = model
        .map(ModelId)
        .or_else(|| config.model.clone())
        .ok_or(ServiceError::InvalidSeed)?;
    let model = catalog
        .resolve(&model_id)
        .map_err(|_| ServiceError::InvalidSeed)?;
    let reasoning = reasoning
        .as_deref()
        .map(config::parse_reasoning)
        .transpose()
        .map_err(|_| ServiceError::InvalidSeed)?
        .or_else(|| config.reasoning.clone())
        .unwrap_or_else(|| crate::app::default_reasoning_for_model(&model));
    Ok(selection_for_model(&model, &reasoning, config))
}

pub(super) fn advertised_selection_from_session(
    session: &Session,
    catalog: &ModelCatalog,
    config: &Config,
    models: &[ModelSummary],
) -> Option<ModelSelection> {
    selection_from_session(session, catalog, config)
        .ok()
        .filter(|selection| {
            models
                .iter()
                .any(|model| model.provider == selection.provider && model.id == selection.model)
        })
}

pub(super) fn advertised_selection_from_catalog_entry(
    entry: &SessionCatalogEntry,
    catalog: &ModelCatalog,
    config: &Config,
    models: &[ModelSummary],
) -> Option<ModelSelection> {
    selection_from_catalog_entry(entry, catalog, config)
        .ok()
        .filter(|selection| {
            models
                .iter()
                .any(|model| model.provider == selection.provider && model.id == selection.model)
        })
}

#[cfg(test)]
pub(super) fn current_selection(plan: &WorkerPlan) -> ModelSelection {
    let summary = plan
        .available_models
        .iter()
        .find(|summary| summary.id == plan.launch.model.0);
    match summary {
        Some(summary) => {
            let projected = reasoning_label(&plan.launch.reasoning);
            ModelSelection {
                provider: summary.provider.clone(),
                model: summary.id.clone(),
                reasoning: summary
                    .reasoning
                    .iter()
                    .find(|choice| choice.as_str() == projected.as_str())
                    .cloned()
                    .or_else(|| summary.default_reasoning.clone())
                    .or_else(|| summary.reasoning.first().cloned())
                    .unwrap_or_else(|| "off".into()),
            }
        }
        None => ModelSelection {
            provider: "unknown".into(),
            model: plan.launch.model.0.clone(),
            reasoning: "off".into(),
        },
    }
}

pub(super) fn thinking_label(level: crate::config::ThinkingLevel) -> String {
    match level {
        crate::config::ThinkingLevel::Off => "off",
        crate::config::ThinkingLevel::On => "on",
        crate::config::ThinkingLevel::Minimal => "minimal",
        crate::config::ThinkingLevel::Low => "low",
        crate::config::ThinkingLevel::Medium => "medium",
        crate::config::ThinkingLevel::High => "high",
        crate::config::ThinkingLevel::Xhigh => "xhigh",
        crate::config::ThinkingLevel::Max => "max",
        crate::config::ThinkingLevel::Ultra => "ultra",
    }
    .into()
}
