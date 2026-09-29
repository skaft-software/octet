//! Pinned metadata
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[test]
fn direct_opus_5_5_has_official_defaults_and_prices_without_off() {
    let builtin = ModelCatalog::builtin().unwrap();
    let model = builtin.resolve(&ModelId("claude-opus-5-5".into())).unwrap();
    assert_eq!(model.spec.protocol, Protocol::AnthropicMessages);
    assert_eq!(model.spec.limits.context_window, 1_000_000);
    assert_eq!(model.spec.limits.max_output_tokens, 128_000);
    let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
    assert_eq!(
        reasoning.options.as_ref().unwrap().values,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(
        default_reasoning_for_model(&model),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium)
    );
    assert!(!reasoning.supports(&ReasoningConfig::Off));
    let price = model.spec.pricing.as_ref().unwrap();
    assert_eq!(price.input, TokenRate(4_000_000));
    assert_eq!(price.output, TokenRate(20_000_000));
    assert_eq!(price.cache_read, TokenRate(200_000));
    assert_eq!(price.cache_write_5m, TokenRate(5_000_000));
    assert_eq!(price.cache_write_1h, Some(TokenRate(8_000_000)));
    assert!(current_direct_model_pricing("opencode", "claude-opus-5-5").is_none());
    assert!(current_direct_model_pricing("github-copilot", "claude-opus-5-5").is_none());

    // The native provider's sparse inventory retains the same model contract;
    // an explicit negative assertion never gets overwritten by the fallback.
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "anthropic")
        .expect("Anthropic declaration");
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_anthropic_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &serde_json::json!({"data":[{"id":"claude-opus-5-5"}]}),
    )
    .unwrap();
    let discovered = catalog
        .resolve(&ModelId("anthropic/claude-opus-5-5".into()))
        .unwrap();
    assert_eq!(discovered.spec.limits, model.spec.limits);
    assert_eq!(discovered.spec.pricing, model.spec.pricing);
    assert_eq!(
        default_reasoning_for_model(&discovered),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium)
    );
    let mut asserted = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_anthropic_compatible_models_from_response(
        &mut asserted,
        declaration,
        ModelFilter::All,
        &serde_json::json!({"data":[{"id":"claude-opus-5-5","reasoning":false,
            "context_window":16_384,"max_output_tokens":4_096,"input_modalities":["text"]}]}),
    )
    .unwrap();
    let asserted = asserted
        .resolve(&ModelId("anthropic/claude-opus-5-5".into()))
        .unwrap();
    assert!(asserted.spec.capabilities.reasoning.is_none());
    assert_eq!(asserted.spec.limits.context_window, 16_384);
    assert!(!asserted
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
}

