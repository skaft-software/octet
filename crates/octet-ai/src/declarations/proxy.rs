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
            if let Some(value) = values.get(name) {
                env.insert(name.into(), value.clone());
            }
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
mod tests;
