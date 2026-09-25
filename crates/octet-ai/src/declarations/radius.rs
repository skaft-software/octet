//! Radius gateway declaration and dynamic catalog discovery.
//!
//! Upstream Pi declares the `radius` provider with the `pi-messages` wire API:
//! requests are `POST <base>/messages`, and the model catalog is discovered from
//! `<gateway>/v1/config` (an authenticated JSON document listing a `baseUrl` and
//! the gateway's models). This module owns the bounded, offline shape of that
//! document plus the URL normalization Pi applies, so a host discovery pass has
//! one validated value to consume.
//!
//! This is description and parsing only. It performs **no** network, credential,
//! filesystem or environment access, and it never invents a model that the
//! gateway did not declare. Model entries that do not match the declared shape
//! are counted in [`RadiusGatewayConfig::ignored_models`] instead of becoming a
//! silent, partially-populated model.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::DeclarationError;

/// Pi's default Radius gateway origin.
pub const DEFAULT_RADIUS_GATEWAY: &str = "https://radius.pi.dev";

/// Maximum accepted `/v1/config` document size.
pub const MAX_RADIUS_CONFIG_BYTES: usize = 256 * 1024;

/// Maximum number of models one gateway document may carry.
pub const MAX_RADIUS_GATEWAY_MODELS: usize = 512;

/// Maximum accepted identifier/name length for a discovered model.
pub const MAX_RADIUS_IDENTIFIER_BYTES: usize = 256;

fn invalid(message: &str) -> DeclarationError {
    DeclarationError::Invalid(message.to_owned())
}

fn bounded_identifier(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_RADIUS_IDENTIFIER_BYTES
        && !value.chars().any(char::is_control)
}

/// Per-million-token prices declared by the gateway for one discovered model.
///
/// The values mirror Pi's `ModelCost` shape exactly (`input`, `output`,
/// `cacheRead`, `cacheWrite`). They are non-negative, finite numbers only; a
/// malformed price makes the whole model entry invalid rather than being
/// rounded to a fabricated rate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusModelCost {
    /// Input-token rate per million tokens.
    #[serde(alias = "input")]
    pub input: f64,
    /// Output-token rate per million tokens.
    #[serde(alias = "output")]
    pub output: f64,
    /// Cache-read rate per million tokens.
    #[serde(alias = "cache_read")]
    pub cache_read: f64,
    /// Cache-write rate per million tokens.
    #[serde(alias = "cache_write")]
    pub cache_write: f64,
}

impl RadiusModelCost {
    fn is_valid(&self) -> bool {
        [self.input, self.output, self.cache_read, self.cache_write]
            .into_iter()
            .all(|rate| rate.is_finite() && rate >= 0.0)
    }
}

/// One model declared by a Radius gateway.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusGatewayModel {
    /// Gateway model identifier, used as the wire `model` value.
    #[serde(alias = "id")]
    pub id: String,
    /// Human-readable display name.
    #[serde(alias = "name")]
    pub name: String,
    /// Whether the model exposes a reasoning/thinking control.
    #[serde(alias = "reasoning")]
    pub reasoning: bool,
    /// Provider-native effort level mapping (`thinkingLevelMap` in Pi).
    #[serde(default, alias = "thinking_level_map")]
    pub thinking_level_map: BTreeMap<String, Option<String>>,
    /// Accepted input modalities; only `text` and `image` are defined.
    #[serde(alias = "input")]
    pub input: Vec<String>,
    /// Per-million-token prices.
    #[serde(alias = "cost")]
    pub cost: RadiusModelCost,
    /// Context window in tokens.
    #[serde(alias = "context_window")]
    pub context_window: u64,
    /// Maximum output tokens.
    #[serde(alias = "max_tokens")]
    pub max_tokens: u64,
}

