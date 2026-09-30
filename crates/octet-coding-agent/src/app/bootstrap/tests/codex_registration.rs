//! Codex registration
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[test]
fn codex_models_require_a_usable_credential_and_include_astra_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    let store = crate::auth::codex::CredentialStore::new(&path);

    let mut catalog = base_model_catalog(true).unwrap();
    register_openai_codex(&mut catalog, store.clone(), false).unwrap();
    assert!(catalog.resolve(&ModelId("gpt-5.6-sol".into())).is_err());

    write_codex_credential(&path, true, "plus");
    let mut catalog = base_model_catalog(true).unwrap();
    let error = register_openai_codex(&mut catalog, store.clone(), false).unwrap_err();
    assert!(error.to_string().contains("localhost-only"));
    assert!(catalog.resolve(&ModelId("gpt-5.6-sol".into())).is_err());

    write_codex_credential(&path, false, "plus");
    let mut catalog = base_model_catalog(true).unwrap();
    register_openai_codex(&mut catalog, store, false).unwrap();
    for model_id in crate::auth::codex::MODELS {
        // GPT-6 routes are always namespaced; older ids keep their bare name.
        let catalog_id = if model_id.starts_with("gpt-6") {
            format!("codex/{model_id}")
        } else {
            (*model_id).to_owned()
        };
        let model = catalog.resolve(&ModelId(catalog_id)).unwrap();
        assert_eq!(model.endpoint.id.0, crate::auth::codex::ENDPOINT_ID);
        assert_eq!(model.spec.protocol, Protocol::OpenAiResponses);
        assert_eq!(
            model.spec.limits.context_window,
            if *model_id == "gpt-5.6-luna" {
                CODEX_5_6_CONTEXT_WINDOW
            } else {
                CODEX_CONTEXT_WINDOW_CAP
            }
        );
        assert_eq!(model.spec.limits.max_output_tokens, 128_000);
        // Subscription pricing is reviewed only where it was observed.
        assert_eq!(
            model.spec.pricing.is_some(),
            !matches!(*model_id, "gpt-6.1-sol" | "gpt-6-sol" | "gpt-6-luna")
        );
        if *model_id == "gpt-6-astra" {
            assert!(model
                .spec
                .capabilities
                .input_modalities
                .contains(octet_ai::Modality::Image));
            let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
            assert_eq!(reasoning.min_effort, octet_ai::ReasoningEffort::Low);
            assert_eq!(reasoning.max_effort, octet_ai::ReasoningEffort::Max);
            assert!(!model.spec.capabilities.responses_lite);
            assert_eq!(model.spec.capabilities.agent_delegation, None);
            let pricing = model.spec.pricing.as_ref().unwrap();
            assert_eq!(pricing.input, octet_ai::TokenRate(10_000_000));
            assert_eq!(pricing.cache_read, octet_ai::TokenRate(1_000_000));
            assert_eq!(pricing.cache_write_5m, octet_ai::TokenRate(12_500_000));
            assert_eq!(pricing.output, octet_ai::TokenRate(50_000_000));
            assert_eq!(pricing.tiers.len(), 1);
            let tier = &pricing.tiers[0];
            assert_eq!(tier.min_input_tokens, 272_001);
            assert_eq!(tier.input, Some(octet_ai::TokenRate(20_000_000)));
            assert_eq!(tier.cache_read, Some(octet_ai::TokenRate(2_000_000)));
            assert_eq!(tier.cache_write_5m, Some(octet_ai::TokenRate(25_000_000)));
            assert_eq!(tier.output, Some(octet_ai::TokenRate(75_000_000)));
        }
        assert!(!model.spec.cache.supports_long_retention);
        assert!(!model.spec.cache.send_session_id_header);
        assert_eq!(
            model.spec.cache.session_affinity_format,
            Some(octet_ai::SessionAffinityFormat::Codex)
        );
        assert_eq!(
            model.endpoint.transport,
            octet_ai::EndpointTransport::WebSocketPreferred
        );
        assert_eq!(
            model.endpoint.runtime.body_encoding,
            octet_ai::RequestBodyEncoding::Zstd
        );
        assert_eq!(
            model.endpoint.runtime.responses_profile,
            octet_ai::ResponsesRuntimeProfile::Codex
        );
    }
    let sol = catalog.resolve(&ModelId("gpt-5.6-sol".into())).unwrap();
    assert_eq!(crate::compaction::context_window(&sol), 272_000);

    // Pro is not in the fallback subscription catalog. Luna is included and
    // live account discovery can add or remove models independently of it.
    assert!(catalog.resolve(&ModelId("gpt-5.5-pro".into())).is_err());
    assert!(catalog.resolve(&ModelId("gpt-5.6-luna".into())).is_ok());
    assert_eq!(
        catalog
            .resolve(&ModelId("gpt-6-astra".into()))
            .unwrap()
            .endpoint
            .id
            .0,
        "openai",
        "the direct OpenAI route must remain distinct from codex/gpt-6-astra"
    );
}

#[test]
fn codex_astra_is_namespaced_without_a_direct_openai_model() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    write_codex_credential(&path, false, "plus");
    let store = crate::auth::codex::CredentialStore::new(path);
    // A default catalog models production after unavailable direct OpenAI
    // presets were filtered out: only usable Codex OAuth remains.
    let mut catalog = ModelCatalog::default();
    register_openai_codex(&mut catalog, store, true).unwrap();

    let astra = catalog
        .resolve(&ModelId("codex/gpt-6-astra".into()))
        .expect("namespaced Codex Astra");
    assert_eq!(astra.endpoint.id.0, crate::auth::codex::ENDPOINT_ID);
    assert!(catalog.resolve(&ModelId("gpt-6-astra".into())).is_err());
}

#[test]
fn codex_astra_fallback_is_conservative_and_retains_advertised_max() {
    let pro = crate::auth::codex::ChatGptPlan::Pro;
    let models = fallback_codex_models(Some(&pro));
    let astra = models
        .iter()
        .find(|model| model.id == "gpt-6-astra")
        .unwrap();
    assert_eq!(astra.context_window, 272_000);
    assert_eq!(astra.max_context_window, 872_000);
    assert_eq!(astra.max_output_tokens, 128_000);
    assert_eq!(astra.min_effort, octet_ai::ReasoningEffort::Low);
    assert_eq!(astra.max_effort, octet_ai::ReasoningEffort::Max);
    assert!(!astra.responses_lite);
    assert_eq!(astra.agent_delegation, None);
}

