use super::*;

fn setup_model(
    context_window: Option<u64>,
    configured_context_window: Option<u64>,
    context_window_asserted: bool,
    requires_context_configuration: bool,
    reasoning_supported: Option<bool>,
) -> SetupModel {
    SetupModel {
        api_name: "model-id".to_owned(),
        display_name: "Model".to_owned(),
        context_window,
        configured_context_window,
        context_window_asserted,
        requires_context_configuration,
        max_output_tokens: None,
        stored_max_output_tokens: 0,
        max_output_tokens_asserted: false,
        reasoning_supported,
        reasoning_source: None,
        reasoning_values: Vec::new(),
        reasoning_default: None,
        reasoning_profile: None,
        native_metadata: None,
    }
}

#[test]
fn provider_setup_model_description_keeps_unknown_facts_unknown() {
    let description = setup_model_metadata_description(&setup_model(None, None, false, true, None));

    assert!(description.contains("effective context unknown"));
    assert!(description.contains("positive explicit limit is required"));
    assert!(description.contains("Chat reasoning support unknown"));
    assert!(!description.contains("endpoint-asserted"));
    assert!(!description.contains("explicitly unsupported"));
}

#[test]
fn provider_setup_model_description_distinguishes_asserted_and_configured_context() {
    let asserted =
        setup_model_metadata_description(&setup_model(Some(32_000), None, true, false, Some(true)));
    assert!(asserted.contains("endpoint-asserted effective context: 32000"));
    assert!(asserted.contains("Chat reasoning explicitly supported"));

    let configured = setup_model_metadata_description(&setup_model(
        Some(16_000),
        Some(8_192),
        false,
        false,
        Some(false),
    ));
    assert!(configured.contains("explicitly configured context: 8192"));
    assert!(configured.contains("Chat reasoning explicitly unsupported"));
    assert!(!configured.contains("endpoint-asserted effective context"));
}

#[test]
fn provider_setup_authentication_summary_never_displays_credential_values() {
    assert_eq!(
        setup_authentication_summary(&SetupCredentialSummary::StoredCredentialConfigured),
        "stored credential configured (value hidden)"
    );
    assert_eq!(
        setup_authentication_summary(&SetupCredentialSummary::Environment {
            variable: "CUSTOM_API_TOKEN".to_owned(),
        }),
        "environment variable CUSTOM_API_TOKEN"
    );
}
