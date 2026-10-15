//! Custom model inventory
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::*;

#[test]
fn opencode_discovery_infers_supported_protocols_and_routes_gemini() {
    let preset = &crate::providers::OPENCODE;
    let binding = |model_id| {
        discovered_preset_binding(preset, model_id).map(|route| (route.endpoint_id, route.protocol))
    };
    assert_eq!(
        binding("gpt-future"),
        Some(("opencode", Protocol::OpenAiResponses))
    );
    assert_eq!(
        binding("claude-future"),
        Some((OPENCODE_ANTHROPIC_ENDPOINT_ID, Protocol::AnthropicMessages))
    );
    assert_eq!(
        binding("qwen3.7-plus"),
        Some((OPENCODE_ANTHROPIC_ENDPOINT_ID, Protocol::AnthropicMessages))
    );
    assert_eq!(
        binding("qwen3.7-instruct"),
        Some(("opencode", Protocol::OpenAiChat))
    );
    assert_eq!(
        binding("gemini-future"),
        Some(("opencode-google", Protocol::GoogleGenerativeAi))
    );
    assert_eq!(
        binding("kimi-future"),
        Some(("opencode", Protocol::OpenAiChat))
    );
}

#[test]
fn opencode_go_discovery_routes_gemini_to_the_google_endpoint() {
    let binding = |model_id| {
        discovered_preset_binding(&crate::providers::OPENCODE_GO, model_id)
            .map(|route| (route.endpoint_id, route.protocol))
    };
    assert_eq!(
        binding("gemini-4-argon-contributor"),
        Some(("opencode-go-google", Protocol::GoogleGenerativeAi))
    );
    assert_eq!(
        binding("gemini-future"),
        Some(("opencode-go-google", Protocol::GoogleGenerativeAi))
    );
    // OpenCode Go serves Gemini only under its inference base; the zen/go base
    // answers that path with a 404 page while serving the other protocols.
    let route = discovered_preset_binding(&crate::providers::OPENCODE_GO, "gemini-future").unwrap();
    let base = crate::providers::OPENCODE_GO
        .resolved_route_base_url(route)
        .unwrap();
    assert_eq!(base.as_str(), "https://opencode.ai/inference/go/v1/");
    assert_eq!(
        crate::providers::OPENCODE_GO
            .resolved_route_base_url(
                discovered_preset_binding(&crate::providers::OPENCODE_GO, "glm-5.2").unwrap()
            )
            .unwrap()
            .as_str(),
        crate::providers::OPENCODE_GO.base_url
    );
}

#[test]
fn every_opencode_go_catalog_model_routes_by_family_and_keeps_its_declared_efforts() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../octet-ai/models/models-dev-capabilities.json");
    let catalog: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut checked = 0;
    for (key, entry) in catalog.as_object().unwrap() {
        let Some(model) = key.strip_prefix("opencode-go/") else {
            continue;
        };
        checked += 1;
        if model == "qwen3.7-max" {
            // Excluded by design: Qwen over Chat needs a distinct thinking format.
            assert!(discovered_preset_binding(&crate::providers::OPENCODE_GO, model).is_none());
            continue;
        }
        let route = discovered_preset_binding(&crate::providers::OPENCODE_GO, model)
            .unwrap_or_else(|| panic!("{model} has no OpenCode Go route"));
        let expected = if model.starts_with("gemini-") {
            Protocol::GoogleGenerativeAi
        } else if model.starts_with("claude-") || model.starts_with("qwen3.") {
            Protocol::AnthropicMessages
        } else if model.starts_with("gpt-") || model.starts_with("codex-") {
            Protocol::OpenAiResponses
        } else {
            Protocol::OpenAiChat
        };
        assert_eq!(route.protocol, expected, "{model}");
        let decoded = decode_reasoning_metadata(entry)
            .unwrap_or_else(|error| panic!("{model} reasoning metadata: {error}"));
        let declared = entry
            .get("reasoning_options")
            .and_then(serde_json::Value::as_array)
            .and_then(|options| {
                options
                    .iter()
                    .find(|option| option["type"] == "effort")
                    .map(|option| option["values"].clone())
            });
        if let Some(values) = declared {
            let values: Vec<String> = serde_json::from_value(values).unwrap();
            assert_eq!(
                decoded
                    .options
                    .as_ref()
                    .map(|options| options.values.clone()),
                Some(values),
                "{model} must keep every declared thinking level"
            );
            assert_eq!(decoded.control, Some(ReasoningControl::Effort), "{model}");
        }
    }
    assert!(
        checked >= 10,
        "expected OpenCode Go models in the catalog, found {checked}"
    );
}

