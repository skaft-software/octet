//! The model catalog the host advertises, and the pricing it reports.
//! Tiered input rates, the private preset headers that must never leak, the
//! distinct states of an unset reasoning default, the ultra gate, and the bound
//! on catalog size are all properties of the catalog itself, so they are checked
//! without standing up a run.

use super::*;
use octet_serve_backend::{CatalogCursor, HostBootstrap, PROTOCOL_VERSION};

use super::test_support::*;

#[test]
fn graphical_pricing_projects_tiered_input_rates() {
    let pricing = octet_ai::Pricing {
        input: octet_ai::TokenRate(3_000_000),
        output: octet_ai::TokenRate(15_000_000),
        cache_read: octet_ai::TokenRate(300_000),
        cache_write_5m: octet_ai::TokenRate(3_750_000),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![
            octet_ai::PricingTier {
                min_input_tokens: 100_000,
                input: None,
                output: Some(octet_ai::TokenRate(20_000_000)),
                cache_read: None,
                cache_write_5m: None,
                cache_write_1h: None,
                reasoning: None,
            },
            octet_ai::PricingTier {
                min_input_tokens: 200_000,
                input: Some(octet_ai::TokenRate(6_000_000)),
                output: None,
                cache_read: None,
                cache_write_5m: None,
                cache_write_1h: None,
                reasoning: None,
            },
        ],
    };

    assert_eq!(
        graphical_input_pricing(Some(&pricing)),
        Some(ModelInputPricing {
            base_microdollars_per_million_tokens: 3_000_000,
            tiers: vec![ModelInputPricingTier {
                min_input_tokens: 200_000,
                microdollars_per_million_tokens: 6_000_000,
            }],
        })
    );
    assert_eq!(graphical_input_pricing(None), None);
}

fn catalog_model(index: usize) -> ModelSummary {
    ModelSummary {
        id: format!("model-{index:03}"),
        name: format!("Model {index:03}"),
        provider: "provider".into(),
        local: false,
        available: true,
        reasoning: vec!["off".into()],
        default_reasoning: Some("off".into()),
        input_pricing: None,
        input_modalities: vec![InputModality::Text],
    }
}

#[test]
fn graphical_model_catalog_omits_private_model_preset_headers() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = serve_test_config(directory.path());
    let mut catalog = ModelCatalog::builtin().unwrap();
    let mut spec = (*catalog
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap()
        .spec)
        .clone();
    spec.id = ModelId("private-header-projection-fixture".into());
    spec.preset.headers.insert(
        "x-private-model-header".into(),
        "model-header-value-must-not-be-public".into(),
    );
    config.model = Some(spec.id.clone());
    catalog.register_model(spec).unwrap();
    let models = graphical_model_catalog(&catalog, &config);
    assert!(models
        .iter()
        .any(|model| model.id == "private-header-projection-fixture"));
    let encoded = serde_json::to_string(&models).unwrap();
    for forbidden in [
        "preset",
        "headers",
        "x-private-model-header",
        "model-header-value-must-not-be-public",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn graphical_reasoning_defaults_distinguish_unset_config_and_persisted_off() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = serve_test_config(directory.path());
    let mut catalog = ModelCatalog::builtin().unwrap();
    let mut spec = (*catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap()
        .spec)
        .clone();
    spec.id = ModelId("reasoning-default-fixture".into());
    spec.capabilities.reasoning.as_mut().unwrap().options =
        Some(octet_ai::types::ReasoningOptions {
            values: vec!["none".into(), "low".into(), "high".into()],
            default: Some("high".into()),
        });
    config.model = Some(spec.id.clone());
    catalog.register_model(spec).unwrap();
    for (preference, expected) in [(None, "high"), (Some(ReasoningConfig::Off), "off")] {
        config.reasoning = preference;
        let models = graphical_model_catalog(&catalog, &config);
        let summary = models
            .iter()
            .find(|model| model.id == "reasoning-default-fixture")
            .unwrap();
        assert_eq!(summary.reasoning, ["off", "low", "high"]);
        assert_eq!(summary.default_reasoning.as_deref(), Some(expected));
        assert_eq!(selection_from_summary(summary).reasoning, expected);
        assert_eq!(
            selection_from_persisted_config(None, None, &catalog, &config)
                .unwrap()
                .reasoning,
            expected
        );
        assert_eq!(
            selection_from_persisted_config(None, Some("off".into()), &catalog, &config)
                .unwrap()
                .reasoning,
            "off"
        );
        assert_eq!(
            selection_from_persisted_config(None, Some("low".into()), &catalog, &config)
                .unwrap()
                .reasoning,
            "low"
        );
    }
}

