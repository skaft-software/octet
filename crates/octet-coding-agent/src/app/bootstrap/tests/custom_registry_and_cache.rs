//! Custom registry and cache
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::*;

#[test]
fn custom_registry_registers_labeled_providers_with_isolated_auth_and_models() {
    use crate::auth::custom::{
        CustomAuthConfig, CustomCredential, CustomModel, CustomProvider, CustomRegistry,
    };

    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    let provider = |label: &str, base_url: &str, model_id: &str, auth| CustomProvider {
        label: label.into(),
        credential: CustomCredential {
            base_url: base_url.into(),
            api_key: String::new(),
            api_name: String::new(),
            headers: Vec::new(),
            models: vec![CustomModel {
                api_name: model_id.into(),
                ..Default::default()
            }],
            auto_discover: false,
        },
        auth,
        api_key_env: None,
        cache: None,
        startup_timeout_secs: None,
        lifecycle_feedback: false,
    };
    let mut registry = CustomRegistry::single(
        "apple-fm",
        provider(
            "Apple Foundation Models",
            "http://127.0.0.1:1976/v1/",
            "shared-model",
            Some(CustomAuthConfig::None),
        ),
    );
    registry
        .providers
        .get_mut("apple-fm")
        .unwrap()
        .lifecycle_feedback = true;
    registry.providers.insert(
        "home-server".into(),
        provider(
            "Home Server",
            "http://127.0.0.1:8000/v1/",
            "shared-model",
            Some(CustomAuthConfig::BearerEnv {
                var: "OCTET_TEST_HOME_SERVER_KEY".into(),
            }),
        ),
    );
    // One provider declares explicit per-token pricing.
    let mut priced = provider(
        "Metered Gateway",
        "http://127.0.0.1:8500/v1/",
        "metered-model",
        Some(CustomAuthConfig::None),
    );
    priced.credential.models[0].pricing = Some(crate::auth::custom::CustomPricing {
        input: 75,
        output: 300,
        ..Default::default()
    });
    registry.providers.insert("metered-gateway".into(), priced);
    registry.providers.insert(
        "invalid/provider".into(),
        provider(
            "Invalid Provider",
            "http://127.0.0.1:9000/v1/",
            "invalid-model",
            Some(CustomAuthConfig::None),
        ),
    );
    store.save_registry(&registry).unwrap();

    let mut catalog = ModelCatalog::default();
    register_custom_openai_endpoints_from_store(&mut catalog, &store, true).unwrap();

    let apple = catalog
        .resolve(&ModelId("custom/apple-fm/shared-model".into()))
        .unwrap();
    assert_eq!(apple.endpoint.id.0, "custom-provider-8-apple-fm");
    assert_eq!(
        catalog.endpoint_label(&apple.endpoint.id),
        Some("Apple Foundation Models")
    );
    assert_eq!(
        apple.endpoint.base_url.as_str(),
        "http://127.0.0.1:1976/v1/"
    );
    assert!(matches!(apple.endpoint.auth, Auth::None));
    assert!(apple.endpoint.runtime.lifecycle_feedback);

    let home = catalog
        .resolve(&ModelId("custom/home-server/shared-model".into()))
        .unwrap();
    assert_eq!(home.endpoint.id.0, "custom-provider-11-home-server");
    assert_eq!(
        catalog.endpoint_label(&home.endpoint.id),
        Some("Home Server")
    );
    assert!(matches!(
        home.endpoint.auth,
        Auth::BearerEnv { ref var } if var == "OCTET_TEST_HOME_SERVER_KEY"
    ));
    assert!(!home.endpoint.runtime.lifecycle_feedback);

    // Undeclared custom-model pricing defaults to trusted zero rates so
    // cost-ceiling guardrails (such as subagent budgets) stay enforceable.
    let default_pricing = home
        .spec
        .pricing
        .as_ref()
        .expect("custom models must carry trusted pricing");
    assert_eq!(default_pricing.input, TokenRate(0));
    assert_eq!(default_pricing.output, TokenRate(0));
    assert_eq!(default_pricing.cache_read, TokenRate(0));
    assert_eq!(default_pricing.cache_write_5m, TokenRate(0));

    // A declared pricing block is honored verbatim.
    let metered = catalog
        .resolve(&ModelId("custom/metered-gateway/metered-model".into()))
        .unwrap();
    let declared = metered.spec.pricing.as_ref().expect("declared pricing");
    assert_eq!(declared.input, TokenRate(75));
    assert_eq!(declared.output, TokenRate(300));
    assert_eq!(declared.cache_read, TokenRate(0));

    assert!(catalog
        .resolve(&ModelId("custom/invalid/provider/invalid-model".into()))
        .is_err());
}

