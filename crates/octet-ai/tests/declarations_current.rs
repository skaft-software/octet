#![allow(missing_docs)]

//! Public declaration-surface regressions for the radius/pi-messages gateway,
//! Bedrock profile-ARN regions, and the per-request Codex transport.
//!
//! Every case is offline and loopback-free: the modules under test are pure
//! parsing/policy, so these tests also prove the host-facing API is reachable
//! without a network or a credential.

use std::collections::BTreeMap;

use octet_ai::declarations::bedrock::{
    bedrock_endpoint_region, bedrock_model_region, bedrock_runtime_endpoint, bedrock_runtime_host,
    parse_bedrock_arn, resolve_bedrock_region, DEFAULT_BEDROCK_REGION,
};
use octet_ai::declarations::codex::{
    effective_codex_connect_timeout_ms, normalize_codex_timeout_ms, resolve_codex_transport,
    CodexTransport, CodexTransportReason, CodexWebSocketDebugStats, MAX_CODEX_WEBSOCKET_CONNECT_TIMEOUT_MS,
};
use octet_ai::declarations::radius::{
    normalize_radius_gateway_url, parse_radius_gateway_config, radius_config_url,
    radius_config_is_stale, DEFAULT_RADIUS_GATEWAY, MAX_RADIUS_CONFIG_BYTES,
};
use octet_ai::declarations::{
    AnthropicCompatPreset, AnthropicFallbackCost, AnthropicFallbackModel, DeclarationError,
    ModelPreset, RequestOverrides,
};
use octet_ai::EndpointTransport;
use serde_json::json;

#[test]
fn radius_gateway_discovery_is_bounded_credential_free_and_normalized() {
    assert_eq!(normalize_radius_gateway_url(" radius.pi.dev/ "), DEFAULT_RADIUS_GATEWAY);
    assert_eq!(
        radius_config_url("radius.pi.dev").unwrap().as_str(),
        "https://radius.pi.dev/v1/config"
    );
    for rejected in [
        "https://user:secret@radius.example",
        "https://radius.example/?token=secret",
        "http://radius.example",
    ] {
        assert!(declaration_error(radius_config_url(rejected)), "{rejected}");
    }

    let body = serde_json::to_vec(&json!({
        "baseUrl": "https://radius.example/",
        "models": [{
            "id": "auto", "name": "Radius Auto", "reasoning": true,
            "thinkingLevelMap": {"off": null, "high": "high"},
            "input": ["text", "image"],
            "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.1, "cacheWrite": 0.2},
            "contextWindow": 128000, "maxTokens": 16384
        }]
    }))
    .unwrap();
    let config = parse_radius_gateway_config(&body).unwrap();
    assert_eq!(config.base_url, "https://radius.example");
    assert_eq!(config.models.len(), 1);
    assert_eq!(config.ignored_models, 0);
    assert!(radius_config_is_stale(&config, None, 1_000, 60_000));
    assert!(!radius_config_is_stale(&config, Some(999_000), 1_000_000, 60_000));
    let oversized = vec![b' '; MAX_RADIUS_CONFIG_BYTES + 1];
    assert!(declaration_error(parse_radius_gateway_config(&oversized)));
    assert!(declaration_error(parse_radius_gateway_config(
        b"{\"baseUrl\":\"https://radius.example\",\"models\":[],\"unknown\":1}"
    )));
}