#[test]
fn direct_grok_4_7_uses_its_own_inventory_route_and_long_context_tariff() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "xai")
        .expect("xAI declaration");
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &serde_json::json!({"data":[
            {"id":"grok-4.7","tools":true},
            {"id":"grok-4.7-unverified"}
        ]}),
    )
    .unwrap();
    let model = catalog.resolve(&ModelId("xai/grok-4.7".into())).unwrap();
    assert_eq!(model.spec.protocol, Protocol::OpenAiResponses);
    assert_eq!(model.spec.limits.context_window, 500_000);
    assert_eq!(model.spec.limits.max_output_tokens, 500_000); // refreshed xAI snapshot limit
    assert!(model
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert!(model.spec.capabilities.tools);
    let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
    assert_eq!(
        reasoning.options.as_ref().unwrap().values,
        ["low", "medium", "high", "xhigh"]
    );
    assert_eq!(
        default_reasoning_for_model(&model),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    assert!(!reasoning.supports(&ReasoningConfig::Off));
    let price = model.spec.pricing.as_ref().unwrap();
    assert_eq!(price.input, TokenRate(2_000_000));
    assert_eq!(price.output, TokenRate(6_000_000));
    assert_eq!(price.cache_read, TokenRate(500_000));
    assert_eq!(price.tiers.len(), 1);
    assert_eq!(price.tiers[0].min_input_tokens, 200_000);
    assert_eq!(price.tiers[0].input, Some(TokenRate(4_000_000)));
    assert_eq!(price.tiers[0].output, Some(TokenRate(12_000_000)));
    assert_eq!(price.tiers[0].cache_read, Some(TokenRate(1_000_000)));
    assert!(current_direct_model_pricing("opencode", "grok-4.7").is_none());
    assert!(current_direct_model_pricing("github-copilot", "grok-4.7").is_none());
    let unverified = catalog
        .resolve(&ModelId("xai/grok-4.7-unverified".into()))
        .unwrap();
    assert_eq!(unverified.spec.limits.context_window, 128_000);
    assert!(unverified.spec.capabilities.reasoning.is_none());
    assert!(unverified.spec.pricing.is_none());

    let mut asserted = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_openai_compatible_models_from_response(
        &mut asserted,
        declaration,
        ModelFilter::All,
        &serde_json::json!({"data":[{"id":"grok-4.7","reasoning":false,
            "context_window":65_536,"max_output_tokens":4_096,
            "input_modalities":["text"],"tools":false}]}),
    )
    .unwrap();
    let asserted = asserted.resolve(&ModelId("xai/grok-4.7".into())).unwrap();
    assert_eq!(asserted.spec.limits.context_window, 65_536);
    assert_eq!(asserted.spec.limits.max_output_tokens, 4_096);
    assert!(!asserted
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert!(!asserted.spec.capabilities.tools);
    assert!(asserted.spec.capabilities.reasoning.is_none());
}

#[test]
fn pinned_metadata_display_merge_never_imports_functional_fields() {
    let snapshot = serde_json::json!({
        "name":"Snapshot label", "limit":{"context":1_000_000,"output":384_000},
        "modalities":{"input":["text","image"]}, "tool_call":true,
        "structured_output":true, "reasoning":true,
        "reasoning_options":{"values":["low","max"],"default":"max"},
        "interleaved":{"field":"reasoning_content"}
    });
    let sparse = serde_json::json!({"id":"fixture"});
    assert_eq!(
        builtin_display_entry(&sparse, &snapshot),
        serde_json::json!({"id":"fixture","display_name":"Snapshot label"})
    );
    for assertion in [
        serde_json::json!("Endpoint label"),
        serde_json::Value::Null,
        serde_json::json!(false),
        serde_json::json!({"malformed":true}),
    ] {
        for entry in [
            serde_json::json!({"id":"fixture","display_name":assertion}),
            serde_json::json!({"id":"fixture","provider":{"name":assertion}}),
            serde_json::json!({"id":"fixture","top_provider":{"name":assertion}}),
            serde_json::json!({"id":"fixture","capabilities":{"name":assertion}}),
        ] {
            assert_eq!(builtin_display_entry(&entry, &snapshot), entry);
        }
    }
}

/// A vision model whose live inventory omits modality fields must still accept
/// images when the pinned models.dev snapshot it is keyed to asserts image input.
/// The endpoint stays authoritative: an explicit live assertion (including an
/// explicit text-only list) always wins, and an id heuristic is never consulted
/// once any modality is asserted.
#[test]
fn sparse_inventory_inherits_pinned_image_input_without_overriding_endpoint_assertions() {
    let declaration = &crate::providers::DEEPSEEK;
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_deepseek_models_from_response(
        &mut catalog,
        declaration,
        &serde_json::json!({"data":[
            {"id":"deepseek-flash"},
            {"id":"deepseek-v4-pro"},
            {"id":"deepseek-v4-flash-vision-exp"},
            {"id":"deepseek-text-only-fixture","input_modalities":["text"]},
            {"id":"deepseek-unpinned-fixture"}
        ]}),
    )
    .unwrap();
    let vision = catalog
        .resolve(&ModelId("deepseek/deepseek-flash".into()))
        .unwrap();
    assert!(
        vision
            .spec
            .capabilities
            .input_modalities
            .contains(octet_ai::Modality::Image),
        "the pinned snapshot asserts text+image for DeepSeek V4.1 Flash"
    );
    // The pinned snapshot is text-only for V4 Pro, so the fix must not grant
    // image input by id, provider or family heuristic.
    let text_only = catalog
        .resolve(&ModelId("deepseek/deepseek-v4-pro".into()))
        .unwrap();
    assert!(
        !text_only
            .spec
            .capabilities
            .input_modalities
            .contains(octet_ai::Modality::Image),
        "the snapshot declares text-only input for DeepSeek V4 Pro"
    );
    // A model whose own id says "vision" and which the snapshot also lists as
    // multimodal still resolves to image input.
    let declared_vision = catalog
        .resolve(&ModelId("deepseek/deepseek-v4-flash-vision-exp".into()))
        .unwrap();
    assert!(declared_vision
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    // An endpoint that asserts a text-only inventory keeps that decision.
    let endpoint_authority = catalog
        .resolve(&ModelId("deepseek/deepseek-text-only-fixture".into()))
        .unwrap();
    assert!(!endpoint_authority
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    // No pinned snapshot entry means no invented capability.
    let unpinned = catalog
        .resolve(&ModelId("deepseek/deepseek-unpinned-fixture".into()))
        .unwrap();
    assert!(!unpinned
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
}

/// Regression: a model released after this binary was built must still get its
/// thinking levels from the provider's own inventory. OpenRouter advertises its
/// reasoning primitive through `supported_parameters`; treating that list as an
/// undecodable assertion left every new model (for example `stealth/union-alpha`)
/// with thinking permanently Off until the pinned snapshot was refreshed and the
/// binary rebuilt.
#[test]
fn newly_discovered_openrouter_model_decodes_its_advertised_reasoning_parameters() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "openrouter")
        .unwrap();
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_openrouter_models_from_response(
        &mut catalog,
        declaration,
        &serde_json::json!({"data":[
            {"id":"stealth/union-alpha","supported_parameters":["tools","reasoning"],
             "context_length":128_000,"max_completion_tokens":8_192},
            {"id":"vendor/text-only","supported_parameters":["tools"],
             "context_length":64_000,"max_completion_tokens":4_096},
            {"id":"vendor/explicit-off","supported_parameters":["reasoning"],
             "reasoning":false,"context_length":64_000,"max_completion_tokens":4_096}
        ]}),
    )
    .unwrap();
    let advertised = catalog
        .resolve(&ModelId("openrouter/stealth/union-alpha".into()))
        .unwrap();
    let capability = advertised
        .spec
        .capabilities
        .reasoning
        .as_ref()
        .unwrap_or_else(|| panic!("an advertised reasoning parameter must grant thinking levels"));
    assert_eq!(
        capability.openai_chat_mode,
        OpenAiChatReasoningMode::OpenRouter
    );
    assert_eq!(capability.choices(), vec![ReasoningConfig::On]);
    assert_eq!(capability.control, ReasoningControl::AlwaysOn);
    assert_eq!(
        octet_ai::select_auxiliary_reasoning(&advertised).unwrap(),
        ReasoningConfig::On
    );
    // A model that does not advertise reasoning stays without it.
    let text_only = catalog
        .resolve(&ModelId("openrouter/vendor/text-only".into()))
        .unwrap();
    assert!(text_only.spec.capabilities.reasoning.is_none());
    // An explicit negative assertion still wins over the parameter list.
    let explicit_off = catalog
        .resolve(&ModelId("openrouter/vendor/explicit-off".into()))
        .unwrap();
    assert!(explicit_off.spec.capabilities.reasoning.is_none());
}

#[test]
fn openrouter_saved_off_emits_diagnostics_during_build_and_session_rebuild() {
    const CHILD: &str = "OCTET_TEST_OPENROUTER_REASONING_DIAGNOSTIC";
    if std::env::var_os(CHILD).is_none() {
        // Exercise the real stderr route without replacing a process-global
        // descriptor underneath concurrently running unit tests.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(CHILD, "1")
            .args([
                "--exact",
                "app::bootstrap::tests::pinned_metadata::openrouter_saved_off_emits_diagnostics_during_build_and_session_rebuild",
                "--nocapture",
            ])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert_eq!(
            stderr
                .matches("cannot honor reasoning=off; using advertised reasoning=max")
                .count(),
            2,
            "{stderr}"
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut boot = bootstrap(config(directory.path(), Some("gpt-4o-mini"))).unwrap();
    let declaration = &crate::providers::OPENROUTER;
    boot.catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_openrouter_models_from_response(&mut boot.catalog, declaration, &serde_json::json!({"data":[{
        "id":"fixture/mandatory", "context_length":1310720, "max_completion_tokens":16384,
        "reasoning":{"mandatory":true,"default_enabled":true,"supported_efforts":["max","high","low"],"default_effort":"max"}
    }]})).unwrap();
    let model = ModelId("openrouter/fixture/mandatory".into());
    let saved_path = directory.path().join("saved-off.jsonl");
    let mut saved = Session::create(&saved_path).unwrap();
    append_config_if_changed(
        &mut saved,
        None,
        &model,
        &ReasoningConfig::Off,
        ReasoningMode::Standard,
    )
    .unwrap();
    drop(saved);
    let app = build_app(
        boot,
        LaunchSelection {
            model: model.clone(),
            session: SessionSelection::CreateNew(directory.path().join("initial-off.jsonl")),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
        },
        "system".into(),
    )
    .unwrap();
    let expected = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max);
    assert_eq!(app.reasoning, expected);
    let app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::OpenExisting(saved_path)),
    )
    .unwrap();
    assert_eq!(app.reasoning, expected);
    assert_eq!(
        persisted_session_config(app.agent.session())
            .unwrap()
            .reasoning,
        Some(expected)
    );
}

#[test]
fn openrouter_mandatory_reasoning_drives_picker_resume_and_summary_selection() {
    use octet_ai::ReasoningEffort;
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == "openrouter")
        .unwrap();
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_openrouter_models_from_response(
        &mut catalog,
        declaration,
        &serde_json::json!({"data":[{
            "id":"z-ai/glm-5.3-flash", "context_length":1_310_720,
            "top_provider":{"context_length":1_048_576,"max_completion_tokens":943_718},
            "supported_parameters":["tools","reasoning","reasoning_effort"],
            "reasoning":{"mandatory":true,"default_enabled":true,
                         "supported_efforts":["max","high","low"],"default_effort":"max"}
        }]}),
    )
    .unwrap();
    let model = catalog
        .resolve(&ModelId("openrouter/z-ai/glm-5.3-flash".into()))
        .unwrap();
    let capability = model.spec.capabilities.reasoning.as_ref().unwrap();
    assert_eq!(
        capability.options.as_ref().unwrap().values,
        ["max", "high", "low"]
    );
    assert_eq!(
        capability.options.as_ref().unwrap().default.as_deref(),
        Some("max")
    );
    assert!(!capability.supports(&ReasoningConfig::Off));
    assert!(!capability.supports(&ReasoningConfig::Effort(ReasoningEffort::Medium)));
    let expected = ReasoningConfig::Effort(ReasoningEffort::Max);
    assert_eq!(
        octet_ai::select_auxiliary_reasoning(&model).unwrap(),
        expected
    );
    let (selected, _, warning) = normalize_reasoning_selection_for_model_with_subagents(
        &ReasoningConfig::Off,
        ReasoningMode::Standard,
        &model,
        false,
    )
    .unwrap();
    assert_eq!(selected, expected);
    assert!(warning.unwrap().contains("cannot honor reasoning=off"));

    // A million-token route is not automatically restricted to a 128K working set.
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), None);
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        1.0
    );
    config.compaction.max_active_tokens = Some(120_000);
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        120_000.0 / 1_310_720.0
    );
    config.compaction.max_active_tokens = Some(0);
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        1.0
    );
}