#[test]
fn codex_luna_fallback_uses_exact_effort_choices_for_auxiliary_requests() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    write_codex_credential(&path, false, "plus");
    let mut catalog = base_model_catalog(true).unwrap();
    register_openai_codex(
        &mut catalog,
        crate::auth::codex::CredentialStore::new(path),
        true,
    )
    .unwrap();

    let luna = catalog
        .resolve(&ModelId("gpt-5.6-luna".into()))
        .expect("offline Codex Luna fallback");
    assert_eq!(luna.endpoint.id.0, crate::auth::codex::ENDPOINT_ID);
    let capability = luna
        .spec
        .capabilities
        .reasoning
        .as_ref()
        .expect("Luna reasoning capability");
    let options = capability.options.as_ref().expect("exact Luna choices");
    assert_eq!(
        options.values,
        ["none", "low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(options.default, None);
    assert_eq!(capability.min_effort, octet_ai::ReasoningEffort::Low);
    assert_eq!(capability.max_effort, octet_ai::ReasoningEffort::Max);
    assert_eq!(
        default_reasoning_for_model(&luna),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low)
    );
    assert_eq!(
        octet_ai::select_auxiliary_reasoning(&luna).unwrap(),
        ReasoningConfig::Off
    );
    assert_eq!(
        capability.wire_value(&ReasoningConfig::Off),
        Some("none".to_owned())
    );
    assert_eq!(
        capability.wire_value(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max)),
        Some("max".to_owned())
    );

    // The observed correction is route-specific; generic sparse fallback keeps
    // its prior conservative range and does not gain an inferred Off choice.
    let sol = codex_fallback_reasoning_options("gpt-5.6-sol");
    assert_eq!(
        sol.values,
        ["minimal", "low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(
        codex_min_effort("gpt-5.6-sol"),
        octet_ai::ReasoningEffort::Minimal
    );
}

#[test]
fn offline_codex_registration_uses_cached_inventory_without_dynamic_capabilities() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cached-codex.json");
    write_codex_credential(&path, false, "plus");
    let store = crate::auth::codex::CredentialStore::new(&path);
    let claims = crate::auth::codex::usable_subscription_claims(&store)
        .unwrap()
        .unwrap();
    let cached = CodexDiscovery {
        claims,
        models: codex_models_from_response(
            &serde_json::json!({
                "models": [{
                    "slug": "cached-account-model",
                    "context_window": 196_000,
                    "max_output_tokens": 24_000,
                    "use_responses_lite": true,
                    "multi_agent_version": "v2",
                    "supported_reasoning_levels": ["high", "ultra"]
                }]
            }),
            Some(&crate::auth::codex::ChatGptPlan::Plus),
        )
        .unwrap(),
    };
    save_codex_model_cache(&store, &cached).unwrap();

    let mut catalog = base_model_catalog(true).unwrap();
    register_openai_codex(&mut catalog, store, true).unwrap();
    let model = catalog
        .resolve(&ModelId("cached-account-model".into()))
        .unwrap();
    assert_eq!(model.endpoint.id.0, crate::auth::codex::ENDPOINT_ID);
    assert_eq!(model.spec.limits.context_window, 196_000);
    assert!(!model.spec.capabilities.responses_lite);
    assert_eq!(model.spec.capabilities.agent_delegation, None);
    assert_eq!(
        model
            .spec
            .capabilities
            .reasoning
            .as_ref()
            .unwrap()
            .max_effort,
        octet_ai::ReasoningEffort::High
    );
    // Unsupported Ultra is removed, not silently replaced with an unadvertised
    // Max. Cached discovery must preserve the remaining exact choice set.
    assert_eq!(
        model
            .spec
            .capabilities
            .reasoning
            .as_ref()
            .unwrap()
            .options
            .as_ref()
            .unwrap()
            .values,
        ["high"]
    );

    let fallback_path = directory.path().join("fallback-codex.json");
    write_codex_credential(&fallback_path, false, "plus");
    let mut fallback_catalog = base_model_catalog(true).unwrap();
    register_openai_codex(
        &mut fallback_catalog,
        crate::auth::codex::CredentialStore::new(fallback_path),
        true,
    )
    .unwrap();
    let fallback = fallback_catalog
        .resolve(&ModelId("gpt-5.6-sol".into()))
        .unwrap();
    assert_eq!(fallback.endpoint.id.0, crate::auth::codex::ENDPOINT_ID);
    // GPT-5.6 uses OpenAI's published standard costs on the Codex route too.
    let luna = fallback_catalog
        .resolve(&ModelId("gpt-5.6-luna".into()))
        .unwrap();
    assert_eq!(luna.spec.limits.context_window, CODEX_5_6_CONTEXT_WINDOW);
    let luna_pricing = luna.spec.pricing.as_ref().expect("codex luna pricing");
    assert_eq!(luna_pricing.input, octet_ai::TokenRate(200_000));
    assert_eq!(luna_pricing.output, octet_ai::TokenRate(1_200_000));
    assert_eq!(luna_pricing.cache_read, octet_ai::TokenRate(20_000));
    assert_eq!(luna_pricing.cache_write_5m, octet_ai::TokenRate(250_000));
    assert_eq!(luna_pricing.tiers.len(), 1);
    assert_eq!(
        luna_pricing.tiers[0].input,
        Some(octet_ai::TokenRate(400_000))
    );
}

#[test]
fn codex_pro_pricing_keeps_long_context_tiers() {
    for model_id in ["gpt-5.4-pro", "gpt-5.5-pro"] {
        let pricing = crate::providers::pricing_for(&crate::providers::CODEX, model_id)
            .expect("codex pro pricing");
        assert_eq!(pricing.input, octet_ai::TokenRate(30_000_000));
        assert_eq!(pricing.output, octet_ai::TokenRate(180_000_000));
        assert_eq!(pricing.tiers.len(), 1);
        let tier = &pricing.tiers[0];
        assert_eq!(tier.min_input_tokens, 272_001);
        assert_eq!(tier.input, Some(octet_ai::TokenRate(60_000_000)));
        assert_eq!(tier.output, Some(octet_ai::TokenRate(270_000_000)));
    }
}

#[test]
fn codex_fallback_never_infers_ultra_or_delegation_from_oauth_plan() {
    let directory = tempfile::tempdir().unwrap();
    for plan in ["pro", "plus"] {
        let path = directory.path().join(format!("{plan}-codex.json"));
        write_codex_credential(&path, false, plan);
        let mut catalog = base_model_catalog(true).unwrap();
        register_openai_codex(
            &mut catalog,
            crate::auth::codex::CredentialStore::new(path),
            true,
        )
        .unwrap();
        let model = catalog.resolve(&ModelId("gpt-5.6-sol".into())).unwrap();
        assert_ne!(
            model
                .spec
                .capabilities
                .reasoning
                .as_ref()
                .unwrap()
                .max_effort,
            octet_ai::ReasoningEffort::Ultra
        );
        assert_eq!(model.spec.capabilities.agent_delegation, None);
        assert!(!model.spec.capabilities.responses_lite);
    }
}

#[test]
fn codex_spark_and_astra_are_registered_as_image_capable() {
    assert!(codex_supports_image_input("gpt-6-astra"));
    assert!(codex_supports_image_input("gpt-6-sol"));
    assert!(codex_supports_image_input("gpt-6.1-sol"));
    assert!(codex_supports_image_input("gpt-6-luna"));
    assert!(!codex_supports_image_input("gpt-6-unadvertised"));
    assert!(codex_supports_image_input("gpt-5.3-codex-spark"));
    assert!(codex_supports_image_input("gpt-5.3-codex"));
    assert!(codex_supports_image_input("gpt-5.4-mini"));
    assert!(codex_supports_image_input("gpt-5.4-pro"));
    assert!(codex_supports_image_input("gpt-5.5"));
    assert!(codex_supports_image_input("gpt-5.5-pro"));
    assert!(codex_supports_image_input("gpt-5.6-sol"));
    assert!(codex_supports_image_input("gpt-5.6-luna"));
    assert!(codex_supports_image_input("gpt-5.1-codex"));
    assert!(codex_supports_image_input("gpt-5.1-codex-mini"));
    assert!(codex_supports_image_input("gpt-5.1-codex-max"));
    assert!(codex_supports_image_input("codex-mini-latest"));
    assert!(!codex_supports_image_input("gpt-5-codex"));
}

