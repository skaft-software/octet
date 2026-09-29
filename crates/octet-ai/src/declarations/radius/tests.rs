//! Unit tests for `crate::declarations::radius`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `radius.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::declarations::radius`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

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
    assert!(parse_radius_gateway_config(br#"{"baseUrl":"http://x.example","models":[]}"#).is_err());
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
    let body = serde_json::json!({"baseUrl": "https://radius.example", "models": [model("auto")]});
    let config = parse_radius_gateway_config(&serde_json::to_vec(&body).unwrap()).unwrap();
    assert!(!radius_config_is_stale(
        &config,
        Some(10_000),
        10_000,
        60_000
    ));
    assert!(radius_config_is_stale(&config, Some(0), 60_000, 60_000));
}