#[test]
fn openai_discovery_skips_the_rejected_gpt_5_6_alias() {
    let preset = &crate::providers::OPENAI;
    assert_eq!(discovered_preset_binding(preset, "gpt-5.6"), None);
    assert_eq!(
        discovered_preset_binding(preset, "gpt-5.6-sol")
            .map(|route| (route.endpoint_id, route.protocol)),
        Some(("openai", Protocol::OpenAiResponses))
    );
}

#[test]
fn metadata_sparse_multimodal_model_ids_get_a_vision_fallback() {
    let response = serde_json::json!({
        "data": [{
            "id": "Intel/Qwen3.6-27B-int4-AutoRound",
            "max_model_len": 131_072
        }]
    });
    let models = api_models_from_response(&response).unwrap();
    assert_eq!(models.len(), 1);
    assert!(models[0].vision);
    assert!(model_id_implies_vision("gemini-2.5-pro"));
    assert!(model_id_implies_vision("anthropic/claude-sonnet-4-6"));
    assert!(!model_id_implies_vision("openai/gpt-6-astra"));
    assert!(crate::providers::OPENAI
        .discovery_capabilities
        .gpt_vision_fallback("openai/gpt-6-astra"));
    assert!(model_id_implies_vision("deepseek-v4-flash-vision-exp"));
    assert!(!model_id_implies_vision("deepseek-v4-flash"));
    assert!(model_id_implies_vision("Qwen/Qwen2.5-VL-7B"));
    assert!(!model_id_implies_vision("Qwen/Qwen3-Coder-30B"));
}

#[test]
fn sparse_gpt_6_compatible_inventory_needs_explicit_capability_metadata() {
    let response = serde_json::json!({
        "data": [{"id": "gpt-6-astra"}]
    });
    let models = api_models_from_response(&response).unwrap();
    assert!(!models[0].vision);
    assert!(!models[0].reasoning);

    let response = serde_json::json!({
        "data": [{
            "id": "gpt-6-astra",
            "architecture": {"input_modalities": ["text", "image"]},
            "supported_parameters": ["reasoning_effort"]
        }]
    });
    let models = api_models_from_response(&response).unwrap();
    assert!(models[0].vision);
    assert!(models[0].reasoning);
}

#[test]
fn deepseek_v4_discovery_uses_documented_limits_when_inventory_is_sparse() {
    let response = serde_json::json!({
        "data": [
            {"id": "deepseek-v4-flash"},
            {"id": "deepseek-v4-flash-vision-exp"},
            {"id": "deepseek-v3"}
        ]
    });
    let models = api_models_from_response(&response).unwrap();
    assert!(models
        .iter()
        .find(|model| model.id == "deepseek-v4-flash-vision-exp")
        .is_some_and(|model| model.vision));
    let limits = models
        .iter()
        .map(|model| (model.id.as_str(), deepseek_discovered_limits(model)))
        .collect::<std::collections::BTreeMap<_, _>>();

    assert_eq!(
        limits["deepseek-v4-flash"],
        (
            DEEPSEEK_DEFAULT_CONTEXT_WINDOW,
            DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS
        )
    );
    assert_eq!(
        limits["deepseek-v4-flash-vision-exp"],
        (
            DEEPSEEK_DEFAULT_CONTEXT_WINDOW,
            DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS
        )
    );
    assert_eq!(limits["deepseek-v3"], (128_000, 64_000));
}

#[test]
fn model_inventory_normalizes_flattened_audio_modalities() {
    let response = serde_json::json!({
        "data": [{
            "id": "audio-model",
            "input_modalities": ["text", "audio"]
        }]
    });
    let models = api_models_from_response(&response).unwrap();
    assert_eq!(models.len(), 1);
    assert!(!models[0].vision);
    assert!(models[0].audio);
}

