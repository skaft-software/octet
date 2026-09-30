//! Unit tests for `crate::catalog`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::catalog`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::auth::CredentialResolver;

#[test]
fn test_builtin_catalog_loads_and_resolves() {
    let cat = ModelCatalog::builtin().unwrap();
    let model = cat.resolve(&ModelId("gpt-4o-mini".to_string())).unwrap();
    assert_eq!(model.spec.api_name, "gpt-4o-mini");
    assert_eq!(model.endpoint.id.0, "openai");
    assert!(model.spec.cache.send_session_affinity_headers);
    assert_eq!(
        model.spec.cache.session_affinity_format,
        Some(crate::types::SessionAffinityFormat::OpenAi)
    );
}

#[test]
fn builtin_gpt_6_astra_matches_the_public_openai_contract() {
    let catalog = ModelCatalog::builtin().unwrap();
    let model = catalog.resolve(&ModelId("gpt-6-astra".to_owned())).unwrap();
    assert_eq!(model.spec.api_name, "gpt-6-astra");
    assert_eq!(model.spec.display_name.as_deref(), Some("GPT-6 Astra"));
    assert_eq!(model.spec.endpoint.0, "openai");
    assert_eq!(model.spec.protocol, crate::types::Protocol::OpenAiResponses);
    assert_eq!(model.spec.limits.context_window, 1_050_000);
    assert_eq!(model.spec.limits.max_output_tokens, 128_000);

    let capabilities = &model.spec.capabilities;
    assert!(capabilities
        .input_modalities
        .contains(crate::types::Modality::Image));
    assert!(!capabilities
        .input_modalities
        .contains(crate::types::Modality::Audio));
    assert_eq!(
        capabilities.output_modalities,
        crate::types::ModalitySet::none()
    );
    assert!(capabilities.tools);
    assert!(capabilities.parallel_tool_calls);
    assert!(capabilities.structured_output);
    assert!(!capabilities.responses_lite);
    assert_eq!(capabilities.agent_delegation, None);
    let reasoning = capabilities.reasoning.as_ref().unwrap();
    assert_eq!(reasoning.control, ReasoningControl::Effort);
    assert_eq!(reasoning.min_effort, crate::types::ReasoningEffort::Low);
    assert_eq!(reasoning.max_effort, crate::types::ReasoningEffort::Max);

    let pricing = model.spec.pricing.as_ref().unwrap();
    assert_eq!(pricing.input, crate::pricing::TokenRate(10_000_000));
    assert_eq!(pricing.cache_read, crate::pricing::TokenRate(1_000_000));
    assert_eq!(
        pricing.cache_write_5m,
        crate::pricing::TokenRate(12_500_000)
    );
    assert_eq!(pricing.output, crate::pricing::TokenRate(50_000_000));
    assert_eq!(pricing.tiers.len(), 1);
    let tier = &pricing.tiers[0];
    assert_eq!(tier.min_input_tokens, 272_001);
    assert_eq!(tier.input, Some(crate::pricing::TokenRate(20_000_000)));
    assert_eq!(tier.cache_read, Some(crate::pricing::TokenRate(2_000_000)));
    assert_eq!(
        tier.cache_write_5m,
        Some(crate::pricing::TokenRate(25_000_000))
    );
    assert_eq!(tier.output, Some(crate::pricing::TokenRate(75_000_000)));
}