#[test]
fn codex_catalog_query_uses_gpt6_compatible_client_and_cache_versions() {
    assert_eq!(CODEX_MODELS_CLIENT_VERSION, "0.156.1");
    assert_eq!(CODEX_MODEL_CACHE_VERSION, 9);
    let url = codex_models_url().unwrap();
    assert_eq!(url.path(), "/backend-api/codex/models");
    assert_eq!(
        url.query_pairs()
            .find(|(name, _)| name == "client_version")
            .map(|(_, value)| value.into_owned()),
        Some(CODEX_MODELS_CLIENT_VERSION.to_string())
    );
}

#[test]
fn codex_discovery_accepts_account_catalog_and_caps_live_context() {
    let body = serde_json::json!({
        "models": [
            {
                "slug": "gpt-5.6-luna",
                "context_window": 400_000,
                "max_output_tokens": 150_000,
                "use_responses_lite": true,
                "multi_agent_version": "v2",
                "supported_reasoning_levels": [
                    {"effort": "low"},
                    {"effort": "max"},
                    {"effort": "ultra"}
                ]
            },
            {"slug": "gpt-account-preview"},
            "gpt-string-preview",
            {"slug": "gpt-5.6-luna"}
        ]
    });
    let models = codex_models_from_response(&body, None).unwrap();
    assert_eq!(models.len(), 3, "duplicate slugs must be collapsed");
    let luna = models
        .iter()
        .find(|model| model.id == "gpt-5.6-luna")
        .unwrap();
    assert_eq!(luna.context_window, CODEX_5_6_CONTEXT_WINDOW);
    assert_eq!(luna.max_context_window, 400_000);
    assert_eq!(luna.max_output_tokens, 150_000);
    assert_eq!(luna.min_effort, octet_ai::ReasoningEffort::Low);
    assert_eq!(luna.max_effort, octet_ai::ReasoningEffort::Ultra);
    assert!(luna.responses_lite);
    assert_eq!(luna.agent_delegation, Some(octet_ai::AgentDelegation::V2));
    assert_eq!(
        models
            .iter()
            .find(|model| model.id == "gpt-string-preview")
            .unwrap()
            .context_window,
        CODEX_LEGACY_CONTEXT_WINDOW
    );
}