#[test]
fn custom_model_inventory_defaults_sparse_metadata_to_tool_capable() {
    let response = serde_json::json!({
        "data": [
            {"id": "unknown"},
            {"id": "parameters", "supported_parameters": ["tools"]},
            {"id": "empty-parameters", "supported_parameters": []},
            {
                "id": "capability-object",
                "capabilities": {"tool_calling": {"supported": true}}
            },
            {
                "id": "provider-metadata",
                "provider": {"capabilities": {"function_calling": true}}
            },
            {
                "id": "explicitly-disabled",
                "supports_tools": false,
                "supported_parameters": ["tools"]
            }
        ]
    });
    let models = api_models_from_response(&response).unwrap();
    let tools = models
        .iter()
        .map(|model| (model.id.as_str(), model.tools))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert!(tools["unknown"]);
    assert!(tools["parameters"]);
    assert!(!tools["empty-parameters"]);
    assert!(tools["capability-object"]);
    assert!(tools["provider-metadata"]);
    assert!(!tools["explicitly-disabled"]);
}

#[test]
fn configured_custom_model_metadata_overrides_discovered_sparse_inventory() {
    use crate::auth::custom::CustomModel;

    let configured = CustomModel {
        api_name: "system".into(),
        context_window: 4_096,
        context_window_configured: true,
        max_output_tokens: 1_024,
        tools: true,
        reasoning: true,
        reasoning_configurable: false,
        ..Default::default()
    };

    let discovered_system = CustomModel {
        api_name: "system".into(),
        context_window: 262_144,
        max_output_tokens: 16_384,
        context_window_asserted: true,
        discovered_context_window: Some(262_144),
        max_output_tokens_asserted: true,
        tools: true,
        ..Default::default()
    };

    let discovered_other = CustomModel {
        api_name: "other".into(),
        context_window: 8_192,
        context_window_asserted: true,
        discovered_context_window: Some(8_192),
        ..Default::default()
    };

    let configured_missing = CustomModel {
        api_name: "configured-only".into(),
        context_window: 12_288,
        context_window_configured: true,
        ..Default::default()
    };

    // Discovery enabled: the saved operational context remains pinned while
    // endpoint limits are retained independently as detected metadata. Output
    // stays the tighter of both caps.
    let merged = apply_configured_custom_model_overrides(
        vec![discovered_system.clone(), discovered_other.clone()],
        &[configured.clone(), configured_missing.clone()],
        true,
    );

    assert_eq!(merged[0].api_name, "system");
    assert_eq!(merged[0].context_window, 4_096);
    assert_eq!(merged[0].discovered_context_window, Some(262_144));
    assert_eq!(merged[0].max_output_tokens, 1_024);
    assert!(merged[0].tools);
    assert!(merged[0].reasoning);
    assert!(!merged[0].reasoning_configurable);
    assert_eq!(
        custom_reasoning_capability(&merged[0]).unwrap().control,
        ReasoningControl::AlwaysOn
    );
    assert_eq!(merged[1].api_name, "other");
    assert_eq!(merged[1].context_window, 8_192);
    assert_eq!(merged[2].api_name, "configured-only");
    assert_eq!(merged[2].context_window, 12_288);

    // Explicit opt-out: `auto_discover: false` pins the registry inventory.
    let pinned = apply_configured_custom_model_overrides(
        vec![discovered_system, discovered_other],
        &[configured, configured_missing],
        false,
    );
    assert_eq!(pinned[0].context_window, 4_096);
    assert_eq!(pinned[0].max_output_tokens, 1_024);
}

#[test]
fn custom_endpoint_limits_remain_separate_from_explicit_operational_pins() {
    use crate::auth::custom::CustomModel;

    // A vLLM profile reports a new effective context. Keep the user's existing
    // operational budget intact and record the live value separately.
    let configured = CustomModel {
        api_name: "qwen38-gptq-mtp4-stable".into(),
        context_window: 131_072,
        context_window_configured: true,
        max_output_tokens: 16_384,
        ..Default::default()
    };
    let discovered = CustomModel {
        api_name: "qwen38-gptq-mtp4-stable".into(),
        context_window: 161_000,
        max_output_tokens: 16_384,
        context_window_asserted: true,
        discovered_context_window: Some(161_000),
        max_output_tokens_asserted: true,
        ..Default::default()
    };
    let merged = apply_configured_custom_model_overrides(
        vec![discovered],
        std::slice::from_ref(&configured),
        true,
    );
    assert_eq!(merged[0].context_window, 131_072);
    assert_eq!(merged[0].discovered_context_window, Some(161_000));
    assert_eq!(merged[0].max_output_tokens, 16_384);

    // A reported shrink is retained as fresh metadata; it does not silently
    // rewrite the explicitly saved operational budget.
    let configured = CustomModel {
        api_name: "qwen38-gptq-mtp4-stable".into(),
        context_window: 161_000,
        context_window_configured: true,
        max_output_tokens: 16_384,
        ..Default::default()
    };
    let discovered = CustomModel {
        api_name: "qwen38-gptq-mtp4-stable".into(),
        context_window: 32_768,
        max_output_tokens: 16_384,
        context_window_asserted: true,
        discovered_context_window: Some(32_768),
        max_output_tokens_asserted: true,
        ..Default::default()
    };
    let merged = apply_configured_custom_model_overrides(
        vec![discovered],
        std::slice::from_ref(&configured),
        true,
    );
    assert_eq!(merged[0].context_window, 161_000);
    assert_eq!(merged[0].discovered_context_window, Some(32_768));
    assert_eq!(merged[0].max_output_tokens, 16_384);
}

