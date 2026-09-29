//! Discovery and capability
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[test]
fn discovered_reasoning_supports_chat_and_responses_models() {
    let openai = &crate::providers::OPENAI;
    assert!(!discovered_model_supports_reasoning(
        openai,
        Protocol::OpenAiChat,
        "gemma-4-31b-it"
    ));
    assert!(discovered_model_supports_reasoning(
        openai,
        Protocol::OpenAiResponses,
        "gpt-5.4"
    ));
    assert!(discovered_model_supports_reasoning(
        openai,
        Protocol::OpenAiResponses,
        "gpt-6-astra"
    ));
    // Discovery receives API IDs, not product catalog IDs. Do not manufacture
    // a public capability contract for a namespaced or future alias.
    assert!(!discovered_model_supports_reasoning(
        openai,
        Protocol::OpenAiResponses,
        "openai/gpt-6-astra"
    ));
    assert!(!discovered_model_supports_reasoning(
        openai,
        Protocol::OpenAiChat,
        "gemma-3-27b-it"
    ));
    assert!(!discovered_model_supports_reasoning(
        openai,
        Protocol::AnthropicMessages,
        "claude-sonnet-4"
    ));
}

#[test]
fn gpt_6_capability_fallback_is_limited_to_public_openai() {
    let openai = &crate::providers::OPENAI;
    let opencode = &crate::providers::OPENCODE;
    assert!(public_openai_gpt_6_model(openai, "gpt-6-astra"));
    assert!(!public_openai_gpt_6_model(opencode, "gpt-6-astra"));
    assert!(!discovered_model_supports_reasoning(
        opencode,
        Protocol::OpenAiResponses,
        "gpt-6-astra"
    ));
}

#[test]
fn azure_openai_configuration_routes_deployments_through_versioned_responses_base() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "azure-openai")
        .expect("Azure OpenAI declaration");
    let (base_url, deployment) = azure_openai_configuration_from_values(
        declaration,
        None,
        Some("enterprise-resource"),
        None,
        Some("production-gpt"),
    )
    .unwrap()
    .expect("Azure configuration");

    assert_eq!(deployment, "production-gpt");
    assert_eq!(
        base_url.as_str(),
        "https://enterprise-resource.openai.azure.com/openai/?api-version=2025-04-01-preview"
    );
}

#[test]
fn azure_openai_configuration_rejects_credential_bearing_endpoint_urls() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "azure-openai")
        .expect("Azure OpenAI declaration");
    let error = azure_openai_configuration_from_values(
        declaration,
        Some("https://example.invalid/?api-key=secret"),
        None,
        None,
        Some("deployment"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("invalid AZURE_OPENAI_ENDPOINT"));
}

#[test]
fn aws_runtime_registration_is_scheduled_without_a_static_environment_marker() {
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "bedrock")
        .expect("Bedrock declaration");
    assert!(declaration_is_configured(declaration).unwrap());
}

#[test]
fn codex_compaction_respects_model_window_and_allows_smaller_caps() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), None);
    let catalog = base_model_catalog(true).unwrap();
    let mut model = catalog
        .resolve(&ModelId("gpt-4o-mini".to_owned()))
        .unwrap()
        .clone();
    Arc::make_mut(&mut model.endpoint).id = EndpointId(crate::auth::codex::ENDPOINT_ID.into());
    Arc::make_mut(&mut model.spec).limits.context_window = 872_000;

    // No route default: the full provider-advertised window is available.
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        1.0
    );

    config.compaction.threshold_fraction = 0.25;
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        0.25
    );

    config.compaction.threshold_fraction = 1.0;
    config.compaction.max_active_tokens = Some(200_000);
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        200_000.0 / 872_000.0
    );

    config.compaction.max_active_tokens = Some(900_000);
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        1.0
    );

    config.compaction.max_active_tokens = Some(0);
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        1.0
    );

    config.compaction.max_active_tokens = None;
    Arc::make_mut(&mut model.endpoint).id = EndpointId("openai".into());
    assert_eq!(
        effective_compaction_threshold_fraction(&config, &model),
        1.0
    );
}

#[test]
fn custom_endpoint_startup_timeout_is_cold_start_safe_and_configurable() {
    assert_eq!(
        resolve_custom_startup_timeout(None, None).unwrap(),
        Duration::from_secs(15 * 60)
    );
    assert_eq!(
        resolve_custom_startup_timeout(Some(420), None).unwrap(),
        Duration::from_secs(420)
    );
    assert_eq!(
        resolve_custom_startup_timeout(Some(420), Some(" 600 ")).unwrap(),
        Duration::from_secs(600)
    );
    assert!(resolve_custom_startup_timeout(None, Some("0")).is_err());
    assert!(resolve_custom_startup_timeout(None, Some("not-a-number")).is_err());
}

#[test]
fn embedded_builtin_endpoints_use_provider_response_header_timeout() {
    let catalog = base_model_catalog(true).unwrap();
    for model_id in ["gpt-4o-mini", "claude-sonnet-4-6"] {
        let model = catalog.resolve(&ModelId(model_id.to_owned())).unwrap();
        assert_eq!(
            model.endpoint.timeout, PROVIDER_RESPONSE_HEADER_TIMEOUT,
            "{model_id} retained a stale embedded response-header timeout"
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]

async fn discovery_clients_do_not_follow_authenticated_redirects() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let origin = MockServer::start().await;
    let destination = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/sink", destination.uri())),
        )
        .mount(&origin)
        .await;

    let blocking_url = format!("{}/models", origin.uri());
    let blocking_status = tokio::task::spawn_blocking(move || {
        blocking_discovery_client(Duration::from_secs(2))
            .unwrap()
            .get(blocking_url)
            .header("x-api-key", "blocking-secret")
            .send()
            .unwrap()
            .status()
    })
    .await
    .unwrap();
    assert_eq!(blocking_status, reqwest::StatusCode::FOUND);

    let async_status = discovery_client(Duration::from_secs(2))
        .unwrap()
        .get(format!("{}/models", origin.uri()))
        .header("x-api-key", "async-secret")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(async_status, reqwest::StatusCode::FOUND);
    assert!(destination.received_requests().await.unwrap().is_empty());
}
