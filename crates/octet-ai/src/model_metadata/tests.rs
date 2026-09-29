//! Unit tests for `crate::model_metadata`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::model_metadata`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn generated_registry_resolves_canonical_and_unique_leaf_ids() {
    assert!(model_display_name("openai/gpt-4o-mini").is_some());
    assert_eq!(
        model_display_name("alibaba/qwen3.6-27b"),
        Some("Qwen3.6 27B")
    );
    assert_eq!(model_display_name("qwen3.6-27b"), Some("Qwen3.6 27B"));
}

#[test]
fn generated_pricing_is_provider_scoped_and_integer_based() {
    let direct = model_pricing("openai", "gpt-5").expect("snapshot price");
    assert_eq!(direct.input, TokenRate(1_250_000));
    assert_eq!(direct.output, TokenRate(10_000_000));

    let routed = model_pricing("openrouter", "deepseek/deepseek-v4-pro")
        .expect("provider-specific snapshot price");
    assert_eq!(routed.input, TokenRate(348_000));
    assert_eq!(routed.output, TokenRate(696_000));
    assert_eq!(routed.reasoning, None);
    assert!(model_pricing("openai", "gpt-5.6").is_none());
    assert!(model_pricing("openai", "gpt-5.6-sol").is_some());
    assert!(model_pricing("unknown", "model").is_none());
}

#[test]
fn generated_capabilities_keep_exact_provider_scoped_source_assertions() {
    let flash = model_capability_metadata("deepseek", "deepseek-flash").unwrap();
    assert_eq!(flash["name"], "DeepSeek V4.1 Flash");
    assert_eq!(flash["limit"]["context"], 1_000_000);
    assert_eq!(flash["limit"]["output"], 393_216);
    assert_eq!(
        flash["reasoning_options"][1]["values"],
        serde_json::json!(["low", "high", "max"])
    );
    assert_eq!(flash["interleaved"]["field"], "reasoning_content");
    assert_eq!(
        flash["modalities"]["input"],
        serde_json::json!(["text", "image"])
    );
    assert_eq!(
        model_display_name("deepseek/deepseek-flash"),
        Some("DeepSeek V4.1 Flash")
    );
    for provider in ["openai", "codex", "custom", "openrouter"] {
        assert!(model_capability_metadata(provider, "deepseek-flash").is_none());
    }
    assert!(model_capability_metadata("deepseek", "DEEPSEEK-FLASH").is_none());
    assert!(model_pricing("deepseek", "deepseek-flash").is_none());
}

#[test]
fn generated_registry_leaves_unknown_ids_untouched_for_the_caller() {
    assert_eq!(model_display_name("acme/unknown-model-v9"), None);
}