#[test]
fn unasserted_discovery_fallbacks_never_overwrite_configured_limits() {
    use crate::auth::custom::CustomModel;

    let configured = CustomModel {
        api_name: "system".into(),
        context_window: 8_192,
        context_window_configured: true,
        max_output_tokens: 1_024,
        ..Default::default()
    };
    // A sparse inventory: the entry carries only an id, so both limits are the
    // discovery fallback rather than something the endpoint said.
    let sparse = CustomModel {
        api_name: "system".into(),
        context_window: 262_144,
        max_output_tokens: 16_384,
        ..Default::default()
    };
    let merged = apply_configured_custom_model_overrides(
        vec![sparse],
        std::slice::from_ref(&configured),
        true,
    );
    assert_eq!(merged[0].context_window, 8_192);
    assert_eq!(merged[0].max_output_tokens, 1_024);
    assert!(!merged[0].context_window_asserted);
    assert!(!merged[0].max_output_tokens_asserted);

    // A llama.cpp gateway reports its effective context without an output cap.
    // Keep the configured operational budget and refresh the detected field.
    let context_only = CustomModel {
        api_name: "system".into(),
        context_window: 131_072,
        max_output_tokens: 16_384,
        context_window_asserted: true,
        discovered_context_window: Some(131_072),
        ..Default::default()
    };
    let merged = apply_configured_custom_model_overrides(
        vec![context_only],
        std::slice::from_ref(&configured),
        true,
    );
    assert_eq!(merged[0].context_window, 8_192);
    assert_eq!(merged[0].discovered_context_window, Some(131_072));
    assert_eq!(merged[0].max_output_tokens, 1_024);
    assert!(merged[0].context_window_asserted);
    assert!(!merged[0].max_output_tokens_asserted);

    // The mirror case: an asserted output cap is honored, and the unasserted
    // window keeps the configured pin rather than the 262144 fallback.
    let output_only = CustomModel {
        api_name: "system".into(),
        context_window: 262_144,
        max_output_tokens: 4_096,
        max_output_tokens_asserted: true,
        ..Default::default()
    };
    let merged = apply_configured_custom_model_overrides(
        vec![output_only],
        std::slice::from_ref(&configured),
        true,
    );
    assert_eq!(merged[0].context_window, 8_192);
    // min(asserted 4096, configured 1024) clamped to the effective window.
    assert_eq!(merged[0].max_output_tokens, 1_024);
    assert!(!merged[0].context_window_asserted);
    assert!(merged[0].max_output_tokens_asserted);

    // An asserted output cap larger than the configured one still cannot raise
    // the user's tighter cap; the tighter of the two wins.
    let generous = CustomModel {
        api_name: "system".into(),
        context_window: 262_144,
        max_output_tokens: 32_000,
        max_output_tokens_asserted: true,
        ..Default::default()
    };
    let merged = apply_configured_custom_model_overrides(
        vec![generous],
        std::slice::from_ref(&configured),
        true,
    );
    assert_eq!(merged[0].max_output_tokens, 1_024);
}