#[test]
fn openrouter_reasoning_optional_toggle_and_malformed_contracts() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == "openrouter")
        .unwrap();
    let decode = |reasoning| {
        builtin_discovery_reasoning(
            &serde_json::json!({
                "reasoning": reasoning, "supported_parameters":["reasoning_effort"]
            }),
            Some(declaration),
        )
    };
    let mandatory = decode(serde_json::json!({"mandatory":true})).unwrap();
    let capability =
        discovered_reasoning_capability(declaration, Protocol::OpenAiChat, "fixture", &mandatory)
            .unwrap();
    assert_eq!(capability.control, ReasoningControl::AlwaysOn);
    assert_eq!(capability.choices(), vec![ReasoningConfig::On]);
    let optional = decode(serde_json::json!({"mandatory":false,"default_enabled":false})).unwrap();
    assert_eq!(optional.control, Some(ReasoningControl::Toggle));
    assert_eq!(optional.options.unwrap().default.as_deref(), Some("false"));
    let optional = decode(
        serde_json::json!({"mandatory":false,"default_enabled":false,
        "supported_efforts":["high","low"],"default_effort":"high"}),
    )
    .unwrap();
    let options = optional.options.unwrap();
    assert_eq!(options.default.as_deref(), Some("false"));
    assert!(options.choices().contains(&ReasoningConfig::Off));
    let optional = decode(serde_json::json!({"mandatory":false,"supported_efforts":["none","high"],"default_effort":"none"})).unwrap();
    assert_eq!(optional.options.unwrap().values, ["none", "high"]);
    let optional = decode(serde_json::json!({"mandatory":false,"default_enabled":true,
        "supported_efforts":["high","medium","low","none"],"default_effort":"none"}))
    .unwrap();
    assert_eq!(optional.options.unwrap().default.as_deref(), Some("none"));
    for invalid in [
        serde_json::json!({"mandatory":"true"}),
        serde_json::json!({"mandatory":true,"default_enabled":false}),
        serde_json::json!({"mandatory":true,"supported_efforts":["none","high"]}),
        serde_json::json!({"mandatory":true,"supported_efforts":["low"],"default_effort":"max"}),
        serde_json::json!({"mandatory":true,"supported_efforts":[]}),
        serde_json::json!({"mandatory":true,"default_effort":"max"}),
        serde_json::json!({"mandatory":false,"default_enabled":"false"}),
        serde_json::json!({"mandatory":false,"supported_efforts":["high","high"]}),
        serde_json::json!({"mandatory":false,"supported_efforts":["on"]}),
    ] {
        assert!(decode(invalid.clone()).is_err(), "accepted {invalid}");
    }
    // A negative assertion still wins, even against the native object.
    let disabled = builtin_discovery_reasoning(
        &serde_json::json!({
            "reasoning":{"mandatory":true}, "supports_reasoning":false,
            "supported_parameters":["reasoning_effort"]
        }),
        Some(declaration),
    )
    .unwrap();
    assert_eq!(disabled.supported, Some(false));
}

#[tokio::test]
async fn openrouter_discovery_to_summary_wire_uses_the_endpoint_contract() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"summary\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
                "data: [DONE]\n\n"
            )))
        .expect(4)
        .mount(&server).await;
    let declaration = &crate::providers::OPENROUTER;
    let mut catalog = metadata_fixture_catalog(declaration, &format!("{}/", server.uri()));
    register_openrouter_models_from_response(&mut catalog, declaration, &serde_json::json!({"data":[
        {"id":"fixture/mandatory","context_length":1_310_720,"max_completion_tokens":16384,
         "supported_parameters":["reasoning","reasoning_effort"],
         "reasoning":{"mandatory":true,"default_enabled":true,"supported_efforts":["max","high","low"],"default_effort":"max"}},
        {"id":"fixture/toggle","context_length":64000,"max_completion_tokens":8192,
         "reasoning":{"mandatory":false,"default_enabled":true}},
        {"id":"fixture/parameter-only","context_length":64000,"max_completion_tokens":8192,
         "supported_parameters":["reasoning_effort"]}
    ]})).unwrap();
    let client = AiClient::new();
    for (id, selection) in [
        ("mandatory", None),
        ("toggle", Some(ReasoningConfig::On)),
        ("toggle", Some(ReasoningConfig::Off)),
        ("parameter-only", None),
    ] {
        let model = catalog
            .resolve(&ModelId(format!("openrouter/fixture/{id}")))
            .unwrap();
        let reasoning =
            selection.unwrap_or_else(|| octet_ai::select_auxiliary_reasoning(&model).unwrap());
        client
            .complete(
                &model,
                octet_ai::Request {
                    system: Some("Summarize the conversation".into()),
                    messages: vec![octet_ai::Message::User(octet_ai::UserMessage {
                        content: vec![octet_ai::UserPart::Text("history".into())],
                    })],
                    tools: vec![],
                    tool_choice: octet_ai::ToolChoice::Auto,
                    max_output_tokens: Some(1024),
                    temperature: None,
                    stop: vec![],
                    reasoning,
                    reasoning_mode: ReasoningMode::Standard,
                    responses: None,
                    output_format: octet_ai::OutputFormat::Text,
                    output_modalities: octet_ai::OutputModalities::Text,
                    compatibility: octet_ai::CompatibilityMode::Strict,
                    cache_retention: octet_ai::CacheRetention::Short,
                    session_id: None,
                },
            )
            .await
            .unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    let bodies = requests
        .iter()
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["reasoning"], serde_json::json!({"effort":"max"}));
    assert_eq!(bodies[1]["reasoning"], serde_json::json!({"enabled":true}));
    assert!(bodies[2].get("reasoning").is_none());
    assert!(bodies[3].get("reasoning").is_none());
    assert!(bodies
        .iter()
        .all(|body| body.get("reasoning_effort").is_none()));
}