#[tokio::test]
async fn custom_model_preset_registry_discovery_cache_and_wire_preserve_only_configured_controls() {
    use octet_ai::{CompatibilityMode, Message, Request, UserMessage, UserPart};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let directory = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let secret = "configured-model-header-canary";
    Mock::given(method("GET")).and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"id": "configured", "preset": {"headers": {"x-model-secret": "untrusted-replacement"}}},
                {"id": "discovered", "preset": {
                    "headers": {"x-remote-secret": "untrusted-inventory-header"},
                    "sampling_params": {"top_p": 0.01}, "vllm_priority": 99
                }}
            ]
        }))).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(concat!(
                "data: {\"id\":\"preset\",\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
                "data: {\"id\":\"preset\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
            ))).expect(1).mount(&server).await;
    let provider: crate::auth::custom::CustomProvider = serde_json::from_value(serde_json::json!({
        "base_url": format!("{}/v1/", server.uri()), "auth": {"kind": "none"}, "auto_discover": true,
        "models": [{"api_name": "configured", "preset": {
            "headers": {"x-model-secret": secret},
            "sampling_params": {"temperature": 0.25, "top_p": 0.7}, "vllm_priority": -3
        }}]
    })).unwrap();
    let expected_preset = provider.credential.models[0].preset.clone();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    store
        .save_registry(&crate::auth::custom::CustomRegistry::single(
            "fixture", provider,
        ))
        .unwrap();
    let (online, offline) = tokio::task::spawn_blocking(move || {
        let mut online = ModelCatalog::default();
        register_custom_openai_endpoints_from_store(&mut online, &store, false).unwrap();
        let mut offline = ModelCatalog::default();
        register_custom_openai_endpoints_from_store(&mut offline, &store, true).unwrap();
        let cache =
            String::from_utf8(store.load_model_cache_for("fixture").unwrap().unwrap()).unwrap();
        assert!(!cache.contains("configured-model-header-canary"));
        assert!(!cache.contains("untrusted-inventory-header"));
        (online, offline)
    })
    .await
    .unwrap();
    let id = ModelId("custom/fixture/configured".into());
    let model = offline.resolve(&id).unwrap();
    assert_eq!(online.resolve(&id).unwrap().spec.preset, expected_preset);
    assert_eq!(model.spec.preset, expected_preset);
    assert_eq!(
        offline
            .resolve(&ModelId("custom/fixture/discovered".into()))
            .unwrap()
            .spec
            .preset,
        octet_ai::ModelPreset::default(),
        "provider inventory cannot grant request overrides"
    );
    let public = serde_json::to_string(&*model.spec).unwrap();
    assert!(!public.contains(secret));
    assert!(!public.contains("x-model-secret"));
    assert!(!format!("{model:?}").contains(secret));
    let request = Request {
        system: Some("fixture system".into()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("fixture prompt".into())],
        })],
        tools: vec![],
        tool_choice: octet_ai::ToolChoice::None,
        max_output_tokens: Some(128),
        temperature: Some(0.75),
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: octet_ai::OutputFormat::Text,
        output_modalities: octet_ai::OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: octet_ai::CacheRetention::None,
        session_id: None,
    };
    AiClient::new().complete(&model, request).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let post = requests
        .iter()
        .find(|request| request.method.as_str() == "POST")
        .unwrap();
    assert_eq!(
        post.headers
            .get("x-model-secret")
            .unwrap()
            .to_str()
            .unwrap(),
        secret
    );
    assert!(post.headers.get("x-remote-secret").is_none());
    assert!(post.headers.get("authorization").is_none());
    let body: serde_json::Value = serde_json::from_slice(&post.body).unwrap();
    assert_eq!(
        body["temperature"], 0.75,
        "canonical request wins over the preset default"
    );
    assert_eq!(body["top_p"], 0.7);
    assert_eq!(body["priority"], -3);
    assert!(!body.to_string().contains(secret));
    assert_eq!(
        model.spec.preset, expected_preset,
        "dispatch must not mutate the configured model"
    );
}

