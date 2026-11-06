use super::support::*;
use super::*;
use octet_ai::{Modality, ReasoningEffort as E};

fn codex_refresh_entry(id: &str) -> serde_json::Value {
    serde_json::json!({
        "slug": id,
        "input_modalities": ["text", "image", "pdf", "audio"],
        "output_modalities": ["text", "audio", "image"],
        "supported_reasoning_levels": [{"effort":"low"}, {"effort":"high"}, {"effort":"max"}],
        "default_reasoning_level": "high"
    })
}

#[test]
fn codex_refresh_keeps_modalities_and_exact_thinking_without_a_model_patch() {
    for id in ["gpt-6.1-terra", "future-account-model"] {
        assert!(!codex_supports_image_input(id));
        let models = codex_models_from_response(
            &serde_json::json!({"models":[codex_refresh_entry(id)]}),
            None,
        )
        .unwrap();
        let model = &models[0];
        for modality in [Modality::Image, Modality::Pdf, Modality::Audio] {
            assert!(
                model.input_modalities.contains(modality),
                "{id}: {modality:?}"
            );
        }
        assert!(model.output_modalities.contains(Modality::Audio));
        assert!(model.output_modalities.contains(Modality::Image));
        let capabilities = codex_inference_capabilities(model);
        assert!(capabilities.input_modalities.contains(Modality::Image));
        assert!(capabilities.input_modalities.contains(Modality::Pdf));
        assert!(!capabilities.input_modalities.contains(Modality::Audio));
        assert_eq!(capabilities.output_modalities, ModalitySet::none());
        let reasoning = capabilities.reasoning.unwrap();
        let options = reasoning.options.as_ref().unwrap();
        assert_eq!(options.values, ["low", "high", "max"]);
        assert_eq!(options.default.as_deref(), Some("high"));
        assert!(!reasoning.supports(&ReasoningConfig::Off));
        assert!(!reasoning.supports(&ReasoningConfig::Effort(E::Medium)));
        assert!(!reasoning.supports(&ReasoningConfig::Effort(E::Xhigh)));
        assert!(reasoning.supports(&ReasoningConfig::Effort(E::Max)));
    }
}

#[test]
fn codex_explicit_input_assertions_override_the_historical_family_fallback() {
    for assertion in [
        serde_json::json!({"input_modalities":["text"]}),
        serde_json::json!({"input_modalities":null}),
        serde_json::json!({"input_modalities":false}),
        serde_json::json!({"input_modalities":"image"}),
        serde_json::json!({"input_modalities":["image",42]}),
        serde_json::json!({"architecture":null}),
        serde_json::json!({"capabilities":{"vision":false}}),
        serde_json::json!({"modalities":{"input":["text"],"output":["image"]}}),
        serde_json::json!({"vision":false,"input_modalities":["image"]}),
        serde_json::json!({"audio":false,"input_modalities":["audio"]}),
    ] {
        assert_eq!(
            codex_input_modalities(&assertion, "gpt-6-astra"),
            ModalitySet::none(),
            "{assertion}"
        );
    }
    assert!(
        codex_input_modalities(&serde_json::json!({}), "gpt-6-astra").contains(Modality::Image)
    );
    assert!(codex_input_modalities(
        &serde_json::json!({"architecture":{"output_modalities":["text"]}}),
        "gpt-6-astra"
    )
    .contains(Modality::Image));
    assert_eq!(
        codex_input_modalities(&serde_json::json!({}), "future-account-model"),
        ModalitySet::none()
    );
}

