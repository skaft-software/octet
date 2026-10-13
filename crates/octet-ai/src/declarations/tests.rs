//! Unit tests for `crate::declarations`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::declarations`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use serde_json::json;

fn args(value: serde_json::Value) -> BTreeMap<String, ChatTemplateValue> {
    serde_json::from_value(value).expect("fixture chat template map")
}

#[test]
fn chat_template_arguments_interpolate_variables_and_literals() {
    let template = args(json!({
        "enable_thinking": {"$var": "thinking.enabled"},
        "budget": {"$var": "thinking.budget"},
        "effort": {"$var": "thinking.effort"},
        "static": {"nested": [1, 2]}
    }));
    let selection = ThinkingSelection {
        enabled: true,
        effort: Some("high".to_owned()),
        budget: Some(8192),
        level_map: BTreeMap::from([("high".to_owned(), Some("retain".to_owned()))]),
    };
    assert_eq!(
        selection.interpolate_chat_template(&template),
        Some(json!({
            "enable_thinking": true,
            "budget": 8192,
            "effort": "retain",
            "static": {"nested": [1, 2]}
        }))
    );
}

#[test]
fn chat_template_omits_unmapped_and_off_values() {
    let template = args(json!({
        "enable_thinking": {"$var": "thinking.enabled"},
        "budget": {"$var": "thinking.budget"},
        "effort": {"$var": "thinking.effort"},
        "omit": {"$var": "thinking.enabled", "omitWhenOff": true}
    }));
    let selection = ThinkingSelection {
        enabled: false,
        effort: Some("medium".to_owned()),
        budget: None,
        level_map: BTreeMap::from([("medium".to_owned(), None)]),
    };
    // `omit` drops for off; `effort` maps to unsupported; `budget` has no value.
    assert_eq!(
        selection.interpolate_chat_template(&template),
        Some(json!({"enable_thinking": false}))
    );

    let empty = args(json!({
        "budget": {"$var": "thinking.budget"},
        "omit": {"$var": "thinking.enabled", "omitWhenOff": true}
    }));
    assert_eq!(selection.interpolate_chat_template(&empty), None);
}

#[test]
fn unknown_chat_template_variable_fails_closed() {
    let template = args(json!({"x": {"$var": "thinking.bogus"}}));
    let preset = ModelPreset {
        chat_template_args: Some(template),
        ..ModelPreset::default()
    };
    assert_eq!(
        preset.validate(),
        Err(DeclarationError::UnknownChatTemplateVariable {
            name: "x".to_owned()
        })
    );
}

#[test]
fn declared_image_limits_are_model_specific_and_checked() {
    let limits = crate::media::ImageInputLimits {
        max_width: 2_000,
        max_height: 1_500,
        max_bytes: 2_000_000,
    };
    let declared = ModelPreset {
        image_input_limits: Some(limits),
        ..Default::default()
    };
    declared.validate().unwrap();
    let roundtrip: ModelPreset =
        serde_json::from_value(serde_json::to_value(&declared).unwrap()).unwrap();
    assert_eq!(roundtrip.image_input_limits, Some(limits));
    assert_eq!(ModelPreset::default().image_input_limits, None);
    let invalid = ModelPreset {
        image_input_limits: Some(crate::media::ImageInputLimits {
            max_width: 0,
            ..limits
        }),
        ..Default::default()
    };
    assert!(invalid.validate().is_err());
}

