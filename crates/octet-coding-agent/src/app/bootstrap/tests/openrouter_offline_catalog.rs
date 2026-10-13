//! Openrouter offline catalog
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[test]
fn offline_explicit_openrouter_model_uses_conservative_fallback_metadata() {
    let mut catalog = ModelCatalog::builtin().unwrap();
    register_test_openrouter_endpoint(&mut catalog);
    assert!(catalog.has_endpoint(&EndpointId("openrouter".into())));

    assert!(register_offline_openrouter_model(&mut catalog, "openrouter/openai/gpt-4o").unwrap());
    let model = catalog
        .resolve(&ModelId("openrouter/openai/gpt-4o".into()))
        .unwrap();
    assert_eq!(model.spec.api_name, "openai/gpt-4o");
    assert_eq!(model.spec.protocol, Protocol::OpenAiChat);
    assert_eq!(model.spec.limits.context_window, 131_072);
    assert!(
        !register_offline_openrouter_model(&mut catalog, "openrouter/openai/gpt-4o/extra").unwrap()
    );
}

#[test]
fn offline_openrouter_catalog_resolves_a_matching_cached_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("openrouter-models.json");
    let declaration = openrouter_declaration();
    let inventory_url = url::Url::parse(declaration.base_url)
        .unwrap()
        .join("models")
        .unwrap()
        .to_string();
    let credential = "cached-openrouter-key";
    let body = serde_json::json!({
        "data": [{
            "id": "cache-test/model",
            "context_length": 64_000,
            "top_provider": {"max_completion_tokens": 8_000},
            "pricing": {"prompt": "0.000001", "completion": "0.000002"}
        }]
    });
    save_provider_inventory_cache(
        &path,
        declaration.id,
        &inventory_url,
        &credential_fingerprint(credential),
        Some(&body),
    )
    .unwrap();

    let cached = cached_provider_inventory_offline_at(
        path.clone(),
        declaration.id,
        inventory_url.clone(),
        credential,
    )
    .unwrap()
    .expect("matching cache should be available offline");
    let mut catalog = ModelCatalog::builtin().unwrap();
    register_test_openrouter_endpoint(&mut catalog);
    register_openrouter_models_from_response(&mut catalog, declaration, &cached).unwrap();

    let model = catalog
        .resolve(&ModelId("openrouter/cache-test/model".into()))
        .unwrap();
    assert_eq!(model.spec.api_name, "cache-test/model");
    assert_eq!(model.spec.limits.context_window, 64_000);
    assert_eq!(model.spec.limits.max_output_tokens, 8_000);
    assert!(cached_provider_inventory_offline_at(
        path,
        declaration.id,
        inventory_url,
        "a-different-key",
    )
    .unwrap()
    .is_none());
}