#[test]
fn codex_refresh_round_trips_modalities_and_exact_thinking_through_cache_and_offline_reduction() {
    let directory = tempfile::tempdir().unwrap();
    let store = crate::auth::codex::CredentialStore::new(directory.path().join("codex.json"));
    let claims = crate::auth::codex::SubscriptionClaims {
        account_id: "synthetic-account".into(),
        plan: Some(crate::auth::codex::ChatGptPlan::Plus),
    };
    let mut entry = codex_refresh_entry("future-account-model");
    entry["supported_reasoning_levels"] = serde_json::json!(["none", "low", "max", "ultra"]);
    entry["default_reasoning_level"] = serde_json::json!("max");
    entry["multi_agent_version"] = serde_json::json!("v2");
    entry["use_responses_lite"] = serde_json::json!(true);
    entry["supports_reasoning_effort_updates"] = serde_json::json!(true);
    let discovery = CodexDiscovery {
        models: codex_models_from_response(&serde_json::json!({"models":[entry]}), None).unwrap(),
        claims: claims.clone(),
    };
    save_codex_model_cache(&store, &discovery).unwrap();
    assert_eq!(
        load_codex_model_cache(&store, &claims).unwrap(),
        Some(discovery.models.clone())
    );
    let (cached, source) = codex_inventory_models(&store, &claims, false, false, |_| {
        panic!("fresh cache must not fetch inventory")
    });
    assert_eq!(source, CodexInventorySource::FreshCache);
    assert_eq!(cached, discovery.models);

    let (offline, source) = codex_inventory_models(&store, &claims, true, false, |_| {
        panic!("offline must not fetch inventory")
    });
    assert_eq!(source, CodexInventorySource::FreshCache);
    assert_eq!(offline[0].input_modalities, cached[0].input_modalities);
    assert_eq!(offline[0].output_modalities, cached[0].output_modalities);
    assert_eq!(offline[0].reasoning_options.values, ["none", "low", "max"]);
    assert_eq!(offline[0].reasoning_options.default.as_deref(), Some("max"));
    assert!(!offline[0].responses_lite);
    assert!(!offline[0].reasoning_effort_updates);
    assert_eq!(offline[0].agent_delegation, None);
    assert!(codex_inference_capabilities(&offline[0])
        .input_modalities
        .contains(Modality::Image));

    let mut old = serde_json::to_value(CodexModelCache {
        version: 10,
        account_id: claims.account_id.clone(),
        plan: codex_plan_cache_key(&claims).map(str::to_owned),
        models: cached,
    })
    .unwrap();
    let model = old["models"][0].as_object_mut().unwrap();
    model.remove("input_modalities");
    model.remove("output_modalities");
    store
        .save_model_cache(&serde_json::to_vec(&old).unwrap())
        .unwrap();
    let requests = std::cell::Cell::new(0);
    let (refreshed, source) = codex_inventory_models(&store, &claims, false, false, |_| {
        requests.set(requests.get() + 1);
        Ok(discovery.clone())
    });
    assert_eq!(requests.get(), 1);
    assert_eq!(source, CodexInventorySource::OnlineDiscovery);
    assert_eq!(refreshed, discovery.models);
}

#[test]
fn codex_unknown_reasoning_assertions_never_receive_guessed_thinking_levels() {
    let inventory = codex_inventory_from_response(
        &serde_json::json!({"models":[
            {"slug":"unknown-null", "reasoning":null},
            {"slug":"unknown-control", "reasoning":{"control":"future-control"}},
            {"slug":"explicit-off", "reasoning":false},
            codex_refresh_entry("valid-account-model")
        ]}),
        None,
    )
    .unwrap();
    assert_eq!(inventory.skipped.len(), 2);
    assert_eq!(inventory.models.len(), 2);
    let off = inventory
        .models
        .iter()
        .find(|model| model.id == "explicit-off")
        .unwrap();
    assert_eq!(off.reasoning_options.values, ["none"]);
    assert_eq!(off.reasoning_options.default.as_deref(), Some("none"));
}