#[test]
fn codex_astra_live_object_metadata_requires_explicit_ultra_and_v2() {
    let mut body = serde_json::json!({
        "models": [{
            "slug": "gpt-6-astra",
            "minimal_client_version": "0.153.0",
            "supported_in_api": true,
            "visibility": "hide",
            "context_window": 1_050_000,
            "max_context_window": 872_000,
            "max_output_tokens": 256_000,
            "default_reasoning_level": "low",
            "supported_reasoning_levels": [
                {"effort": "low"},
                {"effort": "medium"},
                {"effort": "high"},
                {"effort": "xhigh"},
                {"effort": "max"},
                {"effort": "ultra"}
            ],
            "use_responses_lite": true,
            "multi_agent_version": "v2"
        }]
    });
    let models = codex_models_from_response(&body, None).unwrap();
    let astra = &models[0];
    assert_eq!(astra.id, "gpt-6-astra");
    assert_eq!(astra.context_window, 272_000);
    assert_eq!(astra.max_context_window, 872_000);
    assert_eq!(astra.max_output_tokens, 128_000);
    assert_eq!(astra.min_effort, octet_ai::ReasoningEffort::Low);
    assert_eq!(astra.max_effort, octet_ai::ReasoningEffort::Ultra);
    assert!(astra.responses_lite);
    assert_eq!(astra.agent_delegation, Some(octet_ai::AgentDelegation::V2));
    assert!(codex_supports_image_input(&astra.id));

    let mut lower_output_body = body.clone();
    lower_output_body["models"][0]["max_output_tokens"] = serde_json::json!(64_000);
    assert_eq!(
        codex_models_from_response(&lower_output_body, None).unwrap()[0].max_output_tokens,
        64_000
    );

    let offline = conservative_offline_codex_models(models.clone());
    let offline_astra = &offline[0];
    assert_eq!(offline_astra.context_window, 272_000);
    assert_eq!(offline_astra.max_context_window, 872_000);
    assert_eq!(offline_astra.min_effort, octet_ai::ReasoningEffort::Low);
    assert_eq!(offline_astra.max_effort, octet_ai::ReasoningEffort::Max);
    assert!(!offline_astra.responses_lite);
    assert_eq!(offline_astra.agent_delegation, None);

    let mut max_only_body = body.clone();
    let removed = max_only_body["models"][0]["supported_reasoning_levels"]
        .as_array_mut()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(removed["effort"], "ultra");
    let max_only = codex_models_from_response(&max_only_body, None).unwrap();
    let astra = &max_only[0];
    assert_eq!(astra.min_effort, octet_ai::ReasoningEffort::Low);
    assert_eq!(astra.max_effort, octet_ai::ReasoningEffort::Max);
    assert_eq!(astra.agent_delegation, Some(octet_ai::AgentDelegation::V2));

    body["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("multi_agent_version");
    let without_v2 = codex_models_from_response(&body, None).unwrap();
    let astra = &without_v2[0];
    assert_eq!(astra.min_effort, octet_ai::ReasoningEffort::Low);
    assert_eq!(astra.max_effort, octet_ai::ReasoningEffort::Max);
    assert!(astra.responses_lite);
    assert_eq!(astra.agent_delegation, None);
}

#[test]
fn codex_observed_sol_luna_inventory_preserves_exact_ids_and_oauth_routes() {
    // Model-only projection of authenticated GET /backend-api/codex/models on
    // 2026-09-23 with client_version=0.153.2 and installed codex-cli 0.154.0.
    // Both returned these 5.6 slugs, not gpt-6-sol/gpt-6-luna. No inference,
    // output limit, price, or alias equivalence was established by that GET.
    let body = serde_json::json!({"models": [
        {
            "slug": "gpt-5.6-sol",
            "display_name": "GPT-5.6-Sol",
            "context_window": 272_000,
            "max_context_window": 872_000,
            "default_reasoning_level": "low",
            "supported_reasoning_levels": [
                {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}
            ],
            "use_responses_lite": true,
            "multi_agent_version": "v2",
            "visibility": "list",
            "supported_in_api": true,
            "minimal_client_version": "0.144.0",
            "input_modalities": ["text", "image"]
        },
        {
            "slug": "gpt-5.6-luna",
            "display_name": "GPT-5.6-Luna",
            "context_window": 272_000,
            "max_context_window": 872_000,
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [
                {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                {"effort": "xhigh"}, {"effort": "max"}
            ],
            "use_responses_lite": true,
            "multi_agent_version": "v1",
            "visibility": "list",
            "supported_in_api": true,
            "minimal_client_version": "0.144.0",
            "input_modalities": ["text", "image"]
        }
    ]});
    let directory = tempfile::tempdir().unwrap();
    for plan in ["plus", "pro"] {
        let path = directory.path().join(format!("{plan}-codex.json"));
        write_codex_credential(&path, false, plan);
        let store = crate::auth::codex::CredentialStore::new(path);
        let claims = crate::auth::codex::usable_subscription_claims(&store)
            .unwrap()
            .unwrap();
        let models = codex_models_from_response(&body, claims.plan.as_ref()).unwrap();
        assert_eq!(models.len(), 2);
        for model in &models {
            let sol = model.id == "gpt-5.6-sol";
            assert!(sol || model.id == "gpt-5.6-luna");
            assert_eq!(model.default_context_window, 272_000);
            assert_eq!(model.max_context_window, 872_000);
            assert_eq!(
                model.context_window,
                if !sol && plan == "pro" {
                    372_000
                } else {
                    272_000
                }
            );
            assert_eq!(model.max_output_tokens, CODEX_MAX_OUTPUT_TOKENS);
            assert_eq!(model.min_effort, octet_ai::ReasoningEffort::Low);
            assert_eq!(
                model.reasoning_options.default.as_deref(),
                Some(if sol { "low" } else { "medium" })
            );
            let mut expected = vec!["low", "medium", "high", "xhigh", "max"];
            if sol {
                expected.push("ultra");
            }
            assert_eq!(model.reasoning_options.values, expected);
            assert_eq!(model.agent_delegation, sol.then_some(AgentDelegation::V2));
            assert!(model.responses_lite);
        }
        save_codex_model_cache(&store, &CodexDiscovery { claims, models }).unwrap();
        let mut catalog = ModelCatalog::default();
        // Registration uses an account-bound fixture cache, never the network.
        // Its conservative offline reduction must retain exact ordinary choices.
        register_openai_codex(&mut catalog, store, true).unwrap();
        for id in ["gpt-5.6-sol", "gpt-5.6-luna"] {
            let model = catalog.resolve(&ModelId(id.into())).unwrap();
            assert_eq!(model.spec.api_name, id);
            assert_eq!(model.endpoint.id.0, crate::auth::codex::ENDPOINT_ID);
            assert_eq!(
                model.endpoint.base_url.as_str(),
                crate::providers::CODEX.base_url
            );
            assert!(matches!(model.endpoint.auth, Auth::Dynamic(_)));
            assert_eq!(model.spec.protocol, Protocol::OpenAiResponses);
            let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
            assert_eq!(
                reasoning.options.as_ref().unwrap().values,
                ["low", "medium", "high", "xhigh", "max"]
            );
            assert!(!reasoning.supports(&ReasoningConfig::Off));
            assert!(!model.spec.capabilities.responses_lite);
            assert_eq!(model.spec.capabilities.agent_delegation, None);
        }
        for absent in [
            "gpt-6-sol",
            "gpt-6-luna",
            "codex/gpt-6-sol",
            "codex/gpt-6-luna",
        ] {
            assert!(catalog.resolve(&ModelId(absent.into())).is_err());
        }
    }
}

/// Discovery can fail for many reasons (network, 401, the readiness envelope,
/// a malformed body). The offline fallback must still offer every GPT-6 model
/// octet has a contract for, with the same reasoning choices as discovery.
#[test]
fn codex_offline_fallback_covers_every_known_gpt6_model() {
    for id in ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna"] {
        assert!(known_gpt_6_model(id), "{id}");
        assert!(
            crate::auth::codex::MODELS.contains(&id),
            "{id} missing from MODELS"
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    write_codex_credential(&path, false, "plus");
    let store = crate::auth::codex::CredentialStore::new(&path);
    let mut catalog = base_model_catalog(true).unwrap();
    register_openai_codex(&mut catalog, store, true).unwrap();
    for id in ["gpt-6-sol", "gpt-6-luna"] {
        let model = catalog.resolve(&ModelId(format!("codex/{id}"))).unwrap();
        let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
        let options = reasoning.options.as_ref().unwrap();
        assert_eq!(options.values, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(options.default.as_deref(), Some("medium"));
        assert!(!reasoning.supports(&ReasoningConfig::Off));
        assert_eq!(model.spec.limits.context_window, 272_000);
    }
}

#[test]
fn codex_gpt6_sol_luna_inventory_registers_exact_oauth_contracts() {
    // Model-only projection of the account inventory observed on 2026-09-23
    // with client_version=0.156.1. Both models require at least 0.155.0.
    // These OAuth choices intentionally differ from public API reasoning=none.
    let body = serde_json::json!({"models": [
        {
            "slug": "gpt-6-sol", "display_name": "GPT-6-Sol",
            "minimal_client_version": "0.155.0",
            "context_window": 272_000, "max_context_window": 872_000,
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [
                {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}
            ],
            "multi_agent_version": "v2", "use_responses_lite": true,
            "supports_reasoning_effort_updates": true,
            "input_modalities": ["text", "image"]
        },
        {
            "slug": "gpt-6-luna", "display_name": "GPT-6-Luna",
            "minimal_client_version": "0.155.0",
            "context_window": 272_000, "max_context_window": 872_000,
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [
                {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                {"effort": "xhigh"}, {"effort": "max"}
            ],
            "multi_agent_version": "v2", "use_responses_lite": true,
            "supports_reasoning_effort_updates": true,
            "input_modalities": ["text", "image"]
        }
    ]});
    let directory = tempfile::tempdir().unwrap();
    for plan in ["plus", "pro"] {
        let path = directory.path().join(format!("{plan}-codex.json"));
        write_codex_credential(&path, false, plan);
        let store = crate::auth::codex::CredentialStore::new(path);
        let claims = crate::auth::codex::usable_subscription_claims(&store)
            .unwrap()
            .unwrap();
        let models = codex_models_from_response(&body, claims.plan.as_ref()).unwrap();
        assert_eq!(models.len(), 2);
        for model in &models {
            assert_eq!(model.context_window, 272_000);
            assert_eq!(model.max_context_window, 872_000);
            assert_eq!(model.reasoning_options.default.as_deref(), Some("medium"));
            let mut efforts = vec!["low", "medium", "high", "xhigh", "max"];
            if model.id == "gpt-6-sol" {
                efforts.push("ultra");
            }
            assert_eq!(model.reasoning_options.values, efforts);
            assert_eq!(model.agent_delegation, Some(AgentDelegation::V2));
            assert!(model.responses_lite);
            assert!(model.reasoning_effort_updates);
        }
        save_codex_model_cache(&store, &CodexDiscovery { claims, models }).unwrap();
        let mut catalog = ModelCatalog::default();
        register_openai_codex(&mut catalog, store, true).unwrap();
        for id in ["gpt-6-sol", "gpt-6-luna"] {
            let model = catalog.resolve(&ModelId(format!("codex/{id}"))).unwrap();
            assert_eq!(model.spec.api_name, id);
            assert_eq!(model.endpoint.id.0, crate::auth::codex::ENDPOINT_ID);
            assert_eq!(
                model.endpoint.base_url.as_str(),
                crate::providers::CODEX.base_url
            );
            assert!(matches!(model.endpoint.auth, Auth::Dynamic(_)));
            assert_eq!(model.spec.protocol, Protocol::OpenAiResponses);
            assert!(model
                .spec
                .capabilities
                .input_modalities
                .contains(octet_ai::Modality::Image));
            let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
            assert_eq!(
                reasoning.options.as_ref().unwrap().values,
                ["low", "medium", "high", "xhigh", "max"]
            );
            assert!(!reasoning.supports(&ReasoningConfig::Off));
            assert!(!model.spec.capabilities.responses_lite);
            assert!(!model.responses_features().reasoning_effort_updates);
            assert!(!model.responses_features().async_tools);
            assert!(!model.responses_features().steering);
            assert_eq!(model.spec.capabilities.agent_delegation, None);
        }
    }
}

#[test]
fn codex_gpt_6_1_sol_priority_default_and_inventory_contract() {
    let body: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../fixtures/providers/gpt-6.1-sol.json"
    ))
    .unwrap();
    assert_eq!(body["models"][0]["priority"], 1);
    assert_eq!(body["models"][0]["minimal_client_version"], "0.153.0");
    assert_eq!(CODEX_MODELS_CLIENT_VERSION, "0.156.1");
    assert_eq!(crate::auth::codex::MODELS[0], "gpt-6.1-sol");
    assert_eq!(
        fallback_codex_models(None)[0].id,
        crate::auth::codex::MODELS[0]
    );
    for plan in [
        crate::auth::codex::ChatGptPlan::Plus,
        crate::auth::codex::ChatGptPlan::Pro,
    ] {
        let models = codex_models_from_response(&body, Some(&plan)).unwrap();
        assert_eq!(models.len(), 1);
        let sol = &models[0];
        assert_eq!(sol.id, "gpt-6.1-sol");
        assert_eq!(sol.display_name.as_deref(), Some("GPT-6.1-Sol"));
        assert_eq!(sol.context_window, 272_000);
        assert_eq!(sol.max_context_window, 872_000);
        assert_eq!(sol.max_output_tokens, 128_000);
        assert_eq!(sol.reasoning_options.default.as_deref(), Some("low"));
        assert_eq!(
            sol.reasoning_options.values,
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(sol.min_effort, octet_ai::ReasoningEffort::Low);
        assert_eq!(sol.max_effort, octet_ai::ReasoningEffort::Ultra);
        assert!(sol.responses_lite);
        assert_eq!(sol.agent_delegation, Some(AgentDelegation::V2));
        assert!(codex_supports_image_input(&sol.id));
        // An account inventory missing V2 cannot authorize Ultra even for a
        // bundled model. A sparse positive V2 inventory may use the fallback.
        let mut sparse = body.clone();
        sparse["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("supported_reasoning_levels");
        assert_eq!(
            codex_models_from_response(&sparse, Some(&plan)).unwrap()[0]
                .reasoning_options
                .values,
            sol.reasoning_options.values
        );
        sparse["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("multi_agent_version");
        let without_v2 = codex_models_from_response(&sparse, Some(&plan)).unwrap();
        assert_eq!(without_v2[0].max_effort, octet_ai::ReasoningEffort::Max);
        assert_eq!(without_v2[0].reasoning_options.values.len(), 5);
        assert_eq!(without_v2[0].agent_delegation, None);
    }

    let raw = codex_fallback_reasoning_options("gpt-6.1-sol");
    assert_eq!(raw.default.as_deref(), Some("low"));
    assert_eq!(raw.values.last().map(String::as_str), Some("ultra"));
    let fallback = &fallback_codex_models(Some(&crate::auth::codex::ChatGptPlan::Pro))[0];
    assert_eq!(fallback.context_window, 272_000);
    assert_eq!(fallback.max_context_window, 872_000);
    assert_eq!(fallback.reasoning_options.default.as_deref(), Some("low"));
    assert_eq!(
        fallback.reasoning_options.values,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(fallback.agent_delegation, None);
    assert!(!fallback.responses_lite);

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    write_codex_credential(&path, false, "pro");
    let store = crate::auth::codex::CredentialStore::new(&path);
    let claims = crate::auth::codex::usable_subscription_claims(&store)
        .unwrap()
        .unwrap();
    let models = codex_models_from_response(&body, claims.plan.as_ref()).unwrap();
    save_codex_model_cache(&store, &CodexDiscovery { claims, models }).unwrap();
    let mut catalog = base_model_catalog(true).unwrap();
    register_openai_codex(&mut catalog, store, true).unwrap();
    let codex = catalog
        .resolve(&ModelId("codex/gpt-6.1-sol".into()))
        .unwrap();
    let public = catalog.resolve(&ModelId("gpt-6.1-sol".into())).unwrap();
    assert_eq!(public.endpoint.id.0, "openai");
    assert_eq!(codex.endpoint.id.0, crate::auth::codex::ENDPOINT_ID);
    assert_eq!(codex.spec.display_name.as_deref(), Some("GPT-6.1-Sol"));
    assert_eq!(codex.spec.limits.context_window, 272_000);
    assert!(codex
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert_eq!(
        default_reasoning_for_model(&codex),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low)
    );
    assert_eq!(
        codex
            .spec
            .capabilities
            .reasoning
            .as_ref()
            .unwrap()
            .max_effort,
        octet_ai::ReasoningEffort::Max
    );
    assert_eq!(codex.spec.capabilities.agent_delegation, None);
    assert!(!codex.spec.capabilities.responses_lite);
    assert!(
        codex.spec.pricing.is_none(),
        "OAuth must not borrow the public API quote"
    );
}

#[test]
fn public_gpt6_discovery_uses_exact_contracts_without_cross_route_inference() {
    let declaration = &crate::providers::OPENAI;
    let mut catalog = metadata_fixture_catalog(declaration, declaration.base_url);
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &serde_json::json!({"data": [
            {"id":"gpt-6-astra"}, {"id":"gpt-6-sol"}, {"id":"gpt-6.1-sol"}, {"id":"gpt-6-luna"},
            {"id":"gpt-6-unverified"}
        ]}),
    )
    .unwrap();
    for (id, off) in [
        ("gpt-6-astra", false),
        ("gpt-6-sol", true),
        ("gpt-6.1-sol", false),
        ("gpt-6-luna", true),
    ] {
        let model = catalog.resolve(&ModelId(format!("openai/{id}"))).unwrap();
        let capability = model.spec.capabilities.reasoning.as_ref().unwrap();
        let mut efforts = vec!["low", "medium", "high", "xhigh", "max"];
        if off {
            efforts.insert(0, "none");
        }
        assert_eq!(capability.options.as_ref().unwrap().values, efforts);
        assert_eq!(
            capability.options.as_ref().unwrap().default.as_deref(),
            Some(if id == "gpt-6-astra" { "low" } else { "medium" })
        );
        assert_eq!(capability.supports(&ReasoningConfig::Off), off);
        assert_eq!(model.spec.limits.context_window, 1_050_000);
        assert_eq!(model.spec.limits.max_output_tokens, 128_000);
        if id == "gpt-6.1-sol" {
            assert_eq!(model.spec.display_name.as_deref(), Some("GPT-6.1-Sol"));
            assert!(
                !capability.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Minimal))
            );
        }
        assert!(model.spec.capabilities.responses_features.async_tools);
        // Model authority alone never upgrades an unqualified endpoint.
        assert_eq!(
            model.responses_features(),
            octet_ai::ResponsesFeatures::default()
        );
        let mut endpoint = (*model.endpoint).clone();
        endpoint.runtime = declaration.inventory_route().unwrap().runtime;
        let qualified = Model {
            spec: model.spec,
            endpoint: Arc::new(endpoint),
        };
        assert!(qualified.responses_features().async_tools);
        assert!(qualified.responses_features().steering);
        assert!(qualified.responses_features().reasoning_effort_updates);
        assert!(
            !qualified
                .responses_features()
                .compact_reasoning_effort_updates
        );
    }
    let unverified = catalog
        .resolve(&ModelId("openai/gpt-6-unverified".into()))
        .unwrap();
    assert!(unverified.spec.capabilities.reasoning.is_none());
    assert_eq!(unverified.spec.limits.context_window, 128_000);
    assert_eq!(
        unverified.spec.capabilities.responses_features,
        Default::default()
    );
    assert_eq!(
        crate::providers::OPENAI
            .inventory_route()
            .unwrap()
            .transport,
        EndpointTransport::WebSocketPreferred
    );
}

#[test]
fn codex_reasoning_updates_need_positive_fresh_account_metadata() {
    for assertion in [
        serde_json::Value::Null,
        serde_json::json!(false),
        serde_json::json!("true"),
        serde_json::json!(true),
    ] {
        let body = serde_json::json!({"models":[{
            "slug":"gpt-6-sol", "context_window":272_000, "max_context_window":872_000,
            "supported_reasoning_levels":["low","medium","high","xhigh","max"],
            "supports_reasoning_effort_updates": assertion,
            "use_responses_lite":true, "multi_agent_version":"v2"
        }]});
        let models = codex_models_from_response(&body, None).unwrap();
        assert_eq!(models[0].reasoning_effort_updates, assertion == true);
        assert!(models[0].responses_lite);
        assert_eq!(models[0].agent_delegation, Some(AgentDelegation::V2));
        let offline = conservative_offline_codex_models(models);
        assert!(!offline[0].reasoning_effort_updates);
    }
    assert!(fallback_codex_models(None)
        .iter()
        .all(|model| !model.reasoning_effort_updates));
    let endpoint = crate::providers::CODEX
        .inventory_route()
        .unwrap()
        .runtime
        .responses_features;
    assert!(endpoint.reasoning_effort_updates);
    assert!(
        !endpoint.async_tools && !endpoint.steering && !endpoint.compact_reasoning_effort_updates
    );
}

#[test]
fn gpt6_public_prices_do_not_invent_subscription_or_alias_rates() {
    for (id, input) in [("gpt-6-sol", 2_000_000), ("gpt-6-luna", 100_000)] {
        let public = crate::providers::pricing_for(&crate::providers::OPENAI, id).unwrap();
        assert_eq!(public.input.0, input);
        assert_eq!(public.output.0, input * 5);
        assert_eq!(public.cache_read.0, input / 10);
        assert_eq!(public.cache_write_5m.0, input * 5 / 4);
        assert_eq!(public.tiers[0].min_input_tokens, 272_001);
        assert_eq!(public.tiers[0].input.unwrap().0, input * 2);
        assert_eq!(public.tiers[0].output.unwrap().0, input * 15 / 2);
        assert!(crate::providers::pricing_for(&crate::providers::CODEX, id).is_none());
        assert!(crate::providers::pricing_for(
            &crate::providers::OPENAI,
            &format!("{id}-unverified")
        )
        .is_none());
    }
    let sol_61 = crate::providers::pricing_for(&crate::providers::OPENAI, "gpt-6.1-sol").unwrap();
    assert_eq!(sol_61.input, TokenRate(2_000_000));
    assert_eq!(sol_61.output, TokenRate(10_000_000));
    assert_eq!(sol_61.cache_read, TokenRate(100_000));
    assert_eq!(sol_61.cache_write_5m, TokenRate(2_500_000));
    assert_eq!(sol_61.tiers.len(), 1);
    assert_eq!(sol_61.tiers[0].min_input_tokens, 272_001);
    assert_eq!(sol_61.tiers[0].input, Some(TokenRate(4_000_000)));
    assert_eq!(sol_61.tiers[0].output, Some(TokenRate(15_000_000)));
    assert_eq!(sol_61.tiers[0].cache_read, Some(TokenRate(200_000)));
    assert_eq!(sol_61.tiers[0].cache_write_5m, Some(TokenRate(5_000_000)));
    assert!(crate::providers::pricing_for(&crate::providers::CODEX, "gpt-6.1-sol").is_none());
    assert!(
        crate::providers::pricing_for(&crate::providers::OPENAI, "gpt-6.1-sol-unverified")
            .is_none()
    );
    // Reviewed subscription prices still apply to explicitly allowlisted models,
    // but even a known public API quote must not price an unreviewed OAuth route.
    let astra = crate::providers::pricing_for(&crate::providers::CODEX, "gpt-6-astra").unwrap();
    assert_eq!(astra.input, TokenRate(10_000_000));
    assert_eq!(astra.output, TokenRate(50_000_000));
    assert!(crate::providers::pricing_for(&crate::providers::OPENAI, "gpt-4o-mini").is_some());
    assert!(crate::providers::pricing_for(&crate::providers::CODEX, "gpt-4o-mini").is_none());
}

#[test]
fn session_resume_uses_effective_reasoning_update_not_the_pinned_baseline() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("reasoning.jsonl")).unwrap();
    let low = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low);
    let high = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
    let model = ModelId("codex/gpt-6-sol".into());
    append_config_if_changed(&mut session, None, &model, &low, ReasoningMode::Standard).unwrap();
    session
        .append(EntryValue::ResponsesReasoning {
            endpoint: EndpointId(crate::auth::codex::ENDPOINT_ID.into()),
            model: model.clone(),
            baseline: low,
            update: Some(octet_ai::ResponsesConfigurationUpdate {
                reasoning: high.clone(),
            }),
        })
        .unwrap();
    let persisted = persisted_session_config(&session).unwrap();
    assert_eq!(persisted.model, Some(model));
    assert_eq!(persisted.reasoning, Some(high));
    // A later explicit selection wins over an older route's cache marker.
    append_config_if_changed(
        &mut session,
        None,
        &ModelId("gpt-4o-mini".into()),
        &ReasoningConfig::Off,
        ReasoningMode::Standard,
    )
    .unwrap();
    assert_eq!(
        persisted_session_config(&session).unwrap().reasoning,
        Some(ReasoningConfig::Off)
    );
}

#[test]
fn gpt6_resume_and_idle_rebuild_honor_explicit_effort_without_replacing_baseline() {
    let directory = tempfile::tempdir().unwrap();
    let low = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low);
    let high = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
    let max = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max);
    let mut process_config = config(directory.path(), Some("gpt-6-sol"));
    process_config.resume = ResumeSelector::Continue;
    process_config.reasoning = Some(max.clone());
    process_config.reasoning_explicit = true;
    let boot = bootstrap(process_config).unwrap();
    let path = boot.sessions.new_path("2026-09-23T00-00-00Z");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut session = Session::create(&path).unwrap();
    append_config_if_changed(
        &mut session,
        None,
        &ModelId("gpt-6-sol".into()),
        &low,
        ReasoningMode::Standard,
    )
    .unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("resumable task".into())],
            },
        )))
        .unwrap();
    session
        .append(EntryValue::ResponsesReasoning {
            endpoint: EndpointId("openai".into()),
            model: ModelId("gpt-6-sol".into()),
            baseline: low.clone(),
            update: Some(octet_ai::ResponsesConfigurationUpdate {
                reasoning: high.clone(),
            }),
        })
        .unwrap();
    drop(session);
    let launch = resolve_launch_print(&boot, "unused").unwrap();
    assert_eq!(launch.reasoning, max);
    let app = build_app(boot, launch, "system".into()).unwrap();
    assert_eq!(app.reasoning, max);
    assert_eq!(app.agent.reasoning(), &max);
    assert_eq!(
        app.agent
            .session()
            .responses_reasoning(&EndpointId("openai".into()), &ModelId("gpt-6-sol".into()))
            .unwrap(),
        Some((low.clone(), max))
    );
    let app = rebuild_app(app, None, Some(high.clone()), None, None).unwrap();
    assert_eq!(app.reasoning, high);
    assert_eq!(app.agent.reasoning(), &high);
    assert_eq!(
        app.agent
            .session()
            .responses_reasoning(&EndpointId("openai".into()), &ModelId("gpt-6-sol".into()))
            .unwrap(),
        Some((low, high))
    );
}