#[test]
fn pinned_metadata_enriches_discovered_display_only() {
    let declaration = &crate::providers::DEEPSEEK;
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_deepseek_models_from_response(
        &mut catalog,
        declaration,
        &serde_json::json!({"data":[]}),
    )
    .unwrap();
    assert_eq!(catalog.models().count(), 0);
    register_deepseek_models_from_response(
        &mut catalog,
        declaration,
        &serde_json::json!({"data":[{"id":"deepseek-flash"}]}),
    )
    .unwrap();
    assert_eq!(catalog.models().count(), 1);
    let model = catalog
        .resolve(&ModelId("deepseek/deepseek-flash".into()))
        .unwrap();
    assert_eq!(model.spec.api_name, "deepseek-flash");
    assert_eq!(
        model.spec.display_name.as_deref(),
        Some("DeepSeek V4.1 Flash")
    );
    // A sparse endpoint publishes no limits, so the pinned provider record
    // supplies its documented window (DeepSeek V4.1 Flash is 1M context /
    // 393,216 output) instead of the generic 128K/64K placeholder that used to
    // truncate this model. The endpoint still wins whenever it says anything.
    assert_eq!(model.spec.limits.context_window, 1_000_000);
    assert_eq!(model.spec.limits.max_output_tokens, 393_216);
    // The existing sparse tools default is independent of the supplement, and
    // every pinned field except the asserted input modalities and a documented
    // limit the endpoint refused to publish stays out of the authority boundary
    // (see the sibling image-input regression).
    assert!(model.spec.capabilities.tools);
    assert!(!model.spec.capabilities.structured_output);
    assert!(model
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert!(!model.spec.capabilities.parallel_tool_calls);
    assert!(!model.spec.capabilities.responses_lite);
    let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
    assert_eq!(
        reasoning.openai_chat_mode,
        OpenAiChatReasoningMode::DeepSeekThinking
    );
    assert!(reasoning.preserves_state);
    assert_eq!(
        reasoning.options.as_ref().unwrap().values,
        ["none", "low", "high", "max"]
    );
    assert_eq!(reasoning.options.as_ref().unwrap().default, None);
    assert!(!reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium)));
    assert!(!reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Xhigh)));
    // A flat models.dev quote is not the official peak/off-peak tariff.
    assert!(model.spec.pricing.is_none());
    assert!(crate::providers::pricing_for(declaration, "deepseek-v4-pro").is_none());

    // The same model may advertise richer functionality, without borrowing any
    // missing field or extra reasoning choice from its pinned snapshot.
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_deepseek_models_from_response(
        &mut catalog,
        declaration,
        &serde_json::json!({"data":[{
            "id":"deepseek-flash", "name":"Endpoint Flash",
            "context_window":96_000, "max_output_tokens":8192,
            "tools":true, "structured_output":true, "input_modalities":["text","image"],
            "reasoning":{"supported":true,"values":["low","high"],"default":"high"}
        }]}),
    )
    .unwrap();
    let model = catalog
        .resolve(&ModelId("deepseek/deepseek-flash".into()))
        .unwrap();
    assert_eq!(model.spec.display_name.as_deref(), Some("Endpoint Flash"));
    // Explicit endpoint limits are never replaced by the pinned record.
    assert_eq!(model.spec.limits.context_window, 96_000);
    assert_eq!(model.spec.limits.max_output_tokens, 8192);
    assert!(model.spec.capabilities.tools);
    assert!(model.spec.capabilities.structured_output);
    assert!(model
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
    assert_eq!(reasoning.options.as_ref().unwrap().values, ["low", "high"]);
    assert_eq!(
        reasoning.options.as_ref().unwrap().default.as_deref(),
        Some("high")
    );
    assert_eq!(
        reasoning.openai_chat_mode,
        OpenAiChatReasoningMode::DeepSeekThinking
    );
    assert!(reasoning.preserves_state);
    assert!(!reasoning.supports(&ReasoningConfig::Off));
    assert!(!reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max)));
}

#[test]
fn pinned_metadata_preserves_endpoint_assertions_and_unknowns() {
    let declaration = &crate::providers::DEEPSEEK;
    let parse = |entry| {
        api_models_from_response_for(&serde_json::json!({"data":[entry]}), Some(declaration))
    };
    let model = parse(serde_json::json!({
        "id":"deepseek-flash", "name":"Account Flash", "context_length":8192,
        "max_completion_tokens":2048, "tools":false, "structured_output":false,
        "input_modalities":["text"],
        "reasoning":{"supported":true,"values":["low","high"],"default":"high"}
    }))
    .unwrap()
    .remove(0);
    assert_eq!(model.display_name.as_deref(), Some("Account Flash"));
    assert_eq!(deepseek_discovered_limits(&model), (8192, 2048));
    assert!(!model.tools);
    assert!(!model.vision);
    assert_eq!(model.structured_output, Some(false));
    let reasoning = discovered_reasoning_capability(
        declaration,
        Protocol::OpenAiChat,
        &model.id,
        &model.reasoning_metadata,
    )
    .unwrap();
    assert_eq!(reasoning.options.as_ref().unwrap().values, ["low", "high"]);
    assert_eq!(
        reasoning.options.as_ref().unwrap().default.as_deref(),
        Some("high")
    );
    assert!(!reasoning.supports(&ReasoningConfig::Off));
    for reasoning in [
        serde_json::json!(false),
        serde_json::Value::Null,
        serde_json::json!({"control":"future-control"}),
    ] {
        let model = parse(serde_json::json!({"id":"deepseek-flash", "reasoning":reasoning}))
            .unwrap()
            .remove(0);
        assert!(discovered_reasoning_capability(
            declaration,
            Protocol::OpenAiChat,
            &model.id,
            &model.reasoning_metadata
        )
        .is_none());
        // Limits are independent of the reasoning assertion: the endpoint
        // publishes none, so the documented window applies and reasoning still
        // refuses to be invented from the snapshot.
        assert_eq!(model.context_window, Some(1_000_000));
        assert_eq!(model.max_output_tokens, Some(393_216));
        assert_eq!(deepseek_discovered_limits(&model), (1_000_000, 393_216));
    }
    for reasoning in [
        serde_json::json!("yes"),
        serde_json::json!({"supported":"yes"}),
        serde_json::json!({"supported":true,"values":["low","unknown"]}),
    ] {
        assert!(parse(serde_json::json!({"id":"deepseek-flash", "reasoning":reasoning})).is_err());
    }
    for value in [
        serde_json::Value::Null,
        serde_json::json!("malformed"),
        serde_json::json!(false),
    ] {
        let model = parse(serde_json::json!({"id":"deepseek-flash", "tools":value,
            "context_window":value, "max_output_tokens":value, "input_modalities":value,
            "structured_output":value}))
        .unwrap()
        .remove(0);
        assert_eq!(model.context_window, None);
        assert_eq!(model.max_output_tokens, None);
        assert!(!model.tools);
        assert!(!model.vision);
        assert_eq!(model.structured_output, Some(false));
    }
    let model = parse(serde_json::json!({"id":"deepseek-flash", "capabilities":{
        "reasoning":false, "tools":false, "structured_output":false}}))
    .unwrap()
    .remove(0);
    assert!(!model.tools);
    assert_eq!(model.reasoning_metadata.supported, Some(false));
}