impl RadiusGatewayModel {
    fn validate(&self) -> Result<(), DeclarationError> {
        if !bounded_identifier(&self.id) {
            return Err(invalid("invalid Radius gateway model identifier"));
        }
        if !bounded_identifier(&self.name) {
            return Err(invalid("invalid Radius gateway model name"));
        }
        if self.context_window == 0 || self.max_tokens == 0 || self.max_tokens > self.context_window
        {
            return Err(invalid("invalid Radius gateway model limits"));
        }
        if !self.cost.is_valid() {
            return Err(invalid("invalid Radius gateway model cost"));
        }
        if self
            .input
            .iter()
            .any(|modality| !matches!(modality.as_str(), "text" | "image"))
        {
            return Err(invalid("invalid Radius gateway input modality"));
        }
        if self.thinking_level_map.iter().any(|(level, mapped)| {
            !bounded_identifier(level)
                || mapped
                    .as_ref()
                    .is_some_and(|value| value.len() > MAX_RADIUS_IDENTIFIER_BYTES)
        }) {
            return Err(invalid("invalid Radius gateway thinking level map"));
        }
        Ok(())
    }
}

/// A gateway-discovered catalog document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusGatewayConfig {
    /// Base URL every discovered model uses for `POST <base>/messages`.
    #[serde(alias = "base_url")]
    pub base_url: String,
    /// Models the gateway declared and that passed this module's validation.
    #[serde(alias = "models")]
    pub models: Vec<RadiusGatewayModel>,
    /// Number of declared model entries skipped because they did not match the
    /// declared shape. A non-zero value means the gateway offered something this
    /// client cannot represent; it is never silently zero.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ignored_models: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Normalize a gateway value the way Pi does: prefix `https://` when no scheme
/// is present and strip every trailing slash.
pub fn normalize_radius_gateway_url(value: &str) -> String {
    let trimmed = value.trim();
    let with_scheme = if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    with_scheme.trim_end_matches('/').to_owned()
}

fn gateway_is_acceptable(url: &url::Url) -> bool {
    // The pi-messages codec sends a bearer credential to this origin. Accept
    // HTTPS everywhere, or literal-loopback HTTP for local fixtures/desktops.
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => url.host_str().is_some_and(|host| host == "localhost"),
    };
    url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && ((url.scheme() == "https") || (url.scheme() == "http" && loopback))
}

/// Resolve the `<gateway>/v1/config` discovery URL.
///
/// The path is absolute, exactly like Pi's `new URL("/v1/config", gateway)`, so
/// a configured gateway path prefix does not change the discovery endpoint.
pub fn radius_config_url(gateway: &str) -> Result<url::Url, DeclarationError> {
    let normalized = normalize_radius_gateway_url(gateway);
    let mut url =
        url::Url::parse(&normalized).map_err(|_| invalid("invalid Radius gateway URL"))?;
    if !gateway_is_acceptable(&url) {
        return Err(invalid(
            "Radius gateway requires HTTPS (or literal-loopback HTTP) without userinfo, query or fragment",
        ));
    }
    url.set_path("/v1/config");
    Ok(url)
}

/// Parse and validate a `/v1/config` document.
///
/// The envelope is strict (unknown fields fail closed). Model entries that do
/// not match the declared shape are dropped and counted; the envelope itself is
/// never partially accepted.
pub fn parse_radius_gateway_config(bytes: &[u8]) -> Result<RadiusGatewayConfig, DeclarationError> {
    if bytes.len() > MAX_RADIUS_CONFIG_BYTES {
        return Err(invalid("Radius gateway config exceeds its byte limit"));
    }
    let mut config: RadiusGatewayConfig =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid Radius gateway config"))?;
    config.base_url = normalize_radius_gateway_url(&config.base_url);
    let base = url::Url::parse(&config.base_url)
        .map_err(|_| invalid("invalid Radius gateway config base URL"))?;
    if !gateway_is_acceptable(&base) {
        return Err(invalid(
            "Radius gateway config base URL requires HTTPS (or literal-loopback HTTP) without userinfo, query or fragment",
        ));
    }
    if config.models.len() > MAX_RADIUS_GATEWAY_MODELS {
        return Err(invalid("Radius gateway config declares too many models"));
    }
    let mut models = Vec::with_capacity(config.models.len());
    let mut ignored = 0usize;
    for model in config.models {
        if model.validate().is_ok() {
            models.push(model);
        } else {
            ignored += 1;
        }
    }
    Ok(RadiusGatewayConfig {
        base_url: config.base_url,
        models,
        ignored_models: ignored,
    })
}

