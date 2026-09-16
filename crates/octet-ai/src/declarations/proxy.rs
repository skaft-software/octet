//! Environment-derived HTTP proxy resolution for request targets.
//!
//! This reproduces upstream `pi`'s `resolveHttpProxyUrlForTarget`
//! (`packages/ai/src/utils/node-http-proxy.ts`) as pure logic so it can be
//! tested without a network or a live client. A host supplies the already-read
//! `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` values (case-insensitive,
//! lower- or upper-case) and applies the returned proxy URL to its transport.
//!
//! `NO_PROXY` semantics match upstream exactly: a comma/space separated list of
//! entries, optional `:port`, exact hosts, and `.domain` / `*.domain` prefixes
//! that exclude **both** the root domain and its subdomains. A bare `*`
//! disables proxying entirely. Only `http`/`https` proxies are accepted; SOCKS
//! and PAC URLs fail closed.
//!
//! [`crate::AiClient::try_new`] snapshots these variables and uses the resolver
//! for both pre-dispatch validation and its no-redirect HTTP transport.

use std::collections::BTreeMap;

/// Failure returned when a configured proxy cannot be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProxyError {
    /// The proxy value was present but not a parsable URL.
    #[error("invalid proxy URL: {0}")]
    InvalidProxyUrl(String),
    /// The proxy URL used a protocol other than `http` or `https`.
    #[error("unsupported proxy protocol {0}; SOCKS and PAC proxy URLs are not supported")]
    UnsupportedProxyProtocol(String),
}

fn default_proxy_port(protocol: &str) -> u16 {
    match protocol {
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        "ftp" => 21,
        "gopher" => 70,
        _ => 0,
    }
}

fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

fn parse_no_proxy_entry(entry: &str) -> Option<(String, u16)> {
    let trimmed = entry.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some(closing) = rest.find(']') {
            let host = rest[..closing].to_owned();
            let after = &rest[closing + 1..];
            if let Some(port) = after.strip_prefix(':') {
                let port = port.parse::<u16>().unwrap_or(0);
                return Some((host, port));
            }
            return Some((host, 0));
        }
    }
    // An unbracketed value with more than one colon is an IPv6 literal.
    if trimmed.matches(':').count() > 1 {
        return Some((trimmed, 0));
    }
    if let Some((host, port)) = trimmed.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            return Some((host.to_owned(), port));
        }
    }
    Some((trimmed, 0))
}

fn no_proxy_excludes(hostname: &str, port: u16, no_proxy: &str) -> bool {
    let no_proxy = no_proxy.to_ascii_lowercase();
    if no_proxy.is_empty() {
        return false;
    }
    if no_proxy == "*" {
        return true;
    }
    let target = strip_brackets(hostname).to_ascii_lowercase();
    no_proxy
        .split(|c: char| c == ',' || c.is_whitespace())
        .any(|entry| {
            let Some((mut domain, entry_port)) = parse_no_proxy_entry(entry) else {
                return false;
            };
            if entry_port != 0 && entry_port != port {
                return false;
            }
            domain = strip_brackets(&domain).to_owned();
            if let Some(stripped) = domain.strip_prefix("*.") {
                domain = stripped.to_owned();
            } else if let Some(stripped) = domain
                .strip_prefix('.')
                .or_else(|| domain.strip_prefix('*'))
            {
                domain = stripped.to_owned();
            }
            if domain.is_empty() {
                return false;
            }
            target == domain || target.ends_with(&format!(".{domain}"))
        })
}

