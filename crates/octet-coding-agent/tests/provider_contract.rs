#![allow(missing_docs)]

use octet_sdk::provider::{
    builtin_provider_definitions, PricingProfile, ProviderAccess, ProviderCatalogKind,
};

#[test]
fn generated_builtin_definitions_are_credential_free() {
    let definitions = builtin_provider_definitions();
    // Additive presets: Baseten, three Qwen token plans, Meta, Z.ai Coding CN,
    // and the four subscription logins.
    assert_eq!(definitions.len(), 41);
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
fn every_subscription_login_is_a_public_credential_free_definition() {
    let definitions = builtin_provider_definitions();
    // Each of these is reachable only through `--login <selector>`, so the
    // selector is the public contract and must match the documented one.
    for (id, login, pricing) in [
        ("xai-subscription", "grok", PricingProfile::Subscription),
        (
            "kimi-coding-subscription",
            "kimi",
            PricingProfile::Subscription,
        ),
        ("meta-subscription", "meta", PricingProfile::Subscription),
        // OpenRouter's login mints an ordinary metered API key, so its route
        // keeps the public catalog's per-model pricing rather than the
        // reviewed subscription allowlist.
        ("openrouter-oauth", "openrouter", PricingProfile::OpenRouter),
    ] {
        let definition = definitions
            .iter()
            .find(|definition| definition.id() == id)
            .unwrap_or_else(|| panic!("missing public definition for {id}"));
        assert!(
            matches!(
                definition.authentication(),
                ProviderAccess::Subscription { login: actual } if actual == login
            ),
            "{id} must be a subscription login named {login:?}"
        );
        assert_eq!(
            definition.catalog(),
            ProviderCatalogKind::Subscription,
            "{id} catalog kind drifted"
        );
        // A subscription route never inherits public API quotes: only the
        // provider's own reviewed pricing may establish a price.
        assert_eq!(
            definition.pricing(),
            pricing,
            "{id} pricing profile drifted"
        );
        let rendered = format!("{definition:?}");
        assert!(
            !rendered.contains("https://") && !rendered.contains("api.x.ai"),
            "{id} public projection leaked an endpoint: {rendered}"
        );
    }

    // The API-key provider for the same vendor stays an independent definition,
    // so signing in with a plan can never silently replace a paid API key.
    for (api_key_id, subscription_id) in [
        ("xai", "xai-subscription"),
        ("kimi-coding", "kimi-coding-subscription"),
        ("meta", "meta-subscription"),
        ("openrouter", "openrouter-oauth"),
    ] {
        let api_key = definitions
            .iter()
            .find(|definition| definition.id() == api_key_id)
            .unwrap_or_else(|| panic!("missing public definition for {api_key_id}"));
        assert!(
            matches!(api_key.authentication(), ProviderAccess::Environment { .. }),
            "{api_key_id} must stay an environment-authenticated provider"
        );
        assert_ne!(api_key.id(), subscription_id);
    }
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
