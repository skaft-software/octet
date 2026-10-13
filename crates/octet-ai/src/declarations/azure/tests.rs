//! Unit tests for `crate::declarations::azure`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `azure.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::declarations::azure`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn azure_base_normalization_is_host_specific_and_rejects_query_credentials() {
    for suffix in [
        "openai.azure.com",
        "cognitiveservices.azure.com",
        "ai.azure.com",
    ] {
        for path in ["", "/", "/openai/", "/openai/v1/responses"] {
            let url = parse_base(&format!("https://resource.{suffix}{path}")).unwrap();
            assert_eq!(url.path(), "/openai/v1/");
        }
    }
    let custom = parse_base("https://proxy.example/custom%20path?api-version=v1").unwrap();
    assert_eq!(custom.path(), "/custom%20path/");
    assert_eq!(custom.query(), Some("api-version=v1"));
    for url in [
        "https://user:secret@resource.openai.azure.com/",
        "https://resource.openai.azure.com/?secret=value",
        "https://resource.openai.azure.com/#secret",
    ] {
        assert!(parse_base(url).is_err());
    }
}

#[test]
fn explicit_resource_version_and_deployment_mapping_are_resolved_without_network() {
    let mut model = crate::ModelCatalog::builtin()
        .unwrap()
        .resolve(&crate::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).protocol = Protocol::OpenAiResponses;
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile = ResponsesRuntimeProfile::Azure;
    let api_name = model.spec.api_name.clone();
    let mut env: BTreeMap<String, String> = [
        "AZURE_OPENAI_BASE_URL",
        "AZURE_OPENAI_RESOURCE_NAME",
        "AZURE_OPENAI_API_VERSION",
        "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
    ]
    .into_iter()
    .map(|name| (name.into(), String::new()))
    .collect();
    let options = AzureRequestOptions {
        resource_name: Some("selected-resource".into()),
        ..Default::default()
    };
    apply(&mut model, Some(&options), &env).unwrap();
    assert_eq!(
        model.endpoint.base_url.as_str(),
        "https://selected-resource.openai.azure.com/openai/v1/?api-version=v1"
    );
    assert_eq!(model.spec.api_name, api_name);
    env.insert(
        "AZURE_OPENAI_API_VERSION".into(),
        "2025-04-01-preview".into(),
    );
    env.insert(
        "AZURE_OPENAI_DEPLOYMENT_NAME_MAP".into(),
        format!("{api_name}=chosen"),
    );
    apply(&mut model, None, &env).unwrap();
    assert_eq!(model.spec.api_name, "chosen");
    assert_eq!(
        model.endpoint.base_url.query(),
        Some("api-version=2025-04-01-preview")
    );
}