#[test]
fn bedrock_profile_arn_regions_drive_endpoint_and_signing_scope_together() {
    let model = "arn:aws:bedrock:ap-southeast-2:123456789012:application-inference-profile/profile-1";
    let arn = parse_bedrock_arn(model).unwrap();
    assert!(arn.is_application_inference_profile());
    assert_eq!(bedrock_model_region(model).as_deref(), Some("ap-southeast-2"));
    assert_eq!(
        resolve_bedrock_region(
            Some("us-east-1"),
            model,
            &"https://bedrock-runtime.us-east-1.amazonaws.com/"
                .parse()
                .unwrap()
        )
        .as_deref(),
        Some("ap-southeast-2")
    );
    let moved = bedrock_runtime_endpoint(
        &"https://bedrock-runtime.us-east-1.amazonaws.com/"
            .parse()
            .unwrap(),
        model,
        Some("us-east-1"),
    )
    .unwrap();
    assert_eq!(
        moved.as_str(),
        "https://bedrock-runtime.ap-southeast-2.amazonaws.com/"
    );
    assert_eq!(bedrock_runtime_host("ap-southeast-2", false), "bedrock-runtime.ap-southeast-2.amazonaws.com");
    assert_eq!(
        bedrock_endpoint_region(&"https://bedrock-runtime-fips.us-gov-west-1.amazonaws.com/".parse().unwrap())
            .as_deref(),
        Some("us-gov-west-1")
    );
    assert_eq!(
        resolve_bedrock_region(None, "anthropic.claude-3-5-sonnet-20240620-v1:0", &"https://proxy.internal/".parse().unwrap())
            .as_deref(),
        Some(DEFAULT_BEDROCK_REGION)
    );
    for malformed in [
        "arn:aws:bedrock:eu-central-1:123456789012",
        "arn:aws:bedrock:EU-CENTRAL-1:123456789012:inference-profile/x",
        "arn:aws:bedrock:eu-central-1:123456789012:inference-profile/",
    ] {
        assert!(parse_bedrock_arn(malformed).is_none(), "{malformed}");
    }
}

#[test]
fn codex_transport_selection_is_per_request_and_endpoint_declared() {
    let ws = EndpointTransport::WebSocketPreferred;
    let resolved = resolve_codex_transport(CodexTransport::Auto, ws, false, true);
    assert!(resolved.uses_websocket());
    assert!(resolved.cached_context);
    assert_eq!(resolved.transport, CodexTransport::WebSocket);
    assert_eq!(resolved.requested, CodexTransport::Auto);
    let resolved = resolve_codex_transport(CodexTransport::WebSocketCached, ws, false, false);
    assert_eq!(resolved.transport, CodexTransport::WebSocket);
    assert_eq!(resolved.reason, CodexTransportReason::NoSession);
    let resolved = resolve_codex_transport(CodexTransport::WebSocket, EndpointTransport::Http, false, true);
    assert_eq!(resolved.transport, CodexTransport::Sse);
    assert_eq!(resolved.requested, CodexTransport::WebSocket);
    assert_eq!(resolved.reason, CodexTransportReason::EndpointDeclaresHttp);
    let resolved = resolve_codex_transport(CodexTransport::Sse, ws, false, true);
    assert_eq!(resolved.reason, CodexTransportReason::SseRequested);
    let resolved = resolve_codex_transport(CodexTransport::Auto, ws, true, true);
    assert_eq!(resolved.reason, CodexTransportReason::SessionSseFallback);

    let mut overrides = RequestOverrides {
        codex_transport: Some(CodexTransport::WebSocketCached),
        codex_connect_timeout_ms: Some(2_500),
        ..Default::default()
    };
    overrides.validate().unwrap();
    let encoded = serde_json::to_value(&overrides).unwrap();
    assert_eq!(encoded["codex_transport"], json!("websocket-cached"));
    assert_eq!(encoded["codex_connect_timeout_ms"], json!(2_500));
    let decoded: RequestOverrides = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.codex_transport, Some(CodexTransport::WebSocketCached));
    overrides.codex_connect_timeout_ms = Some(MAX_CODEX_WEBSOCKET_CONNECT_TIMEOUT_MS + 1);
    assert!(declaration_error(overrides.validate()));
    assert!(normalize_codex_timeout_ms(Some(0)).unwrap() == Some(0));
    assert_eq!(effective_codex_connect_timeout_ms(None).unwrap(), Some(15_000));
    assert_eq!(effective_codex_connect_timeout_ms(Some(0)).unwrap(), None);
}