#[test]
fn merging_twice_preserves_the_operational_pin_and_live_assertion() {
    use crate::auth::custom::CustomModel;

    // Registration merges once while caching and again on startup. Both the
    // explicit operational pin and independent discovered assertion survive.
    let configured = CustomModel {
        api_name: "alpha".into(),
        context_window: 32_000,
        context_window_configured: true,
        ..Default::default()
    };
    let discovered = CustomModel {
        api_name: "alpha".into(),
        context_window: 64_000,
        context_window_asserted: true,
        discovered_context_window: Some(64_000),
        ..Default::default()
    };
    let once = apply_configured_custom_model_overrides(
        vec![discovered.clone()],
        std::slice::from_ref(&configured),
        true,
    );
    assert_eq!(once[0].context_window, 32_000);
    assert_eq!(once[0].discovered_context_window, Some(64_000));
    assert!(once[0].context_window_asserted);

    let twice =
        apply_configured_custom_model_overrides(once, std::slice::from_ref(&configured), true);
    assert_eq!(twice[0].context_window, 32_000);
    assert_eq!(twice[0].discovered_context_window, Some(64_000));
    assert!(twice[0].context_window_asserted);
}

#[test]
fn a_sparse_id_only_inventory_keeps_configured_limits_through_registration_and_cache() {
    use crate::auth::custom::{CredentialStore, CustomCredential, CustomModel};

    // The real-world shape: a local OpenAI-compatible gateway whose /v1/models
    // lists ids and nothing else. Discovery must not replace the user's small
    // window and output cap with the 262144/16384 fallbacks, and the cached
    // inventory must carry the same effective limits on the next start.
    let directory = tempfile::tempdir().unwrap();
    let store = CredentialStore::new(directory.path().join("custom.json"));
    let cred = CustomCredential {
        base_url: "http://127.0.0.1:9/v1/".into(),
        api_key: "fixture".into(),
        api_name: String::new(),
        headers: Vec::new(),
        models: vec![CustomModel {
            api_name: "local".into(),
            display_name: "Local".into(),
            context_window: 8_192,
            context_window_configured: true,
            max_output_tokens: 1_024,
            tools: true,
            ..Default::default()
        }],
        auto_discover: true,
    };

    // Stand in for the id-only response: every limit came from a fallback.
    let sparse = |api_name: &str| CustomModel {
        api_name: api_name.to_owned(),
        context_window: 262_144,
        max_output_tokens: 16_384,
        context_window_asserted: false,
        max_output_tokens_asserted: false,
        ..Default::default()
    };
    let discovered = vec![sparse("local"), sparse("local-extra")];

    let merged = apply_configured_custom_model_overrides(
        apply_known_custom_model_defaults(&cred, discovered),
        &cred.models,
        true,
    );
    assert_eq!(merged[0].context_window, 8_192);
    assert_eq!(merged[0].max_output_tokens, 1_024);
    // A model with no configured counterpart keeps the discovery fallback.
    assert_eq!(merged[1].api_name, "local-extra");
    assert_eq!(merged[1].context_window, 262_144);

    // The cached inventory is the already-merged effective limit, so the next
    // start must read back the same numbers rather than re-deriving them.
    let fingerprint = "fixture-fingerprint";
    save_custom_model_cache_for(
        &store,
        "local-provider",
        &cred.base_url,
        fingerprint,
        &merged,
    )
    .unwrap();
    let loaded = load_custom_model_cache_for(&store, "local-provider", &cred.base_url, fingerprint)
        .unwrap()
        .expect("inventory must round-trip through the cache");
    let cached = match loaded {
        CachedCustomInventory::Available(models) => models,
        CachedCustomInventory::Unavailable => panic!("inventory cached as unavailable"),
    };
    assert_eq!(cached[0].api_name, "local");
    assert_eq!(cached[0].context_window, 8_192);
    assert_eq!(cached[0].max_output_tokens, 1_024);
    // The cached limit came from this file, not the endpoint, so the
    // provenance must round-trip or the next start cannot tell them apart.
    assert!(!cached[0].context_window_asserted);
    assert!(!cached[0].max_output_tokens_asserted);
}

