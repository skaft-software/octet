#![allow(missing_docs)]

use octet_sdk::provider::{
    builtin_provider_definitions, PricingProfile, ProviderAccess, ProviderCatalogKind,
};

#[test]
fn generated_builtin_definitions_are_credential_free() {
    let definitions = builtin_provider_definitions();
    // Additive presets: Baseten, three Qwen token plans, Meta, and Z.ai Coding CN.
    assert_eq!(definitions.len(), 37);
    // Host-owned Copilot remains deliberately absent until an embedding host
    // completes discovery; it is not a generated CLI/configuration preset.
    assert!(!definitions
        .iter()
        .any(|definition| definition.id() == "github-copilot"));

    let rendered = format!("{definitions:?}");
    assert!(!rendered.contains("https://"));
    assert!(!rendered.contains("authorization"));
    assert!(!rendered.contains("x-api-key"));
    assert!(!rendered.contains("openai-beta"));
    assert!(!rendered.contains("originator"));
    assert!(!rendered.contains("CredentialStore"));

    let codex = definitions
        .iter()
        .find(|definition| definition.id() == "codex")
        .expect("generated Codex definition");
    assert!(matches!(
        codex.authentication(),
        ProviderAccess::Subscription { login } if login == "codex"
    ));
    assert_eq!(codex.catalog(), ProviderCatalogKind::Subscription);
    assert_eq!(codex.pricing(), PricingProfile::Subscription);
}

#[test]
fn token_plan_and_coding_presets_are_public_credential_free_definitions() {
    let definitions = builtin_provider_definitions();
    // Additive presets for row 1a.1: the five declarations must be reachable
    // through the public SDK boundary with their documented environment
    // variables, and their public projection must stay credential- and
    // endpoint-free.
    for (id, label, variables) in [
        ("baseten", "Baseten", &["BASETEN_API_KEY"][..]),
        (
            "qwen-token-plan",
            "Qwen Token Plan",
            &["QWEN_TOKEN_PLAN_API_KEY"][..],
        ),
        (
            "qwen-token-plan-cn",
            "Qwen Token Plan CN",
            &["QWEN_TOKEN_PLAN_CN_API_KEY"][..],
        ),
        (
            "qwen-token-plan-individual",
            "Qwen Token Plan Individual",
            &["QWEN_TOKEN_PLAN_API_KEY"][..],
        ),
        (
            "zai-coding-cn",
            "Z.AI Coding CN",
            &["ZAI_CODING_CN_API_KEY"][..],
        ),
    ] {
        let definition = definitions
            .iter()
            .find(|definition| definition.id() == id)
            .unwrap_or_else(|| panic!("missing public definition for {id}"));
        assert_eq!(definition.label(), label, "{id} label drifted");
        assert_eq!(
            definition.pricing(),
            PricingProfile::Reference,
            "{id} pricing profile drifted"
        );
        match definition.authentication() {
            ProviderAccess::Environment { variables: actual } => assert_eq!(
                actual.as_slice(),
                variables,
                "{id} credential environment drifted"
            ),
            other => panic!("{id}: unexpected public access {other:?}"),
        }
        let rendered = format!("{definition:?}");
        assert!(
            !rendered.contains("https://") && !rendered.contains("baseten.co"),
            "{id} public projection leaked an endpoint: {rendered}"
        );
    }

    // The Vertex definition stays ADC-only in the public contract; the
    // GOOGLE_CLOUD_API_KEY selection is a private runtime resolution.
    let vertex = definitions
        .iter()
        .find(|definition| definition.id() == "vertex")
        .expect("generated Vertex definition");
    assert!(matches!(
        vertex.authentication(),
        ProviderAccess::ApplicationDefaultCredentials
    ));
    assert_eq!(vertex.catalog(), ProviderCatalogKind::Static);
}