#[test]
fn codex_fresh_pre_gpt6_cache_refreshes_once_before_use() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    write_codex_credential(&path, false, "plus");
    let store = crate::auth::codex::CredentialStore::new(path);
    let claims = crate::auth::codex::usable_subscription_claims(&store)
        .unwrap()
        .unwrap();
    let mut body = serde_json::json!({"models": [{
        "slug": "gpt-5.6-sol", "context_window": 272_000,
        "max_context_window": 872_000, "default_reasoning_level": "medium",
        "supported_reasoning_levels": ["low", "medium", "high", "xhigh", "max"]
    }]});
    let old_cache = CodexModelCache {
        version: 8,
        account_id: claims.account_id.clone(),
        plan: codex_plan_cache_key(&claims).map(str::to_owned),
        models: codex_models_from_response(&body, claims.plan.as_ref()).unwrap(),
    };
    store
        .save_model_cache(&serde_json::to_vec(&old_cache).unwrap())
        .unwrap();
    assert!(load_codex_model_cache(&store, &claims).unwrap().is_none());
    let (offline, source) = codex_inventory_models(&store, &claims, true, false, |_| {
        panic!("offline must not refresh the old inventory")
    });
    assert_eq!(source, CodexInventorySource::ConservativeFallback);
    // A model newer than the checked-in fallback only arrives with a refresh.
    assert!(!offline.iter().any(|m| m.id == "gpt-6-future"));

    body["models"][0]["slug"] = serde_json::json!("gpt-6-future");
    let live = CodexDiscovery {
        claims: claims.clone(),
        models: codex_models_from_response(&body, claims.plan.as_ref()).unwrap(),
    };
    let requests = std::cell::Cell::new(0);
    let (models, source) = codex_inventory_models(&store, &claims, false, false, |_| {
        requests.set(requests.get() + 1);
        Ok(live)
    });
    assert_eq!(source, CodexInventorySource::OnlineDiscovery);
    assert_eq!(requests.get(), 1);
    assert_eq!(models[0].id, "gpt-6-future");
    assert_eq!(
        load_codex_model_cache(&store, &claims).unwrap(),
        Some(models.clone())
    );
    let (cached, source) = codex_inventory_models(&store, &claims, false, false, |_| {
        panic!("current inventory must not refresh again")
    });
    assert_eq!(source, CodexInventorySource::FreshCache);
    assert_eq!(cached, models);
}