#[test]
fn pinned_metadata_is_provider_and_protocol_scoped_and_preserves_cerebras_default() {
    let body = serde_json::json!({"data":[{"id":"deepseek-flash"}]});
    for declaration in [
        None,
        Some(&crate::providers::OPENAI),
        Some(openrouter_declaration()),
    ] {
        let model = api_models_from_response_for(&body, declaration)
            .unwrap()
            .remove(0);
        assert_eq!(model.context_window, None);
        assert_eq!(model.display_name, None);
        assert!(model.reasoning_metadata.options.is_none());
    }
    let declaration = &crate::providers::DEEPSEEK;
    let model = api_models_from_response_for(&body, Some(declaration))
        .unwrap()
        .remove(0);
    assert_eq!(model.display_name.as_deref(), Some("DeepSeek V4.1 Flash"));
    let reasoning = discovered_reasoning_capability(
        declaration,
        Protocol::OpenAiChat,
        &model.id,
        &model.reasoning_metadata,
    )
    .unwrap();
    assert_eq!(
        reasoning.openai_chat_mode,
        OpenAiChatReasoningMode::DeepSeekThinking
    );
    assert_eq!(
        reasoning.options.as_ref().unwrap().values,
        ["none", "low", "high", "max"]
    );
    assert!(discovered_reasoning_capability(
        declaration,
        Protocol::OpenAiResponses,
        &model.id,
        &model.reasoning_metadata,
    )
    .is_none());
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == "cerebras")
        .unwrap();
    let model = api_models_from_response_for(
        &serde_json::json!({"data":[{"id":"qwen-3.8-27b"}]}),
        Some(declaration),
    )
    .unwrap()
    .remove(0);
    // The scoped snapshot names Cerebras' inventory row and, because the
    // endpoint publishes no limit at all, supplies that row's documented
    // window. Reasoning still comes only from the source contract.
    assert_eq!(model.context_window, Some(131_072));
    assert_eq!(model.max_output_tokens, Some(40_960));
    assert_eq!(model.display_name.as_deref(), Some("Qwen3.8 27B"));
    assert_eq!(model.reasoning_metadata.supported, None);
    assert!(model.reasoning_metadata.options.is_none());
    let reasoning = discovered_reasoning_capability(
        declaration,
        Protocol::OpenAiChat,
        &model.id,
        &model.reasoning_metadata,
    )
    .unwrap();
    assert_eq!(
        reasoning.openai_chat_mode,
        OpenAiChatReasoningMode::Cerebras
    );
    assert_eq!(
        reasoning.options.as_ref().unwrap().values,
        ["none", "low", "medium", "high"]
    );
    assert_eq!(
        reasoning.options.as_ref().unwrap().default.as_deref(),
        Some("high")
    );
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &serde_json::json!({"data":[{"id":"qwen-3.8-27b"}]}),
    )
    .unwrap();
    let registered = catalog
        .resolve(&ModelId("cerebras/qwen-3.8-27b".into()))
        .unwrap();
    // Registration uses this model's documented row (128K context / 40K
    // output) instead of the generic 128K/64K placeholder, because the endpoint
    // publishes no limit for it.
    assert_eq!(registered.spec.limits.context_window, 131_072);
    assert_eq!(registered.spec.limits.max_output_tokens, 40_960);
    assert_eq!(
        registered.spec.capabilities.reasoning.as_ref(),
        Some(&reasoning)
    );
    // An effort array in an external catalog is not a universal wire contract.
    let groq = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == "groq")
        .unwrap();
    let model = api_models_from_response_for(&body, Some(groq))
        .unwrap()
        .remove(0);
    assert!(discovered_reasoning_capability(
        groq,
        Protocol::OpenAiChat,
        &model.id,
        &model.reasoning_metadata,
    )
    .is_none());
}

#[test]
fn pinned_metadata_openrouter_uses_endpoint_limits_and_fails_closed_for_invalid_prices() {
    let declaration = openrouter_declaration();
    let parse =
        |entry| openrouter_models_from_response(declaration, &serde_json::json!({"data":[entry]}));
    let model = parse(serde_json::json!({
        "id":"deepseek/deepseek-v4-pro",
        "top_provider":{"max_completion_tokens":65536}
    }))
    .unwrap()
    .remove(0);
    assert_eq!(model.limits.context_window, 131_072);
    assert_eq!(model.limits.max_output_tokens, 65_536);
    assert!(model.capabilities.reasoning.is_none());
    assert!(model.pricing.is_some());
    for value in [
        serde_json::Value::Null,
        serde_json::json!({"prompt":"unknown","completion":"0.5"}),
    ] {
        let model = parse(serde_json::json!({
            "id":"deepseek/deepseek-v4-pro",
            "top_provider":{"max_completion_tokens":65536},
            "pricing":value
        }))
        .unwrap()
        .remove(0);
        assert!(model.pricing.is_none());
    }
    assert!(parse(serde_json::json!({
        "id":"deepseek/deepseek-v4-pro",
        "top_provider":{"max_completion_tokens":null}
    }))
    .unwrap()
    .is_empty());
}