#[test]
fn graphical_model_default_keeps_ultra_gated_without_selecting_off() {
    let directory = tempfile::tempdir().unwrap();
    let config = serve_test_config(directory.path());
    let mut catalog = ModelCatalog::builtin().unwrap();
    let mut spec = (*catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap()
        .spec)
        .clone();
    spec.id = ModelId("ultra-default-fixture".into());
    spec.capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
    let capability = spec.capabilities.reasoning.as_mut().unwrap();
    capability.max_effort = octet_ai::ReasoningEffort::Ultra;
    capability.options = Some(octet_ai::types::ReasoningOptions {
        values: ["none", "low", "high", "max", "ultra"]
            .map(str::to_owned)
            .to_vec(),
        default: Some("ultra".into()),
    });
    catalog.register_model(spec).unwrap();
    let models = graphical_model_catalog(&catalog, &config);
    let summary = models
        .iter()
        .find(|model| model.id == "ultra-default-fixture")
        .unwrap();
    assert!(!summary.reasoning.iter().any(|choice| choice == "ultra"));
    assert_eq!(summary.default_reasoning.as_deref(), Some("max"));

    let mut enabled = config;
    enabled.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    enabled.sandbox.allow_process = true;
    enabled.enabled_extensions.push("octet-subagents".into());
    let models = graphical_model_catalog(&catalog, &enabled);
    let summary = models
        .iter()
        .find(|model| model.id == "ultra-default-fixture")
        .unwrap();
    assert_eq!(summary.reasoning.last().map(String::as_str), Some("ultra"));
    assert_eq!(summary.default_reasoning.as_deref(), Some("ultra"));
}

#[test]
fn graphical_catalog_keeps_models_beyond_the_old_cutoff() {
    let directory = tempfile::tempdir().unwrap();
    let config = serve_test_config(directory.path());
    let mut catalog = ModelCatalog::builtin().unwrap();
    let template = (*catalog.resolve(&ModelId("gpt-6-sol".into())).unwrap().spec).clone();
    for index in 0..300 {
        let mut spec = template.clone();
        spec.id = ModelId(format!("catalog-fixture-{index}"));
        spec.display_name = Some(format!("AAA fixture {index}"));
        catalog.register_model(spec).unwrap();
    }
    let models = graphical_model_catalog(&catalog, &config);
    assert_eq!(models.len(), catalog.models().count());
    for id in ["gpt-6-astra", "gpt-6-sol", "gpt-6-luna"] {
        assert!(models.iter().any(|model| model.id == id), "{id}");
    }
}

#[test]
fn graphical_catalog_is_stably_bounded_and_retains_the_configured_model() {
    let mut forward = (0..MAX_GRAPHICAL_MODELS + 1)
        .map(catalog_model)
        .collect::<Vec<_>>();
    let configured = ModelId("zz-configured".into());
    forward.last_mut().unwrap().id = configured.0.clone();
    forward.last_mut().unwrap().name = "zz-configured".into();
    let mut reverse = forward.clone();
    reverse.reverse();

    let models = bound_graphical_models(forward, Some(&configured));
    assert_eq!(models, bound_graphical_models(reverse, Some(&configured)));
    assert_eq!(models.len(), MAX_GRAPHICAL_MODELS);
    assert!(models.iter().any(|summary| summary.id == configured.0));
    assert!(models.iter().any(|summary| summary.id == "model-255"));

    let selected = models
        .iter()
        .find(|summary| summary.id == configured.0)
        .unwrap();
    let selection = selection_from_summary(selected);
    let session_id = SessionId::new("catalog-limit-session").unwrap();
    let seed = empty_seed(
        session_id.clone(),
        None,
        selection,
        AuthorityProfile::FullAccess,
        1,
    );
    let theme_id = ThemeId::new("catalog-limit-theme").unwrap();
    let bootstrap = HostBootstrap {
        protocol: PROTOCOL_VERSION,
        host: HostDescriptor {
            id: HostId::new("catalog-limit-host").unwrap(),
            name: "octet test".into(),
        },
        capabilities: HostCapabilities::default(),
        catalog_cursor: CatalogCursor(1),
        models,
        authority_profiles: vec![AuthorityProfile::FullAccess],
        authority_ceiling: AuthorityProfile::FullAccess,
        themes: vec![ThemeOption {
            id: theme_id.clone(),
            theme: ThemeDto {
                name: "Test".into(),
                source: ThemeSourceClass::Bundled,
                revision: 1,
                scheme: ColorScheme::Dark,
                density: ThemeDensity::Comfortable,
                motion: ThemeMotion::Full,
                typography: ThemeTypography {
                    body_family: "system-ui".into(),
                    mono_family: "ui-monospace".into(),
                    body_size: 17,
                    display_ratio_milli: 1235,
                },
                colors: BTreeMap::new(),
                roles: BTreeMap::new(),
            },
        }],
        selected_theme_id: theme_id,
        projects: Vec::new(),
        sessions: vec![seed.summary],
        selected_session_id: Some(session_id),
        selected_session: Some(seed.snapshot),
    };
    bootstrap.validate().unwrap();
}