/// Whether this gateway document must be refreshed rather than reused.
///
/// `checked_at_ms` is the host's own recorded refresh time; a document without
/// models is always considered absent so a failed discovery never pins an empty
/// catalog.
pub fn radius_config_is_stale(
    config: &RadiusGatewayConfig,
    checked_at_ms: Option<u64>,
    now_ms: u64,
    max_age_ms: u64,
) -> bool {
    if config.models.is_empty() {
        return true;
    }
    match checked_at_ms {
        None => true,
        Some(checked) => now_ms.saturating_sub(checked) >= max_age_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": "Radius Auto", "reasoning": true,
            "thinkingLevelMap": {"off": null, "high": "high"},
            "input": ["text", "image"],
            "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.1, "cacheWrite": 0.2},
            "contextWindow": 128000, "maxTokens": 16384
        })
    }

    #[test]
    fn gateway_normalization_and_discovery_url_are_absolute_and_credential_free() {
        assert_eq!(
            normalize_radius_gateway_url("radius.pi.dev/"),
            "https://radius.pi.dev"
        );
        assert_eq!(
            normalize_radius_gateway_url("http://127.0.0.1:8080///"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            radius_config_url("radius.pi.dev").unwrap().as_str(),
            "https://radius.pi.dev/v1/config"
        );
        // An absolute path replaces any configured prefix, exactly like Pi.
        assert_eq!(
            radius_config_url("https://radius.example/gateway/")
                .unwrap()
                .as_str(),
            "https://radius.example/v1/config"
        );
        for rejected in [
            "https://user:secret@radius.example",
            "https://radius.example/?token=secret",
            "https://radius.example/#fragment",
            "http://radius.example",
        ] {
            assert!(radius_config_url(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn config_document_round_trips_and_counts_unrepresentable_models() {
        let body = serde_json::json!({
            "baseUrl": "https://radius.example",
            "models": [
                model("auto"),
                {"id": "broken", "name": "Broken", "reasoning": false, "input": ["video"],
                 "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
                 "contextWindow": 100, "maxTokens": 200}
            ]
        });
        let config = parse_radius_gateway_config(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(config.models.len(), 1);
        assert_eq!(config.models[0].id, "auto");
        assert_eq!(config.ignored_models, 1);
        assert_eq!(config.models[0].thinking_level_map["off"], None);
        assert_eq!(
            config.models[0].thinking_level_map["high"].as_deref(),
            Some("high")
        );
        assert!(serde_json::to_string(&config)
            .unwrap()
            .contains("ignoredModels"));
    }

    #[test]
    fn malformed_or_oversized_documents_fail_closed() {
        assert!(parse_radius_gateway_config(b"not json").is_err());
        assert!(parse_radius_gateway_config(br#"{"baseUrl":"https://x.example"}"#).is_err());
        assert!(parse_radius_gateway_config(
            br#"{"baseUrl":"https://user:secret@x.example","models":[]}"#
        )
        .is_err());
        assert!(
            parse_radius_gateway_config(br#"{"baseUrl":"http://x.example","models":[]}"#).is_err()
        );
        let oversized = vec![b' '; MAX_RADIUS_CONFIG_BYTES + 1];
        assert!(parse_radius_gateway_config(&oversized).is_err());
    }

    #[test]
    fn staleness_never_pins_an_empty_catalog() {
        let empty = RadiusGatewayConfig {
            base_url: "https://radius.example".into(),
            models: vec![],
            ignored_models: 0,
        };
        assert!(radius_config_is_stale(&empty, Some(9_999), 10_000, 60_000));
        let body =
            serde_json::json!({"baseUrl": "https://radius.example", "models": [model("auto")]});
        let config = parse_radius_gateway_config(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(!radius_config_is_stale(
            &config,
            Some(10_000),
            10_000,
            60_000
        ));
        assert!(radius_config_is_stale(&config, Some(0), 60_000, 60_000));
    }
}