#[tokio::test]
async fn pinned_metadata_deepseek_flash_exact_wire_controls_and_required_replay() {
    use octet_ai::{
        AssistantPart, Message, Request, ToolResult, ToolResultPart, UserMessage, UserPart,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let stream = concat!(
        "data: {\"id\":\"synthetic\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Inspect the result.\"}}]}\n\n",
        "data: {\"id\":\"synthetic\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: {\"id\":\"synthetic\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream),
        )
        .expect(4)
        .mount(&server)
        .await;
    let declaration = &crate::providers::DEEPSEEK;
    let mut catalog = metadata_fixture_catalog(declaration, &format!("{}/", server.uri()));
    register_deepseek_models_from_response(
        &mut catalog,
        declaration,
        &serde_json::json!({"data":[{"id":"deepseek-flash"}]}),
    )
    .unwrap();
    let model = catalog
        .resolve(&ModelId("deepseek/deepseek-flash".into()))
        .unwrap();
    let mut request = Request {
        system: Some("System contract".into()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Look up".into())],
        })],
        tools: vec![ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "lookup".into(),
            description: "lookup".into(),
            parameters: serde_json::json!({"type":"object"}),
        }],
        tool_choice: octet_ai::ToolChoice::Auto,
        max_output_tokens: Some(4096),
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low),
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: octet_ai::OutputFormat::Text,
        output_modalities: octet_ai::OutputModalities::Text,
        compatibility: octet_ai::CompatibilityMode::Strict,
        cache_retention: octet_ai::CacheRetention::None,
        session_id: None,
    };
    let client = AiClient::new();
    let response = client.complete(&model, request.clone()).await.unwrap();
    let call = response
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::ToolCall(c) => Some(c.clone()),
            _ => None,
        })
        .unwrap();
    request.messages.push(Message::Assistant(response.message));
    request.messages.push(Message::User(UserMessage {
        content: vec![UserPart::ToolResult(ToolResult {
            tool_call_id: call.id,
            content: vec![ToolResultPart::Text("fixture result".into())],
            is_error: false,
            added_tool_names: None,
        })],
    }));
    for choice in [
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        ReasoningConfig::Off,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max),
    ] {
        request.reasoning = choice;
        client.complete(&model, request.clone()).await.unwrap();
    }
    request.reasoning = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium);
    assert!(client.complete(&model, request).await.is_err());
    let posts = server.received_requests().await.unwrap();
    for (index, effort) in [Some("low"), Some("high"), None, Some("max")]
        .into_iter()
        .enumerate()
    {
        let post: serde_json::Value = posts[index].body_json().unwrap();
        assert!(posts[index].headers.get("authorization").is_none());
        assert_eq!(post["model"], "deepseek-flash");
        assert_eq!(
            post["thinking"]["type"],
            if effort.is_some() {
                "enabled"
            } else {
                "disabled"
            }
        );
        assert_eq!(
            post.get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            effort
        );
        if index > 0 {
            assert_eq!(
                post["messages"][2]["reasoning_content"],
                "Inspect the result."
            );
        }
        for key in [
            "enable_thinking",
            "chat_template_kwargs",
            "preserve_thinking",
        ] {
            assert!(post.get(key).is_none());
        }
    }
}

#[test]
fn pinned_metadata_partial_limit_leaves_are_independent() {
    for (limit, discovered, effective) in [
        (
            serde_json::json!({"context":1_000_000}),
            (Some(1_000_000), None),
            (1_000_000, 64_000),
        ),
        (
            serde_json::json!({"output":384_000}),
            (None, Some(384_000)),
            (128_000, 128_000),
        ),
        (
            serde_json::json!({"context":96_000,"output":8192}),
            (Some(96_000), Some(8192)),
            (96_000, 8192),
        ),
        (serde_json::json!({}), (None, None), (128_000, 64_000)),
        (
            serde_json::json!({"context":null}),
            (None, None),
            (128_000, 64_000),
        ),
        (
            serde_json::json!({"output":null}),
            (None, None),
            (128_000, 64_000),
        ),
        (
            serde_json::json!({"context":64_000,"output":null}),
            (Some(64_000), None),
            (64_000, 64_000),
        ),
        (
            serde_json::json!({"context":null,"output":2048}),
            (None, Some(2048)),
            (128_000, 2048),
        ),
        (serde_json::Value::Null, (None, None), (128_000, 64_000)),
        (
            serde_json::json!("malformed"),
            (None, None),
            (128_000, 64_000),
        ),
        (serde_json::json!(false), (None, None), (128_000, 64_000)),
    ] {
        let model = api_models_from_response_for(
            &serde_json::json!({"data":[{"id":"deepseek-flash", "limit":limit}]}),
            Some(&crate::providers::DEEPSEEK),
        )
        .unwrap()
        .remove(0);
        // Only the asserted leaf is decoded; the snapshot supplies neither.
        assert_eq!((model.context_window, model.max_output_tokens), discovered);
        // Registration independently applies existing defaults and the context
        // clamp, not the snapshot's 1M / 384K limits.
        assert_eq!(deepseek_discovered_limits(&model), effective);
    }
}

#[test]
fn pinned_metadata_production_deepseek_alias_follows_admitted_inventory_and_config_limits() {
    let declaration = &crate::providers::DEEPSEEK;
    for api_name in [DEEPSEEK_MODEL_ID, "deepseek-flash"] {
        let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
        // The production sequence is endpoints -> discovery -> historical alias.
        register_deepseek_models_from_response(
            &mut catalog,
            declaration,
            &serde_json::json!({"data":[{"id":api_name}]}),
        )
        .unwrap();
        let discovered = discovered_deepseek_spec(&catalog, declaration, api_name).unwrap();
        register_deepseek_legacy_alias(
            &mut catalog,
            declaration,
            api_name,
            discovered.limits.context_window,
            discovered.limits.max_output_tokens,
        )
        .unwrap();
        let legacy = catalog.resolve(&ModelId(DEEPSEEK_MODEL_ID.into())).unwrap();
        assert_eq!(legacy.spec.api_name, api_name);
        // Legacy V4 and the current Flash alias have different source defaults
        // and controls; matching display names must not conflate their contracts.
        // Both aliases now carry the refreshed pinned 1M/393,216 limits
        // because the endpoint publishes none; their reasoning contracts still
        // differ and must not be conflated.
        let (context, output, values, default) = if api_name == "deepseek-flash" {
            (1_000_000, 393_216, vec!["none", "low", "high", "max"], None)
        } else {
            (
                1_000_000,
                393_216,
                vec!["none", "high", "xhigh"],
                Some("high"),
            )
        };
        assert_eq!(legacy.spec.limits.context_window, context);
        assert_eq!(legacy.spec.limits.max_output_tokens, output);
        let reasoning = legacy.spec.capabilities.reasoning.as_ref().unwrap();
        assert_eq!(
            reasoning.openai_chat_mode,
            OpenAiChatReasoningMode::DeepSeekThinking
        );
        assert!(reasoning.preserves_state);
        assert_eq!(reasoning.options.as_ref().unwrap().values, values);
        assert_eq!(
            reasoning.options.as_ref().unwrap().default.as_deref(),
            default
        );
        assert_eq!(
            reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max)),
            api_name == "deepseek-flash"
        );
        assert_eq!(
            reasoning.supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Xhigh)),
            api_name != "deepseek-flash"
        );
        if api_name == "deepseek-flash" {
            assert_eq!(
                legacy.spec.display_name.as_deref(),
                Some("DeepSeek V4.1 Flash")
            );
        }
    }
    let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
    register_deepseek_models_from_response(&mut catalog, declaration,
        &serde_json::json!({"data":[{"id":"deepseek-flash", "reasoning":false, "context_window":8192, "max_output_tokens":2048}]})).unwrap();
    // These values are the explicit OCTET_DEEPSEEK_MODEL/limit override inputs,
    // without mutating process environment or touching an ambient credential.
    register_deepseek_legacy_alias(&mut catalog, declaration, "deepseek-flash", 4096, 1024)
        .unwrap();
    let legacy = catalog.resolve(&ModelId(DEEPSEEK_MODEL_ID.into())).unwrap();
    assert!(legacy.spec.capabilities.reasoning.is_none());
    assert_eq!(legacy.spec.limits.context_window, 4096);
    assert_eq!(legacy.spec.limits.max_output_tokens, 1024);
    // Already configured aliases are never overwritten by subsequent defaults.
    register_deepseek_legacy_alias(
        &mut catalog,
        declaration,
        "deepseek-flash",
        1_000_000,
        384_000,
    )
    .unwrap();
    assert_eq!(
        catalog
            .resolve(&ModelId(DEEPSEEK_MODEL_ID.into()))
            .unwrap()
            .spec
            .limits
            .context_window,
        4096
    );
}

