#![allow(missing_docs)]

use octet_sdk::provider::{
    builtin_provider_definitions, ProviderAccess, ProviderCatalogKind,
};

#[test]
fn google_and_vertex_declarations_keep_authentication_boundaries() {
    let definitions = builtin_provider_definitions();
    let google = definitions
        .iter()
        .find(|definition| definition.id() == "google")
        .expect("native Gemini declaration");
    assert!(matches!(
        google.authentication(),
        ProviderAccess::Environment { variables }
            if variables.iter().any(|variable| variable == "GEMINI_API_KEY")
    ));

    let vertex = definitions
        .iter()
        .find(|definition| definition.id() == "google-vertex")
        .expect("Vertex declaration");
    assert!(matches!(
        vertex.authentication(),
        ProviderAccess::ApplicationDefaultCredentials
    ));
    assert_eq!(vertex.catalog(), ProviderCatalogKind::Static);
    assert_eq!(vertex.routes().len(), 1);

    // The setup-facing definition cannot carry a URL, token, private key, or
    // credential-store handle. Those values stay in the private ADC resolver.
    let rendered = format!("{vertex:?}");
    for forbidden in [
        "oauth2.googleapis.com",
        "access_token",
        "private_key",
        "fixture-client",
        "fixture-refresh",
    ] {
        assert!(!rendered.contains(forbidden), "definition leaked {forbidden}");
    }
}