#[test]
fn codex_live_inventory_does_not_inject_unadvertised_astra() {
    let models = codex_models_from_response(
        &serde_json::json!({"models": [{"slug": "gpt-5.6-sol"}]}),
        None,
    )
    .unwrap();
    assert!(models.iter().all(|model| model.id != "gpt-6-astra"));
}

#[test]
fn codex_malformed_exact_choices_fail_closed_and_no_v2_removes_only_ultra() {
    for levels in [
        serde_json::json!(["ultra", {"effort":42}]),
        serde_json::json!(["low", "low"]),
        serde_json::json!(["ultra"]),
    ] {
        assert!(codex_models_from_response(&serde_json::json!({"models":[{"slug":"gpt-5.6-test", "supported_reasoning_levels":levels}]}), None).is_err());
    }
    let models = codex_models_from_response(&serde_json::json!({"models":[{"slug":"gpt-5.6-no-v2", "supported_reasoning_levels":["high", "ultra"]}]}), None).unwrap();
    assert_eq!(models[0].max_effort, octet_ai::ReasoningEffort::High);
    assert_eq!(models[0].reasoning_options.values, ["high"]);
    assert_eq!(models[0].agent_delegation, None);
}

/// Failing closed is per model: an entry with unusable reasoning metadata is
/// left out and named, while the rest of the account's inventory survives.
#[test]
fn codex_malformed_entry_is_left_out_without_discarding_the_inventory() {
    let inventory = codex_inventory_from_response(
        &serde_json::json!({"models": [
            {"slug": "gpt-5.6-broken", "supported_reasoning_levels": ["low", "low"]},
            {"slug": "gpt-5.6-empty", "supported_reasoning_levels": ["ultra"]},
            {"slug": "gpt-5.6-sol"}
        ]}),
        None,
    )
    .unwrap();
    let ids = inventory
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["gpt-5.6-sol"]);
    assert_eq!(inventory.skipped.len(), 2, "{:?}", inventory.skipped);
    assert!(inventory.skipped[0].starts_with("gpt-5.6-broken: "));
    assert_eq!(
        inventory.skipped[1],
        "gpt-5.6-empty: no usable reasoning choices"
    );
}