#[test]
fn builtin_gpt_6_1_sol_uses_the_exact_public_contract_and_whole_request_tier() {
    let catalog = ModelCatalog::builtin().unwrap();
    let model = catalog.resolve(&ModelId("gpt-6.1-sol".into())).unwrap();
    assert_eq!(model.spec.endpoint.0, "openai");
    assert_eq!(model.spec.api_name, "gpt-6.1-sol");
    assert_eq!(model.spec.display_name.as_deref(), Some("GPT-6.1-Sol"));
    assert_eq!(model.spec.protocol, crate::Protocol::OpenAiResponses);
    assert_eq!(model.spec.limits.context_window, 1_050_000);
    assert_eq!(model.spec.limits.max_output_tokens, 128_000);
    assert!(model
        .spec
        .capabilities
        .input_modalities
        .contains(Modality::Image));
    assert!(model.spec.capabilities.tools && model.spec.capabilities.parallel_tool_calls);
    assert!(model.spec.cache.supports_explicit_prompt_cache_mode);
    let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
    let options = reasoning.options.as_ref().unwrap();
    assert_eq!(options.values, ["low", "medium", "high", "xhigh", "max"]);
    assert_eq!(options.default.as_deref(), Some("medium"));
    assert!(!reasoning.supports(&crate::ReasoningConfig::Off));
    assert!(!reasoning.supports(&crate::ReasoningConfig::Effort(
        crate::ReasoningEffort::Minimal
    )));

    let pricing = model.spec.pricing.as_ref().unwrap();
    assert_eq!((pricing.input.0, pricing.output.0), (2_000_000, 10_000_000));
    assert_eq!(
        (pricing.cache_read.0, pricing.cache_write_5m.0),
        (100_000, 2_500_000)
    );
    let tier = &pricing.tiers[0];
    assert_eq!(tier.min_input_tokens, 272_001);
    assert_eq!(
        (tier.input.unwrap().0, tier.output.unwrap().0),
        (4_000_000, 15_000_000)
    );
    assert_eq!(
        (tier.cache_read.unwrap().0, tier.cache_write_5m.unwrap().0),
        (200_000, 5_000_000)
    );
    // Crossing the cliff by one cached token reprices *every* bucket, not just
    // the excess token. Use divisible amounts so category totals stay exact.
    let usage = crate::Usage {
        input_tokens: 200_000,
        cache_read_tokens: 72_000,
        cache_write_tokens: 0,
        output_tokens: 10_000,
        ..Default::default()
    };
    let below = crate::pricing::cost_of(pricing, &usage).unwrap();
    assert_eq!(
        (below.input, below.cache_read, below.output),
        (400_000, 7_200, 100_000)
    );
    assert_eq!(below.total, 507_200);
    let above = crate::pricing::cost_of(
        pricing,
        &crate::Usage {
            cache_read_tokens: 72_001,
            ..usage
        },
    )
    .unwrap();
    assert_eq!(
        (above.input, above.cache_read, above.output),
        (800_000, 14_400, 150_000)
    );
    assert_eq!(above.total, 964_400);
    assert!(model.responses_features().async_tools);
    assert!(model.responses_features().steering);
    assert!(model.responses_features().reasoning_effort_updates);
}

#[test]
fn builtin_gpt6_controls_and_sol_luna_prices_are_route_qualified() {
    let catalog = ModelCatalog::builtin().unwrap();
    for (id, input) in [
        ("gpt-6-astra", 10_000_000),
        ("gpt-6-sol", 2_000_000),
        ("gpt-6-luna", 100_000),
    ] {
        let model = catalog.resolve(&ModelId(id.into())).unwrap();
        assert_eq!(model.spec.api_name, id);
        assert_eq!(model.spec.limits.context_window, 1_050_000);
        assert_eq!(model.spec.limits.max_output_tokens, 128_000);
        assert_eq!(
            model.endpoint.transport,
            crate::EndpointTransport::WebSocketPreferred
        );
        let features = model.responses_features();
        assert!(features.async_tools && features.steering && features.reasoning_effort_updates);
        assert!(!features.compact_reasoning_effort_updates);
        let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
        assert_eq!(
            reasoning.supports(&crate::ReasoningConfig::Off),
            id != "gpt-6-astra"
        );
        assert!(!reasoning.supports(&crate::ReasoningConfig::Effort(
            crate::ReasoningEffort::Ultra
        )));
        let pricing = model.spec.pricing.as_ref().unwrap();
        assert_eq!(pricing.input.0, input);
        assert_eq!(pricing.output.0, input * 5);
        assert_eq!(pricing.cache_read.0, input / 10);
        assert_eq!(pricing.cache_write_5m.0, input * 5 / 4);
        assert_eq!(pricing.tiers[0].min_input_tokens, 272_001);
        assert_eq!(pricing.tiers[0].input.unwrap().0, input * 2);
        assert_eq!(pricing.tiers[0].output.unwrap().0, input * 15 / 2);
        let mut endpoint = (*model.endpoint).clone();
        endpoint.runtime.responses_features = Default::default();
        assert_eq!(
            crate::Model {
                spec: model.spec,
                endpoint: Arc::new(endpoint)
            }
            .responses_features(),
            crate::ResponsesFeatures::default()
        );
    }
    assert_eq!(
        catalog
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap()
            .responses_features(),
        crate::ResponsesFeatures::default()
    );
    assert!(catalog
        .resolve(&ModelId("gpt-6-unverified".into()))
        .is_err());
}

