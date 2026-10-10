//! Mistral Large 4 across the providers that serve it.
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
//!
//! Sources for the pinned contract (reviewed 2026-10-07):
//! - `https://docs.mistral.ai/models/mistral-large-4-0` model card (context,
//!   price, public preview) and the official reasoning guide's
//!   `reasoning_effort` none|high contract;
//! - `GET https://openrouter.ai/api/v1/models` route
//!   `mistralai/mistral-large-4-0` (limits, rates, declared `default_effort`);
//! - `GET https://opencode.ai/zen/v1/models` route `mistral-large-4`;
//! - the checked-in models.dev snapshot for the canonical display name.
use octet_ai::{Message, Request, UserMessage, UserPart};

use super::*;

const MISTRAL_LARGE_4: &str = "mistral-large-4";
const OPENROUTER_LEAF: &str = "mistralai/mistral-large-4-0";
const CONTEXT: u64 = 524_288;
const MAX_OUTPUT: u64 = 262_144;

/// The OpenRouter listing entry captured 2026-10-07 (public route metadata, not
/// an inference capture). Rates are dollars per token, as the client consumes
/// them.
fn openrouter_listing_entry() -> serde_json::Value {
    serde_json::json!({
        "id": OPENROUTER_LEAF,
        "canonical_slug": "mistralai/mistral-large-4-0-20261006",
        "name": "Mistral: Mistral Large 4",
        "context_length": CONTEXT,
        "architecture": {
            "modality": "text+image->text",
            "input_modalities": ["text", "image"],
            "output_modalities": ["text"],
        },
        "pricing": {
            "prompt": "0.00000068",
            "completion": "0.00000209",
            "input_cache_read": "0.00000007",
        },
        "top_provider": {
            "context_length": CONTEXT,
            "max_completion_tokens": MAX_OUTPUT,
        },
        "supported_parameters": [
            "include_reasoning", "reasoning", "reasoning_effort", "response_format",
            "structured_outputs", "temperature", "tool_choice", "tools",
        ],
        "reasoning": {
            "mandatory": false,
            "default_enabled": true,
            "supported_efforts": ["high", "none"],
            "default_effort": "high",
        },
    })
}

fn declaration(id: &str) -> &'static ProviderDeclaration {
    BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == id)
        .unwrap_or_else(|| panic!("{id} declaration"))
}

fn fixture_catalog(provider: &str, base_url: &str) -> (ModelCatalog, &'static ProviderDeclaration) {
    let declaration = declaration(provider);
    let mut catalog = ModelCatalog::default();
    // Declaration-owned routing with a fixture credential: this test owns the
    // catalog contract and wire encoding, not credential resolution.
    crate::providers::register_private_endpoints_at_base_url(
        &mut catalog,
        declaration,
        Auth::bearer("fixture-key"),
        &url::Url::parse(base_url).unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    (catalog, declaration)
}

fn high() -> ReasoningConfig {
    ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
}

fn fixture_request(reasoning: ReasoningConfig) -> Request {
    serde_json::from_value(serde_json::json!({
        "messages": [Message::User(UserMessage {
            content: vec![UserPart::Text("Prove it.".into())],
        })],
        "tools": [], "tool_choice": octet_ai::ToolChoice::Auto, "stop": [],
        "reasoning": reasoning,
        "output_modalities": octet_ai::OutputModalities::Text,
        "compatibility": octet_ai::CompatibilityMode::Strict,
        "cache_retention": octet_ai::CacheRetention::None,
    }))
    .unwrap()
}

/// One loopback OpenAI-compatible route answering every chat completion with a
/// single streaming turn.
async fn chat_fixture_server() -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"}}]}\n\n",
                    "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                )),
        )
        .mount(&server)
        .await;
    server
}

async fn submitted_body(
    server: &wiremock::MockServer,
    model: &Model,
    reasoning: ReasoningConfig,
) -> serde_json::Value {
    let before = server.received_requests().await.unwrap().len();
    AiClient::new()
        .complete(model, fixture_request(reasoning))
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), before + 1, "one request per submitted turn");
    requests.last().unwrap().body_json().unwrap()
}

