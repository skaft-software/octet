//! Unit tests for `crate::declarations::proxy`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `proxy.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::declarations::proxy`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

fn resolve(target: &str, scheme_proxy: Option<&str>, no_proxy: Option<&str>) -> Option<String> {
    resolve_http_proxy(target, scheme_proxy, None, no_proxy).expect("valid proxy resolution")
}

#[test]
fn no_proxy_root_and_subdomain_are_both_excluded() {
    for entry in ["example.com", ".example.com", "*.example.com"] {
        assert!(
            no_proxy_excludes("example.com", 443, entry),
            "{entry}: root must be excluded"
        );
        assert!(
            no_proxy_excludes("api.example.com", 443, entry),
            "{entry}: subdomain must be excluded"
        );
        assert!(
            !no_proxy_excludes("notexample.com", 443, entry),
            "{entry}: unrelated host must be proxied"
        );
    }
    assert_eq!(
        resolve(
            "https://example.com/v1/models",
            Some("http://proxy:8080"),
            Some("example.com")
        ),
        None
    );
    assert_eq!(
        resolve(
            "https://api.example.com/v1/models",
            Some("http://proxy:8080"),
            Some("*.example.com")
        ),
        None
    );
}

#[test]
fn no_proxy_port_and_star_entries() {
    assert!(no_proxy_excludes("example.com", 8080, "example.com:8080"));
    assert!(!no_proxy_excludes("example.com", 443, "example.com:8080"));
    assert!(no_proxy_excludes("anything.invalid", 443, "*"));
    // Only a lone `*` disables proxying; `*` inside a list is a no-op
    // (upstream `shouldProxyHostname` skips empty domains).
    assert!(!no_proxy_excludes("anything.invalid", 443, " , * ,other"));
    assert!(!no_proxy_excludes("example.com", 443, ""));
}

#[test]
fn proxy_scheme_defaults_and_all_proxy_fallback() {
    assert_eq!(
        resolve(
            "https://api.openai.com/v1/models",
            Some("proxy.local:3128"),
            None
        ),
        Some("https://proxy.local:3128".to_owned())
    );
    assert_eq!(
        resolve_http_proxy(
            "http://api.openai.com/v1/models",
            None,
            Some("http://all-proxy:8080"),
            None
        )
        .unwrap(),
        Some("http://all-proxy:8080".to_owned())
    );
    // Empty values are treated as unset.
    assert_eq!(
        resolve_http_proxy("https://x.invalid/", Some(""), Some(""), None).unwrap(),
        None
    );
}

#[test]
fn unsupported_and_malformed_proxies_fail_closed() {
    assert_eq!(
        resolve_http_proxy(
            "https://x.invalid/",
            Some("socks5://proxy:1080"),
            None,
            None
        ),
        Err(ProxyError::UnsupportedProxyProtocol("socks5".to_owned()))
    );
    assert!(matches!(
        resolve_http_proxy("https://x.invalid/", Some("http://["), None, None),
        Err(ProxyError::InvalidProxyUrl(_))
    ));
    // An unparsable target is a direct connection, never a proxy guess.
    assert_eq!(
        resolve_http_proxy("not a url", Some("http://proxy:8080"), None, None).unwrap(),
        None
    );
}

#[tokio::test]
async fn proxy_transport_excludes_root_and_subdomain_on_loopback() {
    use std::sync::Arc;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let direct = MockServer::start().await;
    let proxy = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&direct)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&proxy)
        .await;
    let address = *direct.address();
    for exclusion in [
        "example.invalid",
        ".example.invalid",
        "*.example.invalid",
        "other.invalid\texample.invalid\nmore.invalid",
    ] {
        let env = Arc::new(ProxyEnvironment::new(BTreeMap::from([
            ("HTTP_PROXY".to_owned(), proxy.uri()),
            ("NO_PROXY".to_owned(), exclusion.to_owned()),
        ])));
        let http = env
            .configure(
                reqwest::Client::builder()
                    .resolve("example.invalid", address)
                    .resolve("api.example.invalid", address),
            )
            .build()
            .unwrap();
        for host in [
            "example.invalid",
            "api.example.invalid",
            "notexample.invalid",
        ] {
            http.get(format!("http://{host}:{}/", address.port()))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
    }
    assert_eq!(direct.received_requests().await.unwrap().len(), 8);
    assert_eq!(proxy.received_requests().await.unwrap().len(), 4);
}

#[test]
fn env_lookup_prefers_lowercase() {
    let env = BTreeMap::from([
        ("https_proxy".to_owned(), "http://lower:1".to_owned()),
        ("HTTPS_PROXY".to_owned(), "http://upper:2".to_owned()),
    ]);
    assert_eq!(proxy_env_value(&env, "HTTPS_PROXY"), Some("http://lower:1"));
    assert_eq!(proxy_env_value(&env, "http_proxy"), None);
}

#[test]
fn an_overlay_replaces_both_cases_of_the_variables_it_sets() {
    // The ambient environment uses lower-case names, as many Linux machines do.
    let base = ProxyEnvironment::new(BTreeMap::from([
        ("no_proxy".to_owned(), "localhost".to_owned()),
        ("https_proxy".to_owned(), "http://machine:1".to_owned()),
        ("http_proxy".to_owned(), "http://machine:2".to_owned()),
    ]));
    let overlaid = base.overlay(&BTreeMap::from([
        ("NO_PROXY".to_owned(), ".internal.example".to_owned()),
        ("HTTPS_PROXY".to_owned(), "http://provider:3".to_owned()),
    ]));
    let target = |url: &str| url::Url::parse(url).unwrap();
    assert_eq!(
        overlaid
            .resolve(&target("http://api.internal.example/"))
            .unwrap(),
        None,
        "the request's NO_PROXY must win over the ambient no_proxy"
    );
    assert_eq!(
        overlaid
            .resolve(&target("https://api.example.com/"))
            .unwrap()
            .map(|proxy| proxy.to_string()),
        Some("http://provider:3/".to_owned()),
        "the request's HTTPS_PROXY must win over the ambient https_proxy"
    );
    assert_eq!(
        overlaid
            .resolve(&target("http://api.example.com/"))
            .unwrap()
            .map(|proxy| proxy.to_string()),
        Some("http://machine:2/".to_owned()),
        "variables the overlay does not set keep their ambient value"
    );
}