#[test]
fn pinned_metadata_raw_v1_provider_cache_upgrades_without_synthesizing_persisted_fields() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache/deepseek.json");
    let declaration = &crate::providers::DEEPSEEK;
    let fingerprint = credential_fingerprint("synthetic-cache-key");
    let url = "https://fixture.invalid/v1/models";
    for assertion in [
        serde_json::json!({}),
        serde_json::json!({"reasoning":false}),
        serde_json::json!({"reasoning":null}),
        serde_json::json!({"reasoning":{"supported":true,"values":["low","high"],"default":"high"}}),
        serde_json::json!({"reasoning":"malformed"}),
    ] {
        let mut entry = assertion.clone();
        entry["id"] = serde_json::json!("deepseek-flash");
        let body = serde_json::json!({"data":[entry]});
        // Baseline v1 cache schema, deliberately not a normalized model cache.
        crate::auth::write_private_atomic(
            &path,
            &serde_json::to_vec(&serde_json::json!({
                "version":1, "provider_id":"deepseek", "inventory_url":url,
                "credential_fingerprint":fingerprint, "body":body,
            }))
            .unwrap(),
            ".test-cache-",
        )
        .unwrap();
        let Some(CachedProviderInventory::Available(cached)) =
            load_provider_inventory_cache(&path, "deepseek", url, &fingerprint).unwrap()
        else {
            panic!("v1 cache");
        };
        let mut catalog = metadata_fixture_catalog(declaration, "https://fixture.invalid/");
        let result = register_deepseek_models_from_response(&mut catalog, declaration, &cached);
        if assertion["reasoning"] == "malformed" {
            assert!(result.is_err());
            assert_eq!(catalog.models().count(), 0);
        } else {
            result.unwrap();
            register_deepseek_legacy_alias(
                &mut catalog,
                declaration,
                "deepseek-flash",
                1_000_000,
                384_000,
            )
            .unwrap();
            let model = catalog.resolve(&ModelId(DEEPSEEK_MODEL_ID.into())).unwrap();
            assert_eq!(
                model.spec.display_name.as_deref(),
                Some("DeepSeek V4.1 Flash")
            );
            let reasoning = model.spec.capabilities.reasoning.as_ref();
            if assertion.get("reasoning").is_none() {
                assert_eq!(
                    reasoning.unwrap().options.as_ref().unwrap().values,
                    ["none", "low", "high", "max"]
                );
            } else if assertion["reasoning"].is_object() {
                assert_eq!(
                    reasoning.unwrap().options.as_ref().unwrap().values,
                    ["low", "high"]
                );
                assert_eq!(
                    reasoning
                        .unwrap()
                        .options
                        .as_ref()
                        .unwrap()
                        .default
                        .as_deref(),
                    Some("high")
                );
            } else {
                assert!(reasoning.is_none());
            }
        }
        assert_eq!(cached, body);
        save_provider_inventory_cache(&path, "deepseek", url, &fingerprint, Some(&cached)).unwrap();
        let Some(CachedProviderInventory::Available(reloaded)) =
            load_provider_inventory_cache(&path, "deepseek", url, &fingerprint).unwrap()
        else {
            panic!("saved v1 cache");
        };
        assert_eq!(reloaded, body);
        assert!(reloaded["data"][0].get("name").is_none());
        assert!(reloaded["data"][0].get("context_window").is_none());
    }
}

#[test]
fn pinned_metadata_discovery_precedes_generic_and_messages_static_fallbacks() {
    for (provider, api_name, messages) in [
        ("opencode", "deepseek-v4-pro", false),
        ("minimax", "MiniMax-M2.7", true),
    ] {
        let declaration = BUILTIN_PROVIDER_DECLARATIONS
            .iter()
            .find(|d| d.id == provider)
            .unwrap();
        let mut catalog = ModelCatalog::default();
        for route in declaration.routes {
            let id = EndpointId(route.endpoint_id.into());
            if !catalog.has_endpoint(&id) {
                catalog
                    .register_endpoint(Endpoint {
                        id,
                        base_url: url::Url::parse("https://fixture.invalid/").unwrap(),
                        auth: Auth::None,
                        default_headers: Default::default(),
                        transport: route.transport,
                        runtime: route.runtime,
                        timeout: Duration::from_secs(5),
                    })
                    .unwrap();
            }
        }
        let body = serde_json::json!({"data":[{"id":api_name,"name":"Account model", "context_window":8192,
            "max_output_tokens":2048, "tools":false,"structured_output":false,"reasoning":false}]});
        if messages {
            register_anthropic_compatible_models_from_response(
                &mut catalog,
                declaration,
                ModelFilter::All,
                &body,
            )
            .unwrap();
        } else {
            register_openai_compatible_models_from_response(
                &mut catalog,
                declaration,
                ModelFilter::All,
                &body,
            )
            .unwrap();
        }
        crate::providers::register_static_models(&mut catalog, declaration).unwrap();
        let model = catalog
            .resolve(&ModelId(format!("{provider}/{api_name}")))
            .unwrap();
        assert_eq!(model.spec.display_name.as_deref(), Some("Account model"));
        assert_eq!(model.spec.limits.context_window, 8192);
        assert!(!model.spec.capabilities.tools);
        assert!(!model.spec.capabilities.structured_output);
        assert!(model.spec.capabilities.reasoning.is_none());
    }
}