#[test]
fn codex_discovery_caps_default_and_max_plan_windows_at_272k() {
    let body = serde_json::json!({
        "models": [{
            "slug": "gpt-5.4",
            "context_window": 272_000,
            "max_context_window": 1_000_000
        }]
    });
    let plus = crate::auth::codex::ChatGptPlan::Plus;
    let pro = crate::auth::codex::ChatGptPlan::Pro;
    let pro_lite = crate::auth::codex::ChatGptPlan::ProLite;

    assert_eq!(
        codex_models_from_response(&body, Some(&plus)).unwrap()[0].context_window,
        272_000
    );
    assert_eq!(
        codex_models_from_response(&body, Some(&pro)).unwrap()[0].context_window,
        272_000
    );
    assert_eq!(
        codex_models_from_response(&body, Some(&pro_lite)).unwrap()[0].context_window,
        272_000
    );

    let smaller_body = serde_json::json!({
        "models": [{
            "slug": "gpt-small-window",
            "context_window": 128_000,
            "max_context_window": 200_000
        }]
    });
    assert_eq!(
        codex_models_from_response(&smaller_body, Some(&plus)).unwrap()[0].context_window,
        128_000
    );
    assert_eq!(
        codex_models_from_response(&smaller_body, Some(&pro)).unwrap()[0].context_window,
        200_000
    );
}

#[test]
fn codex_discovery_uses_372k_window_for_luna() {
    let body = serde_json::json!({
        "models": [{
            "slug": "gpt-5.6-luna",
            "context_window": 400_000,
            "max_context_window": 1_000_000
        }]
    });
    let plus = crate::auth::codex::ChatGptPlan::Plus;
    let pro = crate::auth::codex::ChatGptPlan::Pro;

    assert_eq!(
        codex_models_from_response(&body, Some(&plus)).unwrap()[0].context_window,
        CODEX_5_6_CONTEXT_WINDOW
    );
    assert_eq!(
        codex_models_from_response(&body, Some(&pro)).unwrap()[0].context_window,
        CODEX_5_6_CONTEXT_WINDOW
    );
}