#[test]
fn codex_debug_stats_never_retain_unbounded_or_control_text() {
    let mut stats = CodexWebSocketDebugStats::default();
    stats.record_request(true, false, false, 4, Some(("resp_1", 4)));
    stats.record_websocket_failure(&format!("{}\u{1}boom", "x".repeat(4_096)));
    let error = stats.last_websocket_error.clone().unwrap();
    assert!(error.len() <= 256);
    assert!(!error.chars().any(char::is_control));
    assert_eq!(stats.last_previous_response_id.as_deref(), Some("resp_1"));
}

#[test]
fn anthropic_compat_and_strict_declarations_validate_fail_closed() {
    let preset = ModelPreset {
        supports_strict_mode: Some(false),
        supports_openai_grammar_tools: Some(true),
        anthropic_compat: Some(AnthropicCompatPreset {
            supports_strict_tools: Some(true),
            allowed_fallback_models: vec![AnthropicFallbackModel {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                cost: Some(AnthropicFallbackCost {
                    input: 1.0,
                    output: 5.0,
                    cache_read: 0.1,
                    cache_write: 1.25,
                }),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    preset.validate().unwrap();
    let encoded = serde_json::to_value(&preset).unwrap();
    assert_eq!(encoded["supports_openai_grammar_tools"], json!(true));
    assert_eq!(
        encoded["anthropic_compat"]["allowed_fallback_models"][0]["model"],
        json!("claude-haiku-4-5")
    );
    let decoded: ModelPreset = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, preset);

    // Pi-style camelCase declaration keys are accepted on input while octet
    // keeps its snake_case output spelling.
    let camel: ModelPreset = serde_json::from_value(json!({
        "supportsStrictMode": true,
        "supportsOpenAIGrammarTools": true,
        "anthropicCompat": {
            "supportsEagerToolInputStreaming": false,
            "supportsStrictTools": true,
            "supportsMidConvoEffort": true,
            "forceAdaptiveThinking": true,
            "allowEmptySignature": true,
            "allowedFallbackModels": [
                {"provider": "anthropic", "model": "claude-haiku-4-5",
                 "cost": {"input": 1.0, "output": 5.0, "cacheRead": 0.1, "cacheWrite": 1.25}}
            ]
        }
    }))
    .unwrap();
    assert_eq!(camel.supports_strict_mode, Some(true));
    let compat = camel.anthropic_compat.as_ref().expect("declared compat");
    assert_eq!(compat.supports_eager_tool_input_streaming, Some(false));
    assert!(compat.supports_mid_convo_effort == Some(true));
    assert_eq!(
        compat.allowed_fallback_models[0]
            .cost
            .map(|cost| cost.cache_write),
        Some(1.25)
    );
    camel.validate().unwrap();
    let round_trip = serde_json::to_value(&camel).unwrap();
    assert_eq!(round_trip["supports_strict_mode"], json!(true));
    assert!(round_trip["anthropic_compat"]
        .get("allowedFallbackModels")
        .is_none());

    let invalid_cost = ModelPreset {
        anthropic_compat: Some(AnthropicCompatPreset {
            allowed_fallback_models: vec![AnthropicFallbackModel {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                cost: Some(AnthropicFallbackCost {
                    input: f64::NAN,
                    output: 1.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                }),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(declaration_error(invalid_cost.validate()));
    let missing_model = ModelPreset {
        anthropic_compat: Some(AnthropicCompatPreset {
            allowed_fallback_models: vec![AnthropicFallbackModel {
                provider: "anthropic".into(),
                model: String::new(),
                cost: None,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(declaration_error(missing_model.validate()));
    // Debug must never surface a configured header value.
    let secret = ModelPreset {
        headers: BTreeMap::from([("x-model".to_owned(), "fixture-secret".to_owned())]),
        ..Default::default()
    };
    assert!(!format!("{secret:?}").contains("fixture-secret"));
}

fn declaration_error(error: Result<impl Sized, DeclarationError>) -> bool {
    let message = match error {
        Ok(_) => return false,
        Err(error) => format!("{error} {error:?}"),
    };
    !message.contains("fixture-secret") && !message.contains("private-secret")
}
