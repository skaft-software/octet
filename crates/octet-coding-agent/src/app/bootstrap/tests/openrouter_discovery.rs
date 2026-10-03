//! Openrouter discovery
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::*;

#[test]
fn openrouter_discovery_uses_live_ids_limits_and_capabilities() {
    let response = serde_json::json!({
        "data": [
            {
                "id": "zeta/model",
                "context_length": 64_000,
                "top_provider": { "max_completion_tokens": 8_000 },
                "architecture": { "input_modalities": ["text", "image", "audio"] },
                "supported_parameters": ["tools", "tool_choice", "reasoning", "reasoning.effort"],
                "pricing": {
                    "prompt": "0.00000015",
                    "completion": "0.00000060",
                    "input_cache_read": "0.000000075"
                }
            },
            {
                "id": "alpha/model",
                "context_length": 8_000,
                "top_provider": { "max_completion_tokens": 16_000 },
                "supported_parameters": []
            }
        ]
    });

    let models = openrouter_models_from_response(&crate::providers::OPENROUTER, &response).unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id.0, "openrouter/alpha/model");
    assert_eq!(models[1].id.0, "openrouter/zeta/model");
    assert_eq!(models[1].api_name, "zeta/model");
    assert_eq!(models[1].limits.context_window, 64_000);
    assert_eq!(models[1].limits.max_output_tokens, 8_000);
    assert!(models[1].capabilities.tools);
    assert!(models[1].capabilities.reasoning.is_some());
    assert_eq!(
        models[1]
            .capabilities
            .reasoning
            .as_ref()
            .unwrap()
            .openai_chat_mode,
        OpenAiChatReasoningMode::OpenRouter
    );
    let pricing = models[1].pricing.as_ref().expect("OpenRouter price");
    assert_eq!(pricing.input, TokenRate(150_000));
    assert_eq!(pricing.output, TokenRate(600_000));
    assert_eq!(pricing.cache_read, TokenRate(75_000));
    assert!(models[1]
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert!(models[1]
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Audio));
    // An advertised output limit cannot exceed the model context window.
    assert_eq!(models[0].limits.max_output_tokens, 8_000);
    assert!(!models[0].capabilities.tools);
}

#[test]
fn openrouter_discovery_requires_an_advertised_completion_ceiling() {
    let response = serde_json::json!({
        "data": [
            {
                "id": "missing/limit",
                "context_length": 64_000
            },
            {
                "id": "top-level/limit",
                "context_length": 64_000,
                "max_completion_tokens": 12_000
            }
        ]
    });

    let models = openrouter_models_from_response(&crate::providers::OPENROUTER, &response).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].api_name, "top-level/limit");
    assert_eq!(models[0].limits.max_output_tokens, 12_000);
}

#[test]
fn third_party_gpt_6_astra_discovery_uses_its_pinned_provider_metadata() {
    let response = serde_json::json!({
        "data": [{
            "id": "openai/gpt-6-astra",
            "context_length": 128_000,
            "top_provider": {"max_completion_tokens": 32_000}
        }]
    });
    let models = openrouter_models_from_response(&crate::providers::OPENROUTER, &response).unwrap();
    assert_eq!(models.len(), 1);
    // A third-party Astra route does not inherit direct OpenAI or snapshot
    // capabilities, even when its provider-scoped display metadata is known.
    let snapshot =
        octet_ai::model_metadata::model_capability_metadata("openrouter", "openai/gpt-6-astra")
            .unwrap();
    assert_eq!(
        models[0].display_name.as_deref(),
        Some(snapshot["name"].as_str().unwrap())
    );
    assert!(!models[0]
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert!(models[0].capabilities.reasoning.is_none());
    assert!(!models[0].capabilities.tools);
    assert!(!models[0].capabilities.structured_output);
    assert!(!models[0].capabilities.parallel_tool_calls);
    assert_eq!(models[0].protocol, Protocol::OpenAiChat);
    assert_eq!(models[0].limits.context_window, 128_000);
    assert_eq!(models[0].limits.max_output_tokens, 32_000);
    assert!(!models[0].capabilities.responses_lite);
    assert!(models[0].capabilities.agent_delegation.is_none());

    let mut advertised = response;
    advertised["data"][0]["architecture"] =
        serde_json::json!({"input_modalities": ["text", "image"]});
    advertised["data"][0]["supported_parameters"] =
        serde_json::json!(["reasoning.effort", "tools", "response_format"]);
    advertised["data"][0]["reasoning"] =
        serde_json::json!({"supported":true,"values":["low","high"],"default":"high"});
    let models =
        openrouter_models_from_response(&crate::providers::OPENROUTER, &advertised).unwrap();
    assert_eq!(models.len(), 1);
    assert!(models[0]
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert!(models[0].capabilities.tools);
    assert!(models[0].capabilities.structured_output);
    let reasoning = models[0].capabilities.reasoning.as_ref().unwrap();
    assert_eq!(
        reasoning.openai_chat_mode,
        OpenAiChatReasoningMode::OpenRouter
    );
    assert_eq!(reasoning.options.as_ref().unwrap().values, ["low", "high"]);
    assert_eq!(
        reasoning.options.as_ref().unwrap().default.as_deref(),
        Some("high")
    );
    assert!(reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low)));
    assert!(reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)));
    assert!(!reasoning.supports(&ReasoningConfig::Off));
    assert!(!reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max)));
    assert!(!reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra)));
    assert_eq!(models[0].protocol, Protocol::OpenAiChat);
    assert_eq!(models[0].limits.context_window, 128_000);
    assert_eq!(models[0].limits.max_output_tokens, 32_000);
    assert!(!models[0].capabilities.responses_lite);
    assert!(models[0].capabilities.agent_delegation.is_none());
}

#[test]
fn openrouter_anthropic_routes_enable_anthropic_cache_markers() {
    let response = serde_json::json!({
        "data": [{
            "id": "anthropic/claude-sonnet-4-5",
            "context_length": 200_000,
            "top_provider": { "max_completion_tokens": 8_192 }
        }]
    });
    let models = openrouter_models_from_response(&crate::providers::OPENROUTER, &response).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(
        models[0].cache.cache_control_format,
        Some(octet_ai::CacheControlFormat::Anthropic)
    );
}