#[test]
fn custom_model_preset_cache_omits_headers_without_mutating_the_private_source() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    let secret = "cache-preset-header-canary";
    let configured: crate::auth::custom::CustomModel = serde_json::from_value(serde_json::json!({
        "api_name": "configured", "preset": {
            "headers": {"x-model-secret": secret}, "sampling_params": {"top_p": 0.7}
        }
    }))
    .unwrap();
    let fingerprint = custom_model_cache_fingerprint("account", std::slice::from_ref(&configured));
    save_custom_model_cache_for(
        &store,
        "fixture",
        "http://localhost/v1/",
        &fingerprint,
        std::slice::from_ref(&configured),
    )
    .unwrap();
    let raw = String::from_utf8(store.load_model_cache_for("fixture").unwrap().unwrap()).unwrap();
    assert!(!raw.contains(secret));
    assert!(!raw.contains("x-model-secret"));
    assert_eq!(configured.preset.headers["x-model-secret"], secret);
    let Some(CachedCustomInventory::Available(models)) =
        load_custom_model_cache_for(&store, "fixture", "http://localhost/v1/", &fingerprint)
            .unwrap()
    else {
        panic!("missing cache")
    };
    assert!(models[0].preset.headers.is_empty());
    assert_eq!(
        models[0].preset.sampling_params,
        configured.preset.sampling_params
    );
    // Also reject cached header authority at the read boundary, rather than
    // relying solely on every historical cache having used the current writer.
    let cache = CustomModelCache {
        version: CUSTOM_MODEL_CACHE_VERSION,
        base_url: "http://localhost/v1/".into(),
        credential_fingerprint: fingerprint.clone(),
        models: vec![configured.clone()],
    };
    store
        .save_model_cache_for("fixture", &serde_json::to_vec(&cache).unwrap())
        .unwrap();
    let Some(CachedCustomInventory::Available(models)) =
        load_custom_model_cache_for(&store, "fixture", "http://localhost/v1/", &fingerprint)
            .unwrap()
    else {
        panic!("missing cache")
    };
    assert!(models[0].preset.headers.is_empty());
    let mut changed = configured.clone();
    changed
        .preset
        .headers
        .insert("x-model-secret".into(), "replacement".into());
    assert_ne!(
        custom_model_cache_fingerprint("account", &[changed]),
        fingerprint
    );
}