#[test]
fn lm_studio_native_metadata_keeps_loaded_and_maximum_context_distinct() {
    use crate::auth::custom::CustomModel;
    use octet_ai::discovery::DiscoveryMetadataSource::{Absent, Explicit};

    let mut models = vec![CustomModel {
        api_name: "openai/gpt-oss-20b".into(),
        ..Default::default()
    }];
    let inventory = serde_json::json!({
        "models": [{
            "key": "openai/gpt-oss-20b",
            "type": "llm",
            "max_context_length": 262_144,
            "loaded_instances": [{"config": {"context_length": 4_096}}],
            "capabilities": {
                "vision": false,
                "trained_for_tool_use": true,
                "reasoning": {"allowed_options": ["low", "medium"], "default": "low"}
            }
        }]
    });
    apply_lm_studio_native_metadata(&mut models, Some(&inventory));

    let model = &models[0];
    let native = model.native_metadata.as_ref().unwrap();
    assert_eq!(native.max_context_window, Some(262_144));
    assert_eq!(native.max_context_source, Explicit);
    assert_eq!(native.loaded_context_window, Some(4_096));
    assert_eq!(native.loaded_context_source, Explicit);
    assert_eq!(model.context_window, 4_096);
    assert_eq!(model.discovered_context_window, Some(4_096));
    assert!(model.context_window_asserted);
    assert_eq!(native.vision, Some(false));
    assert_eq!(native.vision_source, Explicit);
    assert_eq!(native.tool_use, Some(true));
    assert_eq!(native.reasoning.supported, Some(true));
    assert_eq!(native.reasoning.source, Explicit);
    assert!(
        !model.reasoning,
        "native reasoning does not assert Chat support"
    );
    assert_eq!(model.reasoning_source, None);

    let mut unloaded = vec![CustomModel {
        api_name: "unloaded-model".into(),
        context_window: 8_192,
        context_window_configured: true,
        ..Default::default()
    }];
    let maximum_only = serde_json::json!({
        "models": [{
            "key": "unloaded-model",
            "type": "llm",
            "max_context_length": 131_072,
            "loaded_instances": []
        }]
    });
    apply_lm_studio_native_metadata(&mut unloaded, Some(&maximum_only));
    let model = &unloaded[0];
    let native = model.native_metadata.as_ref().unwrap();
    assert_eq!(native.max_context_window, Some(131_072));
    assert_eq!(native.loaded_context_source, Absent);
    assert_eq!(native.loaded_context_window, None);
    assert_eq!(model.context_window, 8_192);
    assert!(!model.context_window_asserted);
    assert_eq!(model.discovered_context_window, None);
}

#[test]
fn apple_foundation_models_fill_sparse_inventory_from_embedded_metadata() {
    use crate::auth::custom::{CustomCredential, CustomModel};

    let cred = CustomCredential {
        base_url: APPLE_FM_BASE_URL.into(),
        api_key: String::new(),
        api_name: String::new(),
        headers: Vec::new(),
        models: Vec::new(),
        auto_discover: true,
    };
    let models = apply_known_custom_model_defaults(
        &cred,
        vec![
            CustomModel {
                reasoning_source: Some(octet_ai::types::ReasoningMetadataSource::Absent),
                api_name: "system".into(),
                context_window: 262_144,
                max_output_tokens: 16_384,
                reasoning: false,
                ..Default::default()
            },
            CustomModel {
                reasoning_source: Some(octet_ai::types::ReasoningMetadataSource::Absent),
                api_name: "pcc".into(),
                context_window: 262_144,
                max_output_tokens: 16_384,
                ..Default::default()
            },
        ],
    );

    assert_eq!(models[0].context_window, 8_192);
    assert_eq!(models[0].max_output_tokens, APPLE_FM_MAX_OUTPUT_TOKENS);
    assert!(models[0].tools);
    assert!(models[0].reasoning);
    assert!(!models[0].reasoning_configurable);
    assert_eq!(
        custom_reasoning_capability(&models[0]).unwrap().control,
        ReasoningControl::AlwaysOn
    );

    assert_eq!(models[1].context_window, 32_768);
    assert_eq!(models[1].max_output_tokens, APPLE_FM_MAX_OUTPUT_TOKENS);
    assert!(models[1].reasoning_configurable);
    assert_eq!(models[1].reasoning_values, ["low", "medium", "high"]);
    assert_eq!(models[1].reasoning_default, "medium");
    assert_eq!(
        custom_reasoning_capability(&models[1]).unwrap().control,
        ReasoningControl::Effort
    );
}

