//! Bootstrap-bound self-description regressions, with no live credentials.
use super::*;
use serde_json::{json, Value};

fn declaration(id: &str) -> &'static ProviderDeclaration {
    BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == id)
        .unwrap()
}

fn entry(protocol: Protocol, id: &str) -> Value {
    json!({"id":id,"octet_capabilities":{"version":1,"protocol":protocol,
        "context_window":96000,"max_output_tokens":8192,"input_modalities":["text","image"],
        "tools":true,"parallel_tool_calls":true,"structured_output":true,
        "reasoning":{"values":["none","low","high"],"default":"high"}}})
}

fn catalog(declaration: &ProviderDeclaration) -> ModelCatalog {
    let mut catalog = ModelCatalog::default();
    for route in declaration.routes {
        if catalog.has_endpoint(&EndpointId(route.endpoint_id.into())) {
            continue;
        }
        catalog
            .register_endpoint(Endpoint {
                id: EndpointId(route.endpoint_id.into()),
                base_url: url::Url::parse("https://fixture.invalid/v1/").unwrap(),
                auth: Auth::None,
                default_headers: Default::default(),
                transport: EndpointTransport::Http,
                runtime: Default::default(),
                timeout: Duration::from_secs(5),
            })
            .unwrap();
    }
    catalog
}

#[test]
fn unknown_models_use_self_description_without_snapshot_or_route_guessing() {
    for provider in ["groq", "openai", "openrouter"] {
        let declaration = declaration(provider);
        let id = "future-family-unlisted-model";
        assert!(octet_ai::model_metadata::model_capability_metadata(provider, id).is_none());
        let route = declaration.route_for_model(id).unwrap();
        let mut catalog = catalog(declaration);
        let body = json!({"data":[entry(route.protocol, id)]});
        if provider == "openrouter" {
            register_openrouter_models_from_response(&mut catalog, declaration, &body).unwrap();
        } else {
            register_openai_compatible_models_from_response(
                &mut catalog,
                declaration,
                ModelFilter::All,
                &body,
            )
            .unwrap();
        }
        let model = catalog
            .resolve(&ModelId(format!("{provider}/{id}")))
            .unwrap();
        assert_eq!(model.endpoint.id.0, route.endpoint_id);
        assert_eq!(model.spec.protocol, route.protocol);
        assert_eq!(model.spec.limits.context_window, 96000);
        assert_eq!(model.spec.limits.max_output_tokens, 8192);
        let caps = &model.spec.capabilities;
        assert!(caps.tools && caps.parallel_tool_calls && caps.structured_output);
        assert!(caps.input_modalities.contains(octet_ai::Modality::Image));
        assert!(!caps.responses_lite && caps.agent_delegation.is_none());
        assert!(!caps
            .reasoning
            .as_ref()
            .unwrap()
            .supports(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium)));
        assert!(model.spec.pricing.is_none());
    }
}

#[test]
fn legacy_assertions_and_configured_catalog_models_win_per_leaf() {
    let declaration = declaration("groq");
    for value in [
        json!(false),
        Value::Null,
        json!("unknown"),
        json!({"malformed":true}),
    ] {
        let mut entry = entry(Protocol::OpenAiChat, "fixture");
        entry["capabilities"] = json!({"tools":value,"vision":value,"structured_output":value,"parallel_tool_calls":value});
        entry["reasoning"] = Value::Null;
        entry["context_window"] = json!(16384);
        let mut catalog = catalog(declaration);
        register_openai_compatible_models_from_response(
            &mut catalog,
            declaration,
            ModelFilter::All,
            &json!({"data":[entry]}),
        )
        .unwrap();
        let model = catalog.resolve(&ModelId("groq/fixture".into())).unwrap();
        let caps = &model.spec.capabilities;
        assert!(!caps.tools && !caps.parallel_tool_calls && !caps.structured_output);
        assert!(!caps.input_modalities.contains(octet_ai::Modality::Image));
        assert!(caps.reasoning.is_none());
        assert_eq!(model.spec.limits.context_window, 16384);
        assert_eq!(model.spec.limits.max_output_tokens, 8192);
    }
    let mut catalog = catalog(declaration);
    let original = json!({"data":[{"id":"configured","context_window":12345,"max_output_tokens":1234,"tools":false,"vision":false}]});
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &original,
    )
    .unwrap();
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &json!({"data":[entry(Protocol::OpenAiChat,"configured")]}),
    )
    .unwrap();
    let model = catalog.resolve(&ModelId("groq/configured".into())).unwrap();
    assert_eq!(model.spec.limits.context_window, 12345);
    assert!(!model.spec.capabilities.tools);
}