#[test]
fn auxiliary_reasoning_uses_exact_default_when_off_is_not_advertised() {
    let catalog = ModelCatalog::builtin().unwrap();
    let base = catalog.resolve(&ModelId("gpt-6-astra".to_owned())).unwrap();
    let mut spec = (*base.spec).clone();
    let capability = spec.capabilities.reasoning.as_mut().unwrap();
    capability.options = Some(crate::types::ReasoningOptions {
        values: vec!["medium".into(), "high".into(), "max".into()],
        default: Some("high".into()),
    });
    capability.min_effort = crate::types::ReasoningEffort::Medium;
    let model = Model {
        spec: Arc::new(spec),
        endpoint: base.endpoint,
    };

    assert_eq!(
        crate::select_auxiliary_reasoning(&model).unwrap(),
        crate::types::ReasoningConfig::Effort(crate::types::ReasoningEffort::High)
    );
}

#[test]
fn endpoint_labels_are_presentation_only_and_follow_endpoint_identity() {
    let mut catalog = ModelCatalog::default();
    let endpoint_id = EndpointId("custom-apple-fm".into());
    catalog
        .register_endpoint(Endpoint {
            id: endpoint_id.clone(),
            base_url: url::Url::parse("http://127.0.0.1:1976/v1/").unwrap(),
            auth: crate::auth::Auth::None,
            default_headers: http::HeaderMap::new(),
            transport: crate::types::EndpointTransport::Http,
            runtime: crate::types::RequestRuntime::default(),
            timeout: std::time::Duration::from_secs(300),
        })
        .unwrap();
    catalog
        .set_endpoint_label(endpoint_id.clone(), "Apple Foundation Models")
        .unwrap();
    assert_eq!(
        catalog.endpoint_label(&endpoint_id),
        Some("Apple Foundation Models")
    );
    catalog
        .set_endpoint_label(endpoint_id.clone(), "   ")
        .unwrap();
    assert_eq!(
        catalog.endpoint_label(&endpoint_id),
        Some("Apple Foundation Models")
    );
    assert!(catalog
        .set_endpoint_label(EndpointId("missing".into()), "Missing")
        .is_err());
}

#[test]
fn retain_configured_models_hides_only_endpoints_with_missing_env_credentials() {
    let mut catalog = ModelCatalog::builtin().unwrap();
    let available = catalog
        .resolve(&ModelId("gpt-4o-mini".to_string()))
        .unwrap();
    let mut unavailable_spec = (*available.spec).clone();
    unavailable_spec.id = ModelId("unconfigured-model".into());
    unavailable_spec.endpoint = EndpointId("unconfigured".into());
    let mut local_spec = (*available.spec).clone();
    local_spec.id = ModelId("local-model".into());
    local_spec.endpoint = EndpointId("local".into());
    catalog
        .register_endpoint(Endpoint {
            id: EndpointId("local".into()),
            base_url: url::Url::parse("http://127.0.0.1:1234/v1/").unwrap(),
            auth: crate::auth::Auth::None,
            default_headers: http::HeaderMap::new(),
            transport: crate::types::EndpointTransport::Http,
            runtime: crate::types::RequestRuntime::default(),
            timeout: std::time::Duration::from_secs(1),
        })
        .unwrap();
    catalog.register_model(local_spec).unwrap();
    catalog
        .register_endpoint(Endpoint {
            id: EndpointId("unconfigured".into()),
            base_url: url::Url::parse("https://example.invalid/v1/").unwrap(),
            auth: crate::auth::Auth::bearer_env(format!(
                "OCTET_TEST_MISSING_KEY_{}",
                std::process::id()
            )),
            default_headers: http::HeaderMap::new(),
            transport: crate::types::EndpointTransport::Http,
            runtime: crate::types::RequestRuntime::default(),
            timeout: std::time::Duration::from_secs(1),
        })
        .unwrap();
    catalog.register_model(unavailable_spec).unwrap();

    catalog.retain_configured_models();

    assert!(catalog.resolve(&ModelId("local-model".into())).is_ok());
    assert!(catalog
        .resolve(&ModelId("unconfigured-model".into()))
        .is_err());
}