#[tokio::test]
async fn mistral_direct_route_publishes_mistral_large_4_with_default_high() {
    let server = chat_fixture_server().await;
    let (mut catalog, declaration) = fixture_catalog("mistral", &format!("{}/v1/", server.uri()));
    crate::providers::register_static_models(&mut catalog, declaration).unwrap();
    let model = catalog
        .resolve(&ModelId(format!("mistral/{MISTRAL_LARGE_4}")))
        .unwrap();

    assert_eq!(model.spec.api_name, MISTRAL_LARGE_4);
    assert_eq!(model.spec.protocol, Protocol::OpenAiChat);
    assert_eq!(model.spec.limits.context_window, CONTEXT);
    assert_eq!(model.spec.limits.max_output_tokens, MAX_OUTPUT);
    assert!(model
        .spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
    // Mistral documents `reasoning_effort` as none|high for this model and
    // recommends high, so the declaration's exact choices are Off and High and
    // an untouched session selects High instead of the lowest generic level.
    assert_eq!(reasoning.choices(), [ReasoningConfig::Off, high()]);
    assert_eq!(reasoning.default_selection(), Some(high()));
    assert_eq!(default_reasoning_for_model(&model), high());

    let body = submitted_body(&server, &model, default_reasoning_for_model(&model)).await;
    assert_eq!(body["model"], MISTRAL_LARGE_4);
    assert_eq!(body["reasoning_effort"], "high");

    // An explicit user choice still wins over the declaration default.
    let off = submitted_body(&server, &model, ReasoningConfig::Off).await;
    assert!(off.get("reasoning_effort").is_none(), "{off}");
}

#[tokio::test]
async fn opencode_sparse_inventory_adopts_the_declared_mistral_large_4_contract() {
    let server = chat_fixture_server().await;
    let (mut catalog, declaration) = fixture_catalog("opencode", &format!("{}/v1/", server.uri()));
    // Zen publishes identifiers only, so the declaration owns the wire contract.
    register_openai_compatible_models_from_response(
        &mut catalog,
        declaration,
        ModelFilter::All,
        &serde_json::json!({"data": [{"id": MISTRAL_LARGE_4}]}),
    )
    .unwrap();
    crate::providers::register_static_models(&mut catalog, declaration).unwrap();
    let model = catalog
        .resolve(&ModelId(format!("opencode/{MISTRAL_LARGE_4}")))
        .unwrap();
    assert_eq!(model.spec.limits.context_window, CONTEXT);
    assert_eq!(model.spec.limits.max_output_tokens, MAX_OUTPUT);
    let reasoning = model.spec.capabilities.reasoning.as_ref().unwrap();
    assert_eq!(reasoning.choices(), [ReasoningConfig::Off, high()]);
    assert_eq!(default_reasoning_for_model(&model), high());

    let body = submitted_body(&server, &model, default_reasoning_for_model(&model)).await;
    assert_eq!(body["model"], MISTRAL_LARGE_4);
    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn openrouter_listing_keeps_the_declared_mistral_large_4_default_and_rates() {
    let response = serde_json::json!({"data": [openrouter_listing_entry()]});
    let models = openrouter_models_from_response(declaration("openrouter"), &response).unwrap();
    let model = &models[0];

    assert_eq!(model.api_name, OPENROUTER_LEAF);
    assert_eq!(model.limits.context_window, CONTEXT);
    assert_eq!(model.limits.max_output_tokens, MAX_OUTPUT);
    assert!(model
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert_eq!(
        model.display_name.as_deref(),
        Some("Mistral: Mistral Large 4")
    );
    let reasoning = model.capabilities.reasoning.as_ref().unwrap();
    // The endpoint's own effort list is `["high", "none"]` with `high`
    // declared as its default, so the declared choice set is High and Off in
    // endpoint order and an untouched session selects High.
    assert_eq!(reasoning.choices(), [high(), ReasoningConfig::Off]);
    assert_eq!(reasoning.default_selection(), Some(high()));
    let pricing = model.pricing.as_ref().expect("advertised route rate");
    assert_eq!(pricing.input, TokenRate(680_000));
    assert_eq!(pricing.output, TokenRate(2_090_000));
    assert_eq!(pricing.cache_read, TokenRate(70_000));
}

#[test]
fn mistral_large_4_rates_and_display_name_are_pinned_for_every_route() {
    // Direct Mistral does not appear in the models.dev route table, so its
    // reviewed rates live in octet's own profile.
    let direct = crate::providers::pricing_for(declaration("mistral"), MISTRAL_LARGE_4)
        .expect("direct Mistral Large 4 rates");
    assert_eq!(direct.input, TokenRate(680_000));
    assert_eq!(direct.output, TokenRate(2_090_000));
    assert_eq!(direct.cache_read, TokenRate(70_000));
    assert_eq!(direct.cache_write_5m, TokenRate(0));

    // Discovered routes without published rates fall back to the checked-in
    // models.dev projection.
    for (provider, id) in [
        ("openrouter", OPENROUTER_LEAF),
        ("opencode", MISTRAL_LARGE_4),
    ] {
        let pinned = octet_ai::model_metadata::model_pricing(provider, id)
            .unwrap_or_else(|| panic!("{provider}/{id} pinned rates"));
        assert_eq!(pinned.input, TokenRate(680_000));
        assert_eq!(pinned.output, TokenRate(2_090_000));
        assert_eq!(pinned.cache_read, TokenRate(70_000));
    }
    assert_eq!(
        octet_ai::model_metadata::model_display_name("mistral/mistral-large-4").as_deref(),
        Some("Mistral Large 4")
    );
}
