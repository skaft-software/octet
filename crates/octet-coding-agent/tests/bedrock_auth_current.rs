#![allow(missing_docs)]

use octet_ai::{AwsCredentials, AwsSigV4Signer, Protocol, Secret};
use octet_sdk::provider::{builtin_provider_definitions, ProviderAccess, ProviderCatalogKind};

#[test]
fn bedrock_definition_exposes_only_credential_free_chain_setup() {
    let definition = builtin_provider_definitions()
        .into_iter()
        .find(|definition| definition.id() == "bedrock")
        .expect("generated Bedrock provider definition");

    assert_eq!(definition.label(), "Amazon Bedrock");
    assert_eq!(definition.catalog(), ProviderCatalogKind::Static);
    match definition.authentication() {
        ProviderAccess::Environment { variables } => assert_eq!(
            variables,
            &vec![
                "AWS_ACCESS_KEY_ID".to_owned(),
                "AWS_SECRET_ACCESS_KEY".to_owned(),
                "AWS_PROFILE".to_owned(),
                "AWS_REGION".to_owned(),
            ]
        ),
        other => panic!("Bedrock must retain its AWS chain setup classification: {other:?}"),
    }
    assert_eq!(definition.routes().len(), 1);
    assert_eq!(definition.routes()[0].protocol(), Protocol::BedrockConverse);
    assert_eq!(definition.routes()[0].endpoint_id(), "bedrock");

    let rendered = format!("{definition:?}");
    assert!(!rendered.contains("https://"));
    assert!(!rendered.contains("secret"));
    assert!(!rendered.contains("credential_process"));
}

#[test]
fn aws_credential_debug_output_is_redacted_and_signing_components_are_bounded() {
    let credentials = AwsCredentials::new(
        "fixture-access-key",
        "fixture-secret-key",
        Some(Secret::from("fixture-session-token")),
    )
    .unwrap();
    let rendered = format!("{credentials:?}");
    assert!(!rendered.contains("fixture-access-key"));
    assert!(!rendered.contains("fixture-secret-key"));
    assert!(!rendered.contains("fixture-session-token"));

    assert!(AwsSigV4Signer::new(credentials.clone(), "us-west-2", "bedrock").is_ok());
    assert!(AwsSigV4Signer::new(credentials.clone(), "us/west-2", "bedrock").is_err());
    assert!(AwsSigV4Signer::new(credentials, "us-west-2", "bedrock/runtime").is_err());
}