#[test]
fn codex_model_cache_is_scoped_to_account_and_plan() {
    let directory = tempfile::tempdir().unwrap();
    let store = crate::auth::codex::CredentialStore::new(directory.path().join("codex.json"));
    let plus = crate::auth::codex::ChatGptPlan::Plus;
    let claims = crate::auth::codex::SubscriptionClaims {
        account_id: "acct-a".into(),
        plan: Some(plus.clone()),
    };
    let body = serde_json::json!({
        "models": [{"slug": "gpt-5.6-sol", "context_window": 272_000}]
    });
    let discovery = CodexDiscovery {
        models: codex_models_from_response(&body, Some(&plus)).unwrap(),
        claims: claims.clone(),
    };
    save_codex_model_cache(&store, &discovery).unwrap();
    assert_eq!(
        load_codex_model_cache(&store, &claims).unwrap(),
        Some(discovery.models)
    );

    let upgraded = crate::auth::codex::SubscriptionClaims {
        account_id: "acct-a".into(),
        plan: Some(crate::auth::codex::ChatGptPlan::Pro),
    };
    assert!(load_codex_model_cache(&store, &upgraded).unwrap().is_none());
    let other_account = crate::auth::codex::SubscriptionClaims {
        account_id: "acct-b".into(),
        plan: Some(plus),
    };
    assert!(load_codex_model_cache(&store, &other_account)
        .unwrap()
        .is_none());
}

#[test]
fn codex_astra_cache_caps_output_and_honors_lower_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let store = crate::auth::codex::CredentialStore::new(directory.path().join("codex.json"));
    let plan = crate::auth::codex::ChatGptPlan::Plus;
    let claims = crate::auth::codex::SubscriptionClaims {
        account_id: "acct-a".into(),
        plan: Some(plan),
    };
    let cache = |max_output_tokens| CodexModelCache {
        version: CODEX_MODEL_CACHE_VERSION,
        account_id: claims.account_id.clone(),
        plan: codex_plan_cache_key(&claims).map(str::to_owned),
        models: vec![DiscoveredCodexModel {
            display_name: None,
            reasoning_options: codex_fallback_reasoning_options("gpt-6-astra"),
            id: "gpt-6-astra".into(),
            context_window: 272_000,
            default_context_window: 272_000,
            max_context_window: 872_000,
            max_output_tokens,
            min_effort: octet_ai::ReasoningEffort::Low,
            max_effort: octet_ai::ReasoningEffort::Max,
            reasoning_effort_updates: false,
            responses_lite: false,
            agent_delegation: None,
        }],
    };

    store
        .save_model_cache(&serde_json::to_vec(&cache(256_000)).unwrap())
        .unwrap();
    assert_eq!(
        load_codex_model_cache(&store, &claims).unwrap().unwrap()[0].max_output_tokens,
        CODEX_MAX_OUTPUT_TOKENS
    );

    store
        .save_model_cache(&serde_json::to_vec(&cache(64_000)).unwrap())
        .unwrap();
    assert_eq!(
        load_codex_model_cache(&store, &claims).unwrap().unwrap()[0].max_output_tokens,
        64_000
    );
}

#[test]
fn codex_model_cache_fails_closed_when_stale_future_dated_or_incomplete() {
    let directory = tempfile::tempdir().unwrap();
    let plus = crate::auth::codex::ChatGptPlan::Plus;
    let claims = crate::auth::codex::SubscriptionClaims {
        account_id: "acct-a".into(),
        plan: Some(plus.clone()),
    };
    let models = codex_models_from_response(
        &serde_json::json!({
            "models": [{
                "slug": "gpt-5.6-sol",
                "context_window": 272_000,
                "max_output_tokens": 128_000,
                "supported_reasoning_levels": ["high", "ultra"],
                "use_responses_lite": true,
                "multi_agent_version": "v2"
            }]
        }),
        Some(&plus),
    )
    .unwrap();
    let valid = serde_json::to_vec(&CodexModelCache {
        version: CODEX_MODEL_CACHE_VERSION,
        account_id: claims.account_id.clone(),
        plan: codex_plan_cache_key(&claims).map(str::to_owned),
        models,
    })
    .unwrap();
    let cache_path = |credential_path: &std::path::Path| {
        let stem = credential_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap();
        credential_path.with_file_name(format!("{stem}-models.json"))
    };

    for (name, modified) in [
        (
            "stale",
            std::time::SystemTime::now()
                .checked_sub(CODEX_MODEL_CACHE_REFRESH_INTERVAL + Duration::from_secs(1))
                .unwrap(),
        ),
        (
            "future",
            std::time::SystemTime::now() + Duration::from_secs(60),
        ),
    ] {
        let credential_path = directory.path().join(format!("{name}.json"));
        let store = crate::auth::codex::CredentialStore::new(&credential_path);
        store.save_model_cache(&valid).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(cache_path(&credential_path))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(load_codex_model_cache(&store, &claims).unwrap().is_none());
    }

    let malformed_path = directory.path().join("malformed.json");
    let malformed = crate::auth::codex::CredentialStore::new(&malformed_path);
    malformed.save_model_cache(b"{").unwrap();
    assert!(load_codex_model_cache(&malformed, &claims).is_err());

    let valid_value: serde_json::Value = serde_json::from_slice(&valid).unwrap();

    let mut prior_schema = valid_value.clone();
    // Schema 3 predates the Astra-compatible client version and must not hide
    // newly eligible models for the cache refresh interval.
    prior_schema["version"] = serde_json::json!(3);
    let prior_schema_store =
        crate::auth::codex::CredentialStore::new(directory.path().join("prior-schema.json"));
    prior_schema_store
        .save_model_cache(&serde_json::to_vec(&prior_schema).unwrap())
        .unwrap();
    assert!(load_codex_model_cache(&prior_schema_store, &claims)
        .unwrap()
        .is_none());

    let mut cases = Vec::new();

    let mut missing_delegation = valid_value.clone();
    missing_delegation["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("agent_delegation");
    cases.push(("missing-delegation", missing_delegation));

    let mut missing_responses_lite = valid_value.clone();
    missing_responses_lite["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("responses_lite");
    cases.push(("missing-responses-lite", missing_responses_lite));

    let mut duplicate = valid_value.clone();
    let duplicate_model = duplicate["models"][0].clone();
    duplicate["models"]
        .as_array_mut()
        .unwrap()
        .push(duplicate_model);
    cases.push(("duplicate", duplicate));

    let mut empty_id = valid_value.clone();
    empty_id["models"][0]["id"] = serde_json::json!("");
    cases.push(("empty-id", empty_id));

    let mut inconsistent_limits = valid_value.clone();
    inconsistent_limits["models"][0]["max_output_tokens"] = serde_json::json!(300_000);
    cases.push(("inconsistent-limits", inconsistent_limits));

    let mut ultra_without_delegation = valid_value.clone();
    ultra_without_delegation["models"][0]["agent_delegation"] = serde_json::Value::Null;
    cases.push(("ultra-without-delegation", ultra_without_delegation));

    let mut invalid_effort_range = valid_value;
    invalid_effort_range["models"][0]["min_effort"] = serde_json::json!("ultra");
    invalid_effort_range["models"][0]["max_effort"] = serde_json::json!("high");
    cases.push(("invalid-effort-range", invalid_effort_range));

    for (name, contents) in cases {
        let path = directory.path().join(format!("{name}.json"));
        let store = crate::auth::codex::CredentialStore::new(path);
        store
            .save_model_cache(&serde_json::to_vec(&contents).unwrap())
            .unwrap();
        assert!(load_codex_model_cache(&store, &claims).is_err(), "{name}");
    }
}