/// Resolve the proxy URL to use for `target`, or `None` for a direct connection.
///
/// `scheme_proxy` is the value of the target scheme's proxy variable (for
/// example `HTTPS_PROXY` for an `https` target) and `all_proxy` is the
/// `ALL_PROXY` fallback. `no_proxy` is the raw `NO_PROXY` value. A proxy value
/// without a scheme is prefixed with the target scheme, mirroring upstream.
pub fn resolve_http_proxy(
    target: &str,
    scheme_proxy: Option<&str>,
    all_proxy: Option<&str>,
    no_proxy: Option<&str>,
) -> Result<Option<String>, ProxyError> {
    let parsed = match url::Url::parse(target) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(None),
    };
    if parsed.host_str().is_none() {
        return Ok(None);
    }
    let protocol = parsed.scheme();
    let hostname = strip_brackets(parsed.host_str().unwrap_or_default());
    let port = parsed
        .port()
        .unwrap_or_else(|| default_proxy_port(protocol));
    if let Some(no_proxy) = no_proxy {
        if no_proxy_excludes(hostname, port, no_proxy) {
            return Ok(None);
        }
    }

    let proxy = scheme_proxy
        .filter(|value| !value.is_empty())
        .or_else(|| all_proxy.filter(|value| !value.is_empty()));
    let Some(proxy) = proxy else {
        return Ok(None);
    };
    let proxy = if proxy.contains("://") {
        proxy.to_owned()
    } else {
        format!("{protocol}://{proxy}")
    };
    let parsed_proxy =
        url::Url::parse(&proxy).map_err(|_| ProxyError::InvalidProxyUrl(proxy.clone()))?;
    match parsed_proxy.scheme() {
        "http" | "https" => Ok(Some(proxy)),
        other => Err(ProxyError::UnsupportedProxyProtocol(other.to_owned())),
    }
}

/// Look up a proxy value from a provider-scoped environment map.
///
/// The lower-case name wins over the upper-case name, matching upstream
/// `getProxyEnv`; this lets a host resolve the four transport variables without
/// re-implementing case handling.
pub fn proxy_env_value<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    env.get(&name.to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            env.get(&name.to_ascii_uppercase())
                .filter(|value| !value.is_empty())
        })
        .map(String::as_str)
}

/// Immutable environment shared by HTTP dispatch preflight and reqwest's proxy
/// callback. The callback cannot surface errors; every operation therefore
/// validates this exact snapshot before credential resolution or dispatch.
#[derive(Clone)]
pub(crate) struct ProxyEnvironment(BTreeMap<String, String>);

impl ProxyEnvironment {
    pub(crate) const NAMES: [&'static str; 8] = [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
        "no_proxy",
        "NO_PROXY",
    ];

    pub(crate) fn new(mut env: BTreeMap<String, String>) -> Self {
        env.retain(|key, _| Self::NAMES.contains(&key.as_str()));
        Self(env)
    }

    pub(crate) fn overlay(&self, values: &BTreeMap<String, String>) -> Self {
        let mut env = self.0.clone();
        for name in Self::NAMES {
            if let Some(value) = values.get(name) { env.insert(name.into(), value.clone()); }
        }
        Self(env)
    }

    pub(crate) fn resolve(&self, target: &url::Url) -> Result<Option<url::Url>, crate::AiError> {
        let invalid = || {
            crate::ConfigError::Parse(
            "invalid or unsupported HTTP proxy configuration (only HTTP/HTTPS proxies are supported)".to_owned(),
        )
        };
        if self
            .0
            .values()
            .any(|value| value.len() > crate::auth::MAX_ENV_VALUE_BYTES)
        {
            return Err(invalid().into());
        }
        let selected = resolve_http_proxy(
            target.as_str(),
            proxy_env_value(&self.0, &format!("{}_proxy", target.scheme())),
            proxy_env_value(&self.0, "all_proxy"),
            proxy_env_value(&self.0, "no_proxy"),
        )
        .map_err(|_| invalid())?;
        selected
            .map(|proxy| url::Url::parse(&proxy).map_err(|_| invalid().into()))
            .transpose()
    }

    pub(crate) fn configure(
        self: std::sync::Arc<Self>,
        builder: reqwest::ClientBuilder,
    ) -> reqwest::ClientBuilder {
        // Disable reqwest's independent environment/OS resolver; otherwise a
        // NO_PROXY exclusion could fall through to a second proxy policy.
        builder
            .no_proxy()
            .proxy(reqwest::Proxy::custom(move |target| {
                self.resolve(target).ok().flatten()
            }))
    }
}

#[cfg(test)]
mod tests {
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
}