#[test]
fn builtin_catalog_prices_current_text_models() {
    let catalog = ModelCatalog::builtin().unwrap();
    for id in [
        "gpt-4o-mini",
        "gpt-5.4-mini-responses",
        "gpt-6-astra",
        "claude-sonnet-4-5",
        "claude-fable-5",
        "claude-opus-4-8",
        "claude-sonnet-4-6",
    ] {
        let model = catalog.resolve(&ModelId(id.to_owned())).unwrap();
        assert!(model.spec.pricing.is_some(), "{id} must have pricing");
    }
    let sonnet = catalog
        .resolve(&ModelId("claude-sonnet-4-5".to_owned()))
        .unwrap();
    assert_eq!(
        sonnet.spec.pricing.as_ref().unwrap().cache_write_1h,
        Some(crate::pricing::TokenRate(6_000_000))
    );
}

#[test]
fn builtin_audio_capabilities_are_route_effective_and_format_specific() {
    let catalog = ModelCatalog::builtin().unwrap();
    let audio = catalog.resolve(&ModelId("gpt-audio-1.5".into())).unwrap();
    let capabilities = audio.spec.audio_capabilities().unwrap();
    assert_eq!(
        capabilities.input_formats,
        &[
            crate::types::AudioFormat::Wav,
            crate::types::AudioFormat::Mp3
        ]
    );
    assert!(capabilities
        .output_formats
        .contains(&crate::types::AudioFormat::Pcm16));
    assert_eq!(
        capabilities.output_delivery,
        Some(crate::types::AudioOutputDelivery::Completed)
    );
    assert!(audio
        .spec
        .effective_input_modalities()
        .contains(Modality::Audio));

    let responses = catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    assert!(responses.spec.audio_capabilities().is_none());
    assert!(!responses
        .spec
        .effective_input_modalities()
        .contains(Modality::Audio));
}

#[test]
fn catalog_rejects_audio_bits_on_protocols_without_audio_codecs() {
    let catalog = ModelCatalog::builtin().unwrap();
    let mut spec = (*catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap()
        .spec)
        .clone();
    spec.capabilities.input_modalities = spec.capabilities.input_modalities.with(Modality::Audio);
    assert!(matches!(
        validate_model_spec(&spec),
        Err(ConfigError::InvalidModel(_))
    ));
}

#[test]
fn test_builtin_catalog_registers_max_effort_claude_models() {
    let cat = ModelCatalog::builtin().unwrap();
    for id in ["claude-fable-5", "claude-opus-4-8", "claude-sonnet-4-6"] {
        let model = cat.resolve(&ModelId(id.to_string())).unwrap();
        assert_eq!(
            model.spec.protocol,
            crate::types::Protocol::AnthropicMessages
        );
        let reasoning = model
            .spec
            .capabilities
            .reasoning
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must advertise reasoning"));
        assert_eq!(reasoning.control, ReasoningControl::Effort);
        assert_eq!(
            reasoning.max_effort,
            crate::types::ReasoningEffort::Max,
            "{id} must advertise max effort"
        );
    }
}

#[test]
fn test_invalid_reasoning_effort_range_fails() {
    let catalog = ModelCatalog::builtin().unwrap();
    let model = catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".to_string()))
        .unwrap();
    let mut spec = (*model.spec).clone();
    let reasoning = spec.capabilities.reasoning.as_mut().unwrap();
    reasoning.min_effort = crate::types::ReasoningEffort::Max;
    reasoning.max_effort = crate::types::ReasoningEffort::Low;

    assert!(matches!(
        validate_model_spec(&spec),
        Err(ConfigError::InvalidReasoningConfig(_))
    ));
}