#[test]
fn custom_model_cache_is_scoped_to_endpoint_and_reuses_discovery() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    let models = vec![crate::auth::custom::CustomModel {
        api_name: "local-model".into(),
        display_name: "Local Model".into(),
        context_window: 262_144,
        max_output_tokens: 16_384,
        context_window_asserted: true,
        max_output_tokens_asserted: true,
        tools: true,
        parallel_tool_calls: true,
        vision: false,
        structured_output: false,
        reasoning: true,
        reasoning_configurable: true,
        reasoning_profile: None,
        reasoning_source: None,
        reasoning_values: Vec::new(),
        reasoning_default: String::new(),
        reasoning_uses_system_message: true,
        pricing: None,
        preset: Default::default(),
    }];
    let mut first_headers = http::HeaderMap::new();
    first_headers.insert("x-organization", "tenant-one".parse().unwrap());
    first_headers.insert("x-region", "north".parse().unwrap());
    let first_key = custom_credential_fingerprint("custom-key-one", &first_headers);
    save_custom_model_cache(&store, "http://one.test/v1/", &first_key, &models).unwrap();
    let Some(CachedCustomInventory::Available(loaded)) =
        load_custom_model_cache(&store, "http://one.test/v1/", &first_key).unwrap()
    else {
        panic!("expected positive custom inventory")
    };
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].api_name, "local-model");
    assert!(
        load_custom_model_cache(&store, "http://two.test/v1/", &first_key)
            .unwrap()
            .is_none(),
        "a cache from another endpoint must never populate this catalog"
    );
    assert!(
        load_custom_model_cache(
            &store,
            "http://one.test/v1/",
            &custom_credential_fingerprint("custom-key-two", &first_headers),
        )
        .unwrap()
        .is_none(),
        "a cache from another custom account must never populate this catalog"
    );
    let mut changed_headers = first_headers.clone();
    changed_headers.insert("x-organization", "tenant-two".parse().unwrap());
    assert!(
        load_custom_model_cache(
            &store,
            "http://one.test/v1/",
            &custom_credential_fingerprint("custom-key-one", &changed_headers),
        )
        .unwrap()
        .is_none(),
        "changing a tenant or authorization header must invalidate the inventory"
    );

    let mut reordered_headers = http::HeaderMap::new();
    reordered_headers.insert("x-region", "north".parse().unwrap());
    reordered_headers.insert("x-organization", "tenant-one".parse().unwrap());
    assert_eq!(
        first_key,
        custom_credential_fingerprint("custom-key-one", &reordered_headers),
        "header insertion order is not part of the credential scope"
    );

    save_custom_model_cache(&store, "http://one.test/v1/", &first_key, &[]).unwrap();
    assert!(
        matches!(
            load_custom_model_cache(&store, "http://one.test/v1/", &first_key).unwrap(),
            Some(CachedCustomInventory::Unavailable)
        ),
        "an empty inventory is a valid negative cache marker"
    );
}

#[test]
fn custom_model_cache_invalidates_when_configured_metadata_changes() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    let base_url = "http://custom.test/v1/";
    let credential = custom_credential_fingerprint("key", &http::HeaderMap::new());
    let configured = crate::auth::custom::CustomModel {
        api_name: "system".into(),
        context_window: APPLE_FM_SYSTEM_CONTEXT_WINDOW,
        max_output_tokens: APPLE_FM_MAX_OUTPUT_TOKENS,
        ..Default::default()
    };
    let original = custom_model_cache_fingerprint(&credential, std::slice::from_ref(&configured));
    let changed = custom_model_cache_fingerprint(
        &credential,
        &[crate::auth::custom::CustomModel {
            context_window: 4_096,
            ..configured
        }],
    );
    save_custom_model_cache_for(
        &store,
        "provider",
        base_url,
        &original,
        &[crate::auth::custom::CustomModel {
            api_name: "system".into(),
            ..Default::default()
        }],
    )
    .unwrap();

    assert!(matches!(
        load_custom_model_cache_for(&store, "provider", base_url, &original).unwrap(),
        Some(CachedCustomInventory::Available(_))
    ));
    assert!(
        load_custom_model_cache_for(&store, "provider", base_url, &changed)
            .unwrap()
            .is_none(),
        "changing configured metadata must force fresh discovery"
    );
}

#[test]
fn version_four_custom_cache_is_invalid_after_hlid_tool_fallback_change() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    let base_url = "https://ai.watchyourtemper.com/v1/";
    let fingerprint = custom_credential_fingerprint("", &http::HeaderMap::new());
    let stale = CustomModelCache {
        version: 4,
        base_url: base_url.into(),
        credential_fingerprint: fingerprint.clone(),
        models: vec![crate::auth::custom::CustomModel {
            api_name: "qwen3.6-27b".into(),
            tools: false,
            ..Default::default()
        }],
    };
    store
        .save_model_cache(&serde_json::to_vec(&stale).unwrap())
        .unwrap();

    assert!(
        load_custom_model_cache(&store, base_url, &fingerprint)
            .unwrap()
            .is_none(),
        "v4 may contain tools=false from the pre-tri-state hlid path"
    );
}

