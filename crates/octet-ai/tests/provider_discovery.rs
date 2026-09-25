#![allow(missing_docs)]

use octet_ai::discovery::{DiscoverySource, ModelSelfDescription, MAX_SELF_DESCRIPTION_BYTES};
use octet_ai::{EndpointId, Modality, Protocol, ReasoningConfig, ReasoningEffort};
use serde_json::{json, Value};

fn source(protocol: Protocol) -> DiscoverySource {
    DiscoverySource {
        endpoint: EndpointId("selected-endpoint".into()),
        api_name: "future-model-not-in-any-snapshot".into(),
        protocol,
    }
}

fn declaration(protocol: Protocol) -> Value {
    json!({"version":1,"protocol":protocol,"context_window":64000,"max_output_tokens":4096,
        "input_modalities":["text","image"],"output_modalities":["text"],
        "tools":true,"parallel_tool_calls":true,"structured_output":true,
        "reasoning":{"values":["none","low","high"],"default":"low"}})
}

#[test]
fn unknown_models_keep_endpoint_provenance_and_exact_capabilities() {
    for protocol in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        let source = source(protocol);
        let description = ModelSelfDescription::from_entry(
            &json!({"octet_capabilities":declaration(protocol)}),
            source.clone(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(description.source(), &source);
        assert_eq!(description.limits().context_window, 64000);
        assert_eq!(description.limits().max_output_tokens, 4096);
        let caps = description.capabilities();
        assert!(caps.input_modalities.contains(Modality::Image));
        assert!(caps.tools && caps.parallel_tool_calls && caps.structured_output);
        let reasoning = caps.reasoning.as_ref().unwrap();
        assert!(reasoning.supports(&ReasoningConfig::Off));
        assert!(reasoning.supports(&ReasoningConfig::Effort(ReasoningEffort::High)));
        assert!(!reasoning.supports(&ReasoningConfig::Effort(ReasoningEffort::Medium)));
        assert!(!caps.responses_lite && !caps.deferred_tool_loading);
        assert_eq!(caps.agent_delegation, None);
    }
}

#[test]
fn absent_description_is_distinct_from_closed_world_defaults_and_null() {
    assert!(
        ModelSelfDescription::from_entry(&json!({}), source(Protocol::OpenAiChat))
            .unwrap()
            .is_none()
    );
    assert!(ModelSelfDescription::from_entry(
        &json!({"octet_capabilities":null}),
        source(Protocol::OpenAiChat)
    )
    .is_err());
    let entry = json!({"octet_capabilities":{"version":1,"protocol":"open_ai_chat",
        "context_window":4096,"max_output_tokens":512}});
    // Protocol spelling is the canonical Protocol serde spelling, not aliases.
    let description = ModelSelfDescription::from_entry(&entry, source(Protocol::OpenAiChat))
        .unwrap()
        .unwrap();
    let caps = description.capabilities();
    assert!(!caps.tools && !caps.parallel_tool_calls && !caps.structured_output);
    assert!(caps.reasoning.is_none());
    assert!(!caps.input_modalities.contains(Modality::Image));
}

#[test]
fn malformed_declarations_fail_closed_without_echoing_values() {
    for (key, value) in [
        ("version", json!(2)),
        ("version", json!("1")),
        ("context_window", json!(0)),
        ("context_window", json!(-1)),
        ("max_output_tokens", json!(64001)),
        ("max_output_tokens", json!(null)),
        ("tools", json!(null)),
        ("tools", json!("fixture-secret")),
        ("input_modalities", json!(["image", "image"])),
        ("reasoning", json!({"values":["low","low"]})),
        (
            "reasoning",
            json!({"values":["low","high"],"default":"medium"}),
        ),
        ("reasoning", json!({"values":["none"]})),
        (
            "reasoning",
            json!({"values":["fixture-secret".repeat(MAX_SELF_DESCRIPTION_BYTES)]}),
        ),
        ("source", json!("fixture-secret")),
    ] {
        let mut value_to_decode = declaration(Protocol::OpenAiChat);
        value_to_decode[key] = value;
        let error = ModelSelfDescription::from_entry(
            &json!({"octet_capabilities":value_to_decode}),
            source(Protocol::OpenAiChat),
        )
        .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("fixture-secret"));
    }
    let mut value = declaration(Protocol::OpenAiChat);
    value["tools"] = json!(false); // parallel=true cannot invent tool support
    assert!(ModelSelfDescription::from_entry(
        &json!({"octet_capabilities":value}),
        source(Protocol::OpenAiChat)
    )
    .is_err());
}

#[test]
fn unsupported_capabilities_and_routes_cannot_be_enabled_by_inventory() {
    for (key, value) in [
        ("input_modalities", json!(["audio"])),
        ("output_modalities", json!(["image"])),
        ("responses_lite", json!(true)),
        ("agent_delegation", json!("v2")),
        ("reasoning", json!({"values":["ultra"]})),
        ("reasoning", json!({"values":["on"]})),
        (
            "reasoning",
            json!({"values":["low"],"profile":"deep_seek_thinking"}),
        ),
        ("protocol", json!(Protocol::OpenAiResponses)),
        ("base_url", json!("https://fixture-secret.invalid/")),
    ] {
        let mut declaration = declaration(Protocol::OpenAiChat);
        declaration[key] = value;
        assert!(ModelSelfDescription::from_entry(
            &json!({"octet_capabilities":declaration}),
            source(Protocol::OpenAiChat)
        )
        .is_err());
    }
    for protocol in [
        Protocol::AnthropicMessages,
        Protocol::GoogleGenerativeAi,
        Protocol::BedrockConverse,
        Protocol::MistralConversations,
    ] {
        assert!(ModelSelfDescription::from_entry(
            &json!({"octet_capabilities":declaration(protocol)}),
            source(protocol)
        )
        .is_err());
    }
}