#[test]
fn pinned_metadata_sparse_static_routes_keep_their_declared_wire_profiles() {
    for (provider, api_name, protocol) in [
        ("opencode", "deepseek-v4-pro", Protocol::OpenAiChat),
        ("minimax", "MiniMax-M2.7", Protocol::AnthropicMessages),
    ] {
        let declaration = BUILTIN_PROVIDER_DECLARATIONS
            .iter()
            .find(|d| d.id == provider)
            .unwrap();
        let expected = declaration
            .static_reasoning_for(api_name, protocol)
            .unwrap();
        let model = api_models_from_response_for(
            &serde_json::json!({"data":[{"id":api_name}]}),
            Some(declaration),
        )
        .unwrap()
        .remove(0);
        let reasoning = discovered_reasoning_capability(
            declaration,
            protocol,
            api_name,
            &model.reasoning_metadata,
        )
        .unwrap();
        assert_eq!(reasoning.openai_chat_mode, expected.openai_chat_mode);
        assert_eq!(reasoning.control, expected.control);
    }
}

#[test]
fn pinned_metadata_native_discovery_rejects_budgets_outside_output_limit() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == "opencode")
        .unwrap();
    let api_name = "claude-sonnet-4-5";
    let known = declaration
        .static_reasoning_for(api_name, Protocol::AnthropicMessages)
        .unwrap();
    assert_eq!(known.effort_budgets.as_ref().unwrap().max, 32_768);
    for messages in [false, true] {
        let default_context = if messages { 200_000 } else { 128_000 };
        // With no endpoint limit at all, the pinned row supplies this model's
        // documented window (Claude Sonnet 4.5 is 1M context / 64K output).
        let pinned_context = 1_000_000;
        let pinned_output = 64_000;
        for (context, output, effective_output, fits) in [
            // The pinned output (64K) covers the declared 32K budget table.
            (None, None, pinned_output, true),
            (None, Some(32_768), 32_768, false),
            (None, Some(32_769), 32_769, true),
            (None, Some(2048), 2048, false),
            (Some(8192), Some(64_000), 8192, false),
            (Some(128_000), Some(64_000), 64_000, true),
        ] {
            let mut catalog = ModelCatalog::default();
            for route in declaration.routes {
                let id = EndpointId(route.endpoint_id.into());
                if !catalog.has_endpoint(&id) {
                    catalog
                        .register_endpoint(Endpoint {
                            id,
                            base_url: url::Url::parse("https://fixture.invalid/").unwrap(),
                            auth: Auth::None,
                            default_headers: Default::default(),
                            transport: route.transport,
                            runtime: route.runtime,
                            timeout: Duration::from_secs(5),
                        })
                        .unwrap();
                }
            }
            let mut body = serde_json::json!({"data":[{"id":api_name,"reasoning":true}]});
            if let Some(context) = context {
                body["data"][0]["context_window"] = serde_json::json!(context);
            }
            if let Some(output) = output {
                body["data"][0]["max_output_tokens"] = serde_json::json!(output);
            }
            if messages {
                register_anthropic_compatible_models_from_response(
                    &mut catalog,
                    declaration,
                    ModelFilter::All,
                    &body,
                )
                .unwrap();
            } else {
                register_openai_compatible_models_from_response(
                    &mut catalog,
                    declaration,
                    ModelFilter::All,
                    &body,
                )
                .unwrap();
            }
            crate::providers::register_static_models(&mut catalog, declaration).unwrap();
            let model = catalog
                .resolve(&ModelId(format!("opencode/{api_name}")))
                .unwrap();
            assert_eq!(model.spec.protocol, Protocol::AnthropicMessages);
            assert_eq!(
                model.spec.limits.context_window,
                context.unwrap_or(if output.is_none() {
                    pinned_context
                } else {
                    default_context
                })
            );
            assert_eq!(model.spec.limits.max_output_tokens, effective_output);
            // Preserve the full declaration when it fits; otherwise retain the
            // inventory row without raising limits or inventing a budget table.
            assert_eq!(
                model.spec.capabilities.reasoning.as_ref(),
                fits.then_some(&known)
            );
        }
    }
}

#[test]
fn pinned_metadata_native_discovery_narrows_exact_choices_without_changing_codec() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == "opencode")
        .unwrap();
    for api_name in [
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-unknown-fixture",
    ] {
        for (values, expected) in [
            (serde_json::json!(["low", "high"]), true),
            (serde_json::json!(["low", "ultra"]), false),
            (serde_json::json!(["none", "default"]), false),
        ] {
            let mut catalog = ModelCatalog::default();
            for route in declaration.routes {
                let id = EndpointId(route.endpoint_id.into());
                if !catalog.has_endpoint(&id) {
                    catalog
                        .register_endpoint(Endpoint {
                            id,
                            base_url: url::Url::parse("https://fixture.invalid/").unwrap(),
                            auth: Auth::None,
                            default_headers: Default::default(),
                            transport: route.transport,
                            runtime: route.runtime,
                            timeout: Duration::from_secs(5),
                        })
                        .unwrap();
                }
            }
            let default = values[0].clone();
            register_openai_compatible_models_from_response(
                &mut catalog,
                declaration,
                ModelFilter::All,
                &serde_json::json!({"data":[{"id":api_name,"max_output_tokens":64_000,
                    "reasoning":{"supported":true,"values":values,"default":default}}]}),
            )
            .unwrap();
            crate::providers::register_static_models(&mut catalog, declaration).unwrap();
            let model = catalog
                .resolve(&ModelId(format!("opencode/{api_name}")))
                .unwrap();
            assert_eq!(model.spec.protocol, Protocol::AnthropicMessages);
            assert_eq!(model.spec.limits.context_window, 128_000);
            assert_eq!(model.spec.limits.max_output_tokens, 64_000);
            let known = declaration.static_reasoning_for(api_name, Protocol::AnthropicMessages);
            if let Some(mut expected) = known.filter(|_| expected) {
                expected.options = Some(octet_ai::types::ReasoningOptions {
                    values: vec!["low".into(), "high".into()],
                    default: Some("low".into()),
                });
                let actual = model.spec.capabilities.reasoning.as_ref().unwrap();
                assert_eq!(
                    serde_json::to_value(actual).unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
                assert_eq!(
                    actual.default_selection(),
                    Some(ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low))
                );
                for forbidden in [
                    ReasoningConfig::Off,
                    ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium),
                    ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max),
                ] {
                    assert!(!actual.supports(&forbidden));
                }
            } else {
                assert!(
                    model.spec.capabilities.reasoning.is_none(),
                    "{api_name}: {values}"
                );
            }
        }
    }
}