#[test]
fn configured_apple_metadata_overrides_embedded_defaults() {
    use crate::auth::custom::{CustomCredential, CustomModel};

    let cred = CustomCredential {
        base_url: APPLE_FM_BASE_URL.into(),
        api_key: String::new(),
        api_name: String::new(),
        headers: Vec::new(),
        models: Vec::new(),
        auto_discover: true,
    };
    let configured = CustomModel {
        api_name: "system".into(),
        context_window: 4_096,
        context_window_configured: true,
        max_output_tokens: 512,
        tools: false,
        reasoning: false,
        ..Default::default()
    };
    // A sparse Apple response resolves through embedded metadata, but the
    // explicitly saved 4k operational budget remains pinned; the live 8k value
    // is refreshed independently. Other fields stay configured-wins.
    let merged = apply_configured_custom_model_overrides(
        apply_known_custom_model_defaults(
            &cred,
            vec![CustomModel {
                api_name: "system".into(),
                reasoning_source: Some(octet_ai::types::ReasoningMetadataSource::Absent),
                ..Default::default()
            }],
        ),
        std::slice::from_ref(&configured),
        true,
    );

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].context_window, 4_096);
    assert_eq!(
        merged[0].discovered_context_window,
        Some(APPLE_FM_SYSTEM_CONTEXT_WINDOW)
    );
    assert_eq!(merged[0].max_output_tokens, 512);
    assert!(!merged[0].tools);
    assert!(!merged[0].reasoning);

    // With discovery disabled the registry pin is authoritative.
    let pinned = apply_configured_custom_model_overrides(
        apply_known_custom_model_defaults(
            &cred,
            vec![CustomModel {
                api_name: "system".into(),
                reasoning_source: Some(octet_ai::types::ReasoningMetadataSource::Absent),
                ..Default::default()
            }],
        ),
        std::slice::from_ref(&configured),
        false,
    );
    assert_eq!(pinned[0].context_window, 4_096);
    assert_eq!(pinned[0].max_output_tokens, 512);
}

#[test]
fn apple_foundation_models_health_requires_the_native_server_shape() {
    assert!(apple_foundation_models_health_is_valid(
        &serde_json::json!({
            "status": "fm serve is running",
            "models": [{"name": "system", "available": true}]
        })
    ));
    assert!(!apple_foundation_models_health_is_valid(
        &serde_json::json!({
            "status": "ok",
            "models": [{"name": "system", "available": true}]
        })
    ));
    assert!(!apple_foundation_models_health_is_valid(
        &serde_json::json!({
            "status": "fm serve is running",
            "models": [{"name": "system", "available": false}]
        })
    ));
}

#[test]
fn apple_foundation_models_discovery_skips_an_absent_optional_server() {
    assert!(!custom_model_discovery_is_available(
        APPLE_FM_BASE_URL,
        || false
    ));
    assert!(custom_model_discovery_is_available(
        APPLE_FM_BASE_URL,
        || true
    ));
    assert!(custom_model_discovery_is_available(
        "http://127.0.0.1:8000/v1/",
        || panic!("non-Apple discovery must not probe Apple Foundation Models")
    ));
}

#[test]
fn custom_model_cache_fingerprint_changes_with_configured_metadata() {
    let credential = custom_credential_fingerprint("key", &http::HeaderMap::new());
    let empty = custom_model_cache_fingerprint(&credential, &[]);
    let configured = crate::auth::custom::CustomModel {
        api_name: "system".into(),
        context_window: APPLE_FM_SYSTEM_CONTEXT_WINDOW,
        max_output_tokens: APPLE_FM_MAX_OUTPUT_TOKENS,
        ..Default::default()
    };
    let with_override =
        custom_model_cache_fingerprint(&credential, std::slice::from_ref(&configured));
    let changed = custom_model_cache_fingerprint(
        &credential,
        &[crate::auth::custom::CustomModel {
            context_window: 4_096,
            ..configured
        }],
    );

    assert_ne!(empty, with_override);
    assert_ne!(with_override, changed);
}
#[test]
fn fixed_custom_reasoning_is_the_only_octet_thinking_option() {
    use crate::auth::custom::CustomModel;

    let fixed = CustomModel {
        reasoning: true,
        reasoning_configurable: false,
        ..Default::default()
    };
    let capability = custom_reasoning_capability(&fixed).unwrap();
    assert_eq!(capability.control, ReasoningControl::AlwaysOn);

    let configurable = CustomModel {
        reasoning: true,
        ..Default::default()
    };
    assert!(custom_reasoning_capability(&configurable).is_some());
}

fn id_only_model(api_name: &str) -> crate::auth::custom::CustomModel {
    // The shape a generic OpenAI-compatible `/v1/models` list produces: ids
    // only, every limit a fallback.
    crate::auth::custom::CustomModel {
        api_name: api_name.to_owned(),
        context_window: 262_144,
        max_output_tokens: 16_384,
        ..Default::default()
    }
}