#[test]
fn model_preset_round_trips_and_rejects_bad_headers() {
    let preset = ModelPreset {
        sampling_params: BTreeMap::from([("top_p".to_owned(), json!(0.9))]),
        headers: BTreeMap::from([("x-model".to_owned(), "glm".to_owned())]),
        vllm_priority: Some(-5),
        supports_max_output_tokens: Some(false),
        thinking_token_budget_field: Some(ThinkingTokenBudgetField::Vllm),
        chat_template_args: Some(args(json!({
            "enable_thinking": {"$var": "thinking.enabled"}
        }))),
        thinking_format: Some(ThinkingFormat::Baseten),
        ..ModelPreset::default()
    };
    preset.validate().unwrap();
    let encoded = serde_json::to_value(&preset).unwrap();
    assert_eq!(encoded["vllm_priority"], json!(-5));
    assert_eq!(encoded["supports_max_output_tokens"], json!(false));
    assert_eq!(
        encoded["thinking_token_budget_field"],
        json!("thinking_token_budget")
    );
    assert_eq!(encoded["thinking_format"], json!("baseten"));
    let decoded: ModelPreset = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, preset);

    let bad = ModelPreset {
        headers: BTreeMap::from([("bad header".to_owned(), "v".to_owned())]),
        ..ModelPreset::default()
    };
    assert!(matches!(
        bad.validate(),
        Err(DeclarationError::InvalidHeaderName(_))
    ));
}

#[test]
fn string_thinking_is_distinct_and_excludes_chat_template_args() {
    assert_eq!(
        serde_json::to_value(ThinkingFormat::StringThinking).unwrap(),
        json!("string-thinking")
    );
    assert!(ThinkingFormat::StringThinking.is_string_thinking());
    let conflicted = ModelPreset {
        thinking_format: Some(ThinkingFormat::StringThinking),
        chat_template_args: Some(args(json!({"x": true}))),
        ..ModelPreset::default()
    };
    assert!(matches!(
        conflicted.validate(),
        Err(DeclarationError::Invalid(_))
    ));
}

#[test]
fn provider_credential_preset_orders_and_marks_bearer_aliases() {
    let preset = ProviderCredentialPreset {
        environment_variables: vec![
            "ANTHROPIC_AUTH_TOKEN".to_owned(),
            "ANTHROPIC_OAUTH_TOKEN".to_owned(),
            "ANTHROPIC_API_KEY".to_owned(),
        ],
        bearer_token_variables: vec![
            "ANTHROPIC_AUTH_TOKEN".to_owned(),
            "ANTHROPIC_OAUTH_TOKEN".to_owned(),
        ],
    };
    preset.validate().unwrap();
    assert!(preset.presents_as_bearer("ANTHROPIC_AUTH_TOKEN"));
    assert!(preset.presents_as_bearer("ANTHROPIC_OAUTH_TOKEN"));
    assert!(!preset.presents_as_bearer("ANTHROPIC_API_KEY"));

    let unlisted = ProviderCredentialPreset {
        environment_variables: vec!["ANTHROPIC_API_KEY".to_owned()],
        bearer_token_variables: vec!["ANTHROPIC_AUTH_TOKEN".to_owned()],
    };
    assert!(unlisted.validate().is_err());
    let vertex = ProviderCredentialPreset {
        environment_variables: vec!["GOOGLE_CLOUD_API_KEY".to_owned()],
        bearer_token_variables: Vec::new(),
    };
    vertex.validate().unwrap();
    assert!(!vertex.presents_as_bearer("GOOGLE_CLOUD_API_KEY"));
}

#[test]
fn request_overrides_are_bounded() {
    let ok = RequestOverrides {
        headers: BTreeMap::from([("x-session-affinity".to_owned(), "abc".to_owned())]),
        env: BTreeMap::from([("HTTP_PROXY".to_owned(), "http://localhost:8080".to_owned())]),
        timeout_ms: Some(600_000),
        max_retries: Some(2),
        max_retry_delay_ms: Some(60_000),
        ..Default::default()
    };
    ok.validate().unwrap();
    assert!(RequestOverrides {
        timeout_ms: Some(0),
        ..RequestOverrides::default()
    }
    .validate()
    .is_err());
    assert!(RequestOverrides {
        max_retries: Some(MAX_RETRIES_CEILING + 1),
        ..RequestOverrides::default()
    }
    .validate()
    .is_err());
}