#[test]
fn test_invalid_base_url_fails() {
    let endpoints = vec![EndpointConfig {
        id: EndpointId("invalid".to_string()),
        // Userinfo and query parameters are forbidden and must not be
        // reflected back if they carry credentials.
        base_url: url::Url::parse("https://alice:URL_SECRET@example.test/v1/?token=QUERY_SECRET")
            .unwrap(),
        auth: AuthConfig::None,
        default_headers: BTreeMap::new(),
        transport: crate::types::EndpointTransport::Http,
        runtime: crate::types::RequestRuntime::default(),
        timeout_secs: 10,
    }];

    let cfg = CatalogConfig {
        endpoints,
        models: vec![],
    };
    let error = match ModelCatalog::from_config(cfg) {
        Ok(_) => panic!("credential-bearing base URL must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(&error, ConfigError::InvalidBaseUrl(_)));
    let diagnostic = error.to_string();
    assert!(!diagnostic.contains("URL_SECRET"), "{diagnostic}");
    assert!(!diagnostic.contains("QUERY_SECRET"), "{diagnostic}");

    // Missing trailing slash
    let endpoints_slash = vec![EndpointConfig {
        id: EndpointId("invalid".to_string()),
        base_url: url::Url::parse("https://api.openai.com/v1").unwrap(),
        auth: AuthConfig::None,
        default_headers: BTreeMap::new(),
        transport: crate::types::EndpointTransport::Http,
        runtime: crate::types::RequestRuntime::default(),
        timeout_secs: 10,
    }];
    let cfg_slash = CatalogConfig {
        endpoints: endpoints_slash,
        models: vec![],
    };
    assert!(matches!(
        ModelCatalog::from_config(cfg_slash),
        Err(ConfigError::InvalidBaseUrl(_))
    ));
}

#[test]
fn test_auth_header_collision() {
    let mut default_headers = BTreeMap::new();
    // insert authorization header name, which will collide with BearerEnv
    default_headers.insert("authorization".to_string(), "Bearer foo".to_string());

    let cfg = CatalogConfig {
        endpoints: vec![EndpointConfig {
            id: EndpointId("ep".to_string()),
            base_url: url::Url::parse("https://api.openai.com/v1/").unwrap(),
            auth: AuthConfig::BearerEnv {
                var: "KEY".to_string(),
            },
            default_headers,
            transport: crate::types::EndpointTransport::Http,
            runtime: crate::types::RequestRuntime::default(),
            timeout_secs: 10,
        }],
        models: vec![],
    };
    assert!(matches!(
        ModelCatalog::from_config(cfg),
        Err(ConfigError::AuthHeaderCollision(_))
    ));
}

struct DummyResolver;
#[async_trait::async_trait]
impl CredentialResolver for DummyResolver {
    async fn resolve(&self) -> Result<crate::auth::ResolvedCredential, crate::error::AuthError> {
        // This resolver only needs to be *bound* during catalog loading; the
        // loading tests never call `resolve()`. Return a deterministic error
        // rather than panicking during an unexpected test invocation.
        Err(crate::error::AuthError::InvalidCredential)
    }
}

#[test]
fn test_dynamic_resolver_loading() {
    let cfg = CatalogConfig {
        endpoints: vec![EndpointConfig {
            id: EndpointId("ep".to_string()),
            base_url: url::Url::parse("https://api.openai.com/v1/").unwrap(),
            auth: AuthConfig::Dynamic {
                resolver_id: "dyn_id".to_string(),
            },
            default_headers: BTreeMap::new(),
            transport: crate::types::EndpointTransport::Http,
            runtime: crate::types::RequestRuntime::default(),
            timeout_secs: 10,
        }],
        models: vec![],
    };

    // fails through from_config (no resolvers supplied)
    assert!(matches!(
        ModelCatalog::from_config(cfg.clone()),
        Err(ConfigError::MissingCredentialResolver(_))
    ));

    // succeeds when resolvers supplied
    let mut resolvers = CredentialResolverRegistry::new();
    resolvers.insert("dyn_id".to_string(), Arc::new(DummyResolver));
    assert!(ModelCatalog::from_config_with_resolvers(cfg, &resolvers).is_ok());
}
#[test]
fn review_regression_always_on_checks_legacy_exact_values() {
    let catalog = ModelCatalog::builtin().unwrap();
    let model = catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut spec = (*model.spec).clone();
    spec.protocol = Protocol::OpenAiChat;
    let reasoning = spec.capabilities.reasoning.as_mut().unwrap();
    reasoning.control = ReasoningControl::AlwaysOn;
    reasoning.options = None;
    reasoning.openai_chat_mode = OpenAiChatReasoningMode::ProviderValues {
        values: vec!["none".into()],
        default: Some("none".into()),
        system_message: true,
    };
    assert!(matches!(
        validate_model_spec(&spec),
        Err(ConfigError::InvalidReasoningConfig(_))
    ));
    spec.capabilities
        .reasoning
        .as_mut()
        .unwrap()
        .openai_chat_mode = OpenAiChatReasoningMode::ProviderValues {
        values: vec!["default".into()],
        default: Some("default".into()),
        system_message: true,
    };
    validate_model_spec(&spec).unwrap();
}