#[test]
fn version_nine_custom_cache_is_invalid_after_endpoint_limits_fix() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    let base_url = "http://127.0.0.1:8000/v1/";
    let fingerprint = custom_credential_fingerprint("", &http::HeaderMap::new());
    // v9 cached the post-override registry pin, so a profile switch from 131k
    // to 161k could never converge: every refresh re-entombed the stale value.
    let stale = CustomModelCache {
        version: 9,
        base_url: base_url.into(),
        credential_fingerprint: fingerprint.clone(),
        models: vec![crate::auth::custom::CustomModel {
            api_name: "qwen38-gptq-mtp4-stable".into(),
            context_window: 131_072,
            ..Default::default()
        }],
    };
    store
        .save_model_cache(&serde_json::to_vec(&stale).unwrap())
        .unwrap();

    assert!(
        load_custom_model_cache(&store, base_url, &fingerprint)
            .unwrap()
            .is_none(),
        "v9 may contain a configured-wins context_window pin over a live max_model_len assertion"
    );
}

#[tokio::test]
async fn stale_positive_custom_cache_is_available_without_waiting_for_discovery() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    let directory = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "currently-served-model"}]
        })))
        .expect(0)
        .mount(&server)
        .await;
    let provider: crate::auth::custom::CustomProvider = serde_json::from_value(serde_json::json!({
        "base_url": format!("{}/v1/", server.uri()),
        "auth": {"kind": "none"},
        "auto_discover": true
    }))
    .unwrap();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    let credential = custom_credential_fingerprint("", &http::HeaderMap::new());
    let fingerprint = custom_model_cache_fingerprint(&credential, &[]);
    let previous = crate::auth::custom::CustomModel {
        api_name: "last-good-model".into(),
        ..Default::default()
    };
    save_custom_model_cache_for(
        &store,
        "fixture",
        &provider.credential.base_url,
        &fingerprint,
        std::slice::from_ref(&previous),
    )
    .unwrap();
    let cache_path = std::fs::read_dir(directory.path().join("credentials"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .next()
        .unwrap();
    // `File::open` is read-only, and Windows requires write-attribute access
    // for `set_times`; open read/write so the backdate works on all platforms.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&cache_path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    assert!(store
        .model_cache_is_stale_for("fixture", PROVIDER_INVENTORY_REFRESH_INTERVAL)
        .unwrap());

    let [online, offline] = tokio::task::spawn_blocking(move || {
        [false, true].map(|offline| {
            let mut catalog = ModelCatalog::default();
            register_custom_openai_provider(
                &mut catalog,
                &store,
                "fixture",
                &provider,
                false,
                offline,
            )
            .unwrap();
            catalog
        })
    })
    .await
    .unwrap();
    let id = ModelId("custom/fixture/last-good-model".into());
    assert!(online.resolve(&id).is_ok());
    assert!(offline.resolve(&id).is_ok());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn hlid_llama_cpp_metadata_reports_the_served_context_window() {
    let entry = serde_json::json!({
        "id": "ornith-35b-q4km",
        "meta": {
            "n_ctx": 131_072,
            "n_ctx_train": 262_144
        }
    });

    assert_eq!(extract_ctx_from_model_entry(&entry), Some(131_072));
    assert_eq!(
        extract_ctx_from_model_entry(&serde_json::json!({"id": "sparse"})),
        None
    );
}

#[test]
fn sparse_custom_inventory_preserves_local_tools_but_honors_explicit_false() {
    // This is the live hlid shape: it advertises reasoning details but no
    // standardized tool capability field. A user-configured local OpenAI
    // endpoint keeps the historical tool-capable default.
    let sparse_hlid = serde_json::json!({
        "id": "qwen3.6-27b",
        "capabilities": {"reasoning": {
            "supported": true,
            "control": "binary",
            "values": ["none", "default"],
            "default": "default"
        }}
    });
    assert_eq!(model_metadata_tool_support(&sparse_hlid), None);
    assert!(custom_model_metadata_supports_tools(&sparse_hlid));
    assert!(!model_metadata_supports_tools(&sparse_hlid));

    let explicitly_disabled = serde_json::json!({
        "id": "text-only",
        "capabilities": {"tools": {"supported": false}}
    });
    assert_eq!(
        model_metadata_tool_support(&explicitly_disabled),
        Some(false)
    );
    assert!(!custom_model_metadata_supports_tools(&explicitly_disabled));

    let explicit_parameter_list = serde_json::json!({
        "id": "reasoning-only",
        "supported_parameters": ["reasoning_effort"]
    });
    assert_eq!(
        model_metadata_tool_support(&explicit_parameter_list),
        Some(false)
    );
    assert!(!custom_model_metadata_supports_tools(
        &explicit_parameter_list
    ));
}

#[test]
fn hlid_reasoning_metadata_controls_custom_capabilities_exactly() {
    let off_only = serde_json::json!({
        "capabilities": {"reasoning": {
            "supported": true,
            "control": "binary",
            "values": ["none"],
            "default": "none"
        }}
    });
    let (reasoning, values, default) = discovered_custom_reasoning(&off_only);
    assert!(!reasoning);
    assert_eq!(values, ["none"]);
    assert_eq!(default, "none");
    let off_model = crate::auth::custom::CustomModel {
        reasoning,
        reasoning_values: values,
        reasoning_default: default,
        ..Default::default()
    };
    assert!(custom_reasoning_capability(&off_model).is_none());

    let binary = serde_json::json!({
        "capabilities": {"reasoning": {
            "supported": true,
            "control": "binary",
            "values": ["none", "default"],
            "default": "default"
        }}
    });
    let (reasoning, values, default) = discovered_custom_reasoning(&binary);
    let binary_model = crate::auth::custom::CustomModel {
        reasoning,
        reasoning_values: values,
        reasoning_default: default,
        reasoning_uses_system_message: true,
        ..Default::default()
    };
    let binary_capability = custom_reasoning_capability(&binary_model).unwrap();
    assert_eq!(binary_capability.control, ReasoningControl::Toggle);
    assert!(matches!(
        binary_capability.openai_chat_mode,
        OpenAiChatReasoningMode::ProviderValues {
            values,
            default: Some(default),
            system_message: true,
        } if values == ["none", "default"] && default == "default"
    ));

    let levels = serde_json::json!({
        "capabilities": {"reasoning": {
            "supported": true,
            "control": "levels",
            "values": ["none", "low", "medium", "high"],
            "default": "medium"
        }}
    });
    let (reasoning, values, default) = discovered_custom_reasoning(&levels);
    let level_model = crate::auth::custom::CustomModel {
        reasoning,
        reasoning_values: values,
        reasoning_default: default,
        ..Default::default()
    };
    let level_capability = custom_reasoning_capability(&level_model).unwrap();
    assert_eq!(level_capability.control, ReasoningControl::Effort);
    assert_eq!(level_capability.min_effort, octet_ai::ReasoningEffort::Low);
    assert_eq!(level_capability.max_effort, octet_ai::ReasoningEffort::High);
}

#[test]
fn negative_custom_cache_recovers_without_another_restart() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::auth::custom::CredentialStore::new(directory.path().join("credentials/custom.json"));
    let cred = crate::auth::custom::CustomCredential {
        base_url: "http://custom.test/v1/".to_string(),
        api_key: "key".to_string(),
        api_name: String::new(),
        headers: Vec::new(),
        models: Vec::new(),
        auto_discover: true,
    };
    let fingerprint = custom_credential_fingerprint(&cred.api_key, &http::HeaderMap::new());
    save_custom_model_cache(&store, &cred.base_url, &fingerprint, &[]).unwrap();
    assert!(matches!(
        load_custom_model_cache(&store, &cred.base_url, &fingerprint).unwrap(),
        Some(CachedCustomInventory::Unavailable)
    ));
    let recovered = crate::auth::custom::CustomModel {
        api_name: "recovered-local".to_string(),
        ..Default::default()
    };

    let models = discover_and_cache_custom_models_with(&store, &cred, &fingerprint, false, |_| {
        vec![recovered.clone()]
    });
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].api_name, "recovered-local");
    assert!(matches!(
        load_custom_model_cache(&store, &cred.base_url, &fingerprint).unwrap(),
        Some(CachedCustomInventory::Available(models))
            if models.len() == 1 && models[0].api_name == "recovered-local"
    ));
}