#[test]
fn inventory_modality_envelopes_keep_input_and_output_independent() {
    for entry in [
        serde_json::json!({"input_modalities":["text","image","pdf"], "output_modalities":["text","audio"]}),
        serde_json::json!({"architecture":{"input_modalities":["text","image","file"], "output_modalities":["text","audio"]}}),
        serde_json::json!({"modalities":{"input":["text","image","pdf"], "output":["text","audio"]}}),
        serde_json::json!({"provider":{"capabilities":{"input_modalities":["text","image","pdf"], "output_modalities":["text","audio"]}}}),
        serde_json::json!({"top_provider":{"modalities":{"input":["text","image","pdf"], "output":["text","audio"]}}}),
    ] {
        let input = input_modalities_from_entry(&entry);
        assert!(input.contains(Modality::Image), "{entry}");
        assert!(input.contains(Modality::Pdf), "{entry}");
        assert!(!input.contains(Modality::Audio), "{entry}");
        let output = output_modalities_from_entry(&entry);
        assert!(output.contains(Modality::Audio), "{entry}");
        assert!(!output.contains(Modality::Image), "{entry}");
        assert!(!output.contains(Modality::Pdf), "{entry}");
    }
    let legacy = serde_json::json!({"modalities":["image"]});
    assert!(input_modalities_from_entry(&legacy).contains(Modality::Image));
    assert_eq!(output_modalities_from_entry(&legacy), ModalitySet::none());
    assert_eq!(
        input_modalities_from_entry(&serde_json::json!({
            "input_modalities":["not-image","unknown-audio","video"]
        })),
        ModalitySet::none()
    );
}

#[test]
fn discovered_anthropic_modalities_follow_the_inventory_and_codec() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "anthropic")
        .unwrap();
    let mut catalog = metadata_fixture_catalog(declaration, declaration.base_url);
    register_anthropic_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &serde_json::json!({"data":[{
            "id":"synthetic-document-model", "input_modalities":["text","image","pdf","audio"],
            "output_modalities":["text","audio"], "reasoning":false
        }]}),
    )
    .unwrap();
    let model = catalog
        .resolve(&ModelId("anthropic/synthetic-document-model".into()))
        .unwrap();
    let capabilities = &model.spec.capabilities;
    assert!(capabilities.input_modalities.contains(Modality::Image));
    assert!(capabilities.input_modalities.contains(Modality::Pdf));
    assert!(!capabilities.input_modalities.contains(Modality::Audio));
    assert_eq!(capabilities.output_modalities, ModalitySet::none());
}

#[test]
fn discovered_chat_audio_output_is_not_lost_during_catalog_projection() {
    let declaration = &crate::providers::DEEPSEEK;
    let body = serde_json::json!({"data":[{
        "id":"synthetic-audio-model", "input_modalities":["text"],
        "output_modalities":["text","audio"], "reasoning":false
    }]});
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &body,
    )
    .unwrap();
    let model = catalog
        .resolve(&ModelId("deepseek/synthetic-audio-model".into()))
        .unwrap();
    assert!(model
        .spec
        .capabilities
        .output_modalities
        .contains(Modality::Audio));
    assert!(!model
        .spec
        .capabilities
        .input_modalities
        .contains(Modality::Audio));

    let mut specialized = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_deepseek_models_from_response(&mut specialized, declaration, &body).unwrap();
    assert!(specialized
        .resolve(&ModelId("deepseek/synthetic-audio-model".into()))
        .unwrap()
        .spec
        .capabilities
        .output_modalities
        .contains(Modality::Audio));

    let mut router = ModelCatalog::default();
    register_test_openrouter_endpoint(&mut router);
    register_openrouter_models_from_response(
        &mut router,
        openrouter_declaration(),
        &serde_json::json!({"data":[{
            "id":"synthetic/audio-model", "context_length":8192,
            "top_provider":{"max_completion_tokens":2048},
            "architecture":{"input_modalities":["text"], "output_modalities":["text","audio"]}
        }]}),
    )
    .unwrap();
    let model = router
        .resolve(&ModelId("openrouter/synthetic/audio-model".into()))
        .unwrap();
    assert!(model
        .spec
        .capabilities
        .output_modalities
        .contains(Modality::Audio));
}