#[test]
fn malformed_or_native_declarations_do_not_register_partial_inventories() {
    let declaration = declaration("groq");
    for field in [
        json!(null),
        json!({"version":1}),
        entry(Protocol::MistralConversations, "bad")["octet_capabilities"].clone(),
    ] {
        let mut catalog = catalog(declaration);
        let body = json!({"data":[entry(Protocol::OpenAiChat,"valid"),{"id":"bad","octet_capabilities":field}]});
        assert!(register_openai_compatible_models_from_response(
            &mut catalog,
            declaration,
            ModelFilter::All,
            &body
        )
        .is_err());
        assert!(catalog.models().next().is_none());
    }
    let declaration = self::declaration("anthropic");
    let mut catalog = catalog(declaration);
    assert!(register_anthropic_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &json!({"data":[entry(Protocol::AnthropicMessages,"future")]})
    )
    .is_err());
    assert!(catalog.models().next().is_none());
}

#[test]
fn self_description_cannot_enable_family_fallbacks_or_bypass_filters() {
    let declaration = declaration("openai");
    let mut entry = entry(Protocol::OpenAiResponses, "gpt-6-unlisted");
    entry["octet_capabilities"] = json!({"version":1,"protocol":Protocol::OpenAiResponses,"context_window":8192,"max_output_tokens":1024});
    let mut catalog = catalog(declaration);
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &json!({"data":[entry]}),
    )
    .unwrap();
    let model = catalog
        .resolve(&ModelId("openai/gpt-6-unlisted".into()))
        .unwrap();
    assert!(!model.spec.capabilities.tools);
    assert!(!model.spec.capabilities.structured_output);
    assert!(!model
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert!(model.spec.capabilities.reasoning.is_none());
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::Prefix(&["gpt-"]),
        &json!({"data":[self::entry(Protocol::OpenAiResponses,"excluded")]}),
    )
    .unwrap();
    assert!(catalog.resolve(&ModelId("openai/excluded".into())).is_err());
}

#[test]
fn explicit_context_aliases_are_never_overwritten_by_self_description() {
    type Alias = (&'static str, fn(&mut Value));
    // Every context alias recognized by `has_metadata_assertion` for the
    // `context_window` leaf. A valid self-description fills only an absent
    // assertion, so a conflicting explicit alias must keep its own limit.
    let aliases: &[Alias] = &[
        ("context_window", |e| e["context_window"] = json!(12345)),
        ("context_length", |e| e["context_length"] = json!(12345)),
        ("max_model_len", |e| e["max_model_len"] = json!(12345)),
        ("max_context_tokens", |e| {
            e["max_context_tokens"] = json!(12345)
        }),
        ("limit/context", |e| e["limit"] = json!({"context": 12345})),
        ("meta/n_ctx", |e| e["meta"] = json!({"n_ctx": 12345})),
        ("meta/n_ctx_train", |e| {
            e["meta"] = json!({"n_ctx_train": 12345})
        }),
        ("status", |e| {
            e["status"] = json!({"args": ["--max-model-len", "12345"]})
        }),
    ];
    for (name, mutate) in aliases {
        let mut entry = entry(Protocol::OpenAiChat, "fixture");
        mutate(&mut entry);
        let described = self_described_entry(
            &entry,
            EndpointId("groq".into()),
            "fixture",
            Protocol::OpenAiChat,
        )
        .unwrap()
        .unwrap_or_else(|| panic!("{name}: self-description should decode"));
        assert!(
            described
                .get("context_window")
                .and_then(Value::as_u64)
                .is_none_or(|window| window != 96000),
            "{name}: self-description overwrote the explicit context assertion"
        );
    }
    // Control: an absent context assertion still receives the decoded limit.
    let described = self_described_entry(
        &entry(Protocol::OpenAiChat, "fixture"),
        EndpointId("groq".into()),
        "fixture",
        Protocol::OpenAiChat,
    )
    .unwrap()
    .unwrap();
    assert_eq!(described["context_window"], json!(96000));
}

#[tokio::test]
async fn custom_discovery_round_trip_and_explicit_override_preserve_contract() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[entry(Protocol::OpenAiChat,"future-local")]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let cred: crate::auth::custom::CustomCredential =
        serde_json::from_value(json!({"base_url":format!("{}/v1/",server.uri())})).unwrap();
    let models = tokio::task::spawn_blocking(move || {
        discover_models_blocking(&cred, "fixture-provider", false)
    })
    .await
    .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].context_window, 96000);
    assert!(
        models[0].tools
            && models[0].vision
            && models[0].parallel_tool_calls
            && models[0].structured_output
    );
    assert_eq!(models[0].reasoning_values, ["none", "low", "high"]);
    assert_eq!(models[0].reasoning_default, "high");
    let configured = serde_json::from_value(json!({"api_name":"future-local","context_window":4096,"max_output_tokens":512,"tools":false,"vision":false,"reasoning":false})).unwrap();
    let merged = apply_configured_custom_model_overrides(models, &[configured]);
    assert_eq!(merged[0].context_window, 4096);
    assert!(!merged[0].tools && !merged[0].vision && !merged[0].reasoning);
}