fn custom_credential_at(base_url: &str) -> crate::auth::custom::CustomCredential {
    crate::auth::custom::CustomCredential {
        base_url: base_url.into(),
        api_key: "fixture".into(),
        api_name: String::new(),
        headers: Vec::new(),
        models: Vec::new(),
        auto_discover: true,
    }
}

#[test]
fn a_custom_provider_on_a_first_party_endpoint_gets_catalog_context_for_id_only_models() {
    for base in [
        "https://api.cerebras.ai/v1/",
        "https://api.cerebras.ai/v1",
        "https://API.CEREBRAS.AI:443/v1/",
    ] {
        let models = apply_known_custom_model_defaults(
            &custom_credential_at(base),
            vec![id_only_model("gpt-oss-120b"), id_only_model("qwen-3.8-27b")],
        );
        for model in &models {
            assert_eq!(
                custom_model_effective_context_window(model),
                Some(131_072),
                "{base} {}",
                model.api_name
            );
            assert_eq!(model.max_output_tokens, 40_960, "{base}");
            assert!(model.max_output_tokens_asserted);
        }
    }
}

#[test]
fn catalog_context_never_replaces_configured_or_asserted_values_or_guesses() {
    let cred = custom_credential_at("https://api.cerebras.ai/v1/");
    let configured = crate::auth::custom::CustomModel {
        context_window: 8_192,
        context_window_configured: true,
        ..id_only_model("gpt-oss-120b")
    };
    let asserted = crate::auth::custom::CustomModel {
        context_window: 65_536,
        context_window_asserted: true,
        discovered_context_window: Some(65_536),
        ..id_only_model("qwen-3.8-27b")
    };
    let models = apply_known_custom_model_defaults(
        &cred,
        vec![configured, asserted, id_only_model("not-in-the-catalog")],
    );
    assert_eq!(
        custom_model_effective_context_window(&models[0]),
        Some(8_192)
    );
    assert_eq!(
        custom_model_effective_context_window(&models[1]),
        Some(65_536)
    );
    // A model the reviewed catalog does not know stays unknown, not guessed.
    assert_eq!(custom_model_effective_context_window(&models[2]), None);
}

#[test]
fn catalog_context_applies_only_to_the_exact_declared_endpoint() {
    for base in [
        "http://127.0.0.1:9/v1/",
        "https://api.cerebras.ai/v2/",
        "http://api.cerebras.ai/v1/",
        "https://api.cerebras.ai.example.test/v1/",
    ] {
        let models = apply_known_custom_model_defaults(
            &custom_credential_at(base),
            vec![id_only_model("gpt-oss-120b")],
        );
        assert_eq!(
            custom_model_effective_context_window(&models[0]),
            None,
            "{base}"
        );
    }
}

#[test]
fn opencode_gemini_4_argon_offers_its_thinking_levels_from_an_id_only_inventory() {
    // OpenCode publishes model identifiers only, so without a declared contract
    // Argon registered with no reasoning and /thinking offered only "off".
    for (provider, id) in [
        ("opencode-go", "gemini-4-argon-contributor"),
        ("opencode", "gemini-4-argon"),
    ] {
        let declaration = BUILTIN_PROVIDER_DECLARATIONS
            .iter()
            .find(|declaration| declaration.id == provider)
            .unwrap();
        let mut catalog = ModelCatalog::default();
        crate::providers::register_private_endpoints_at_base_url(
            &mut catalog,
            declaration,
            Auth::bearer("fixture-key"),
            &url::Url::parse(declaration.base_url).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
        register_openai_compatible_models_from_response(
            &mut catalog,
            declaration,
            ModelFilter::All,
            &serde_json::json!({"data": [{"id": id}]}),
        )
        .unwrap();
        crate::providers::register_static_models(&mut catalog, declaration).unwrap();
        let model = catalog
            .resolve(&ModelId(format!("{provider}/{id}")))
            .unwrap();
        assert_eq!(model.spec.protocol, Protocol::GoogleGenerativeAi, "{id}");
        assert_eq!(
            crate::app::supported_levels_with_subagents(&model, false),
            [
                crate::config::ThinkingLevel::Low,
                crate::config::ThinkingLevel::Medium,
                crate::config::ThinkingLevel::High,
            ],
            "{id}"
        );
        let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
        assert_eq!(
            reasoning
                .wire_value(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium))
                .as_deref(),
            Some("MEDIUM"),
            "{id}"
        );
    }
}
