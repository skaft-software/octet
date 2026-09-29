//! Unit tests for `crate::protocol::mistral_conversations`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mistral_conversations.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::mistral_conversations`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::protocol::harness::model as harness_model;
use crate::types::UserMessage;

fn native_model(profile: Option<MistralReasoningProfile>) -> Model {
    let mut model = harness_model(Protocol::MistralConversations, None);
    std::sync::Arc::make_mut(&mut model.spec)
        .preset
        .mistral_reasoning = profile;
    model
}

fn request(reasoning: ReasoningConfig) -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".to_owned())],
        })],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: Vec::new(),
        reasoning,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    }
}

fn completion_args(model: &Model, req: &Request) -> serde_json::Value {
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(model, req).unwrap().body).unwrap();
    body["completion_args"].clone()
}

#[test]
fn declared_profiles_emit_exactly_one_native_reasoning_control() {
    let mut effort_model = native_model(Some(MistralReasoningProfile::ReasoningEffort));
    std::sync::Arc::make_mut(&mut effort_model.spec)
        .preset
        .thinking_level_map
        .insert("high".to_owned(), Some("none".to_owned()));
    let args = completion_args(
        &effort_model,
        &request(ReasoningConfig::Effort(crate::types::ReasoningEffort::High)),
    );
    assert_eq!(args["reasoning_effort"], "none");
    assert!(args.get("prompt_mode").is_none());

    // An unmapped level uses Pi's `mapReasoningEffort` fallback: Pi's own
    // Mistral wire enum collapses every enabled level to `high` unless the
    // model's `thinkingLevelMap` names a different value.
    let default_model = native_model(Some(MistralReasoningProfile::ReasoningEffort));
    let args = completion_args(
        &default_model,
        &request(ReasoningConfig::Effort(crate::types::ReasoningEffort::Low)),
    );
    assert_eq!(args["reasoning_effort"], "high");
    assert!(args.get("prompt_mode").is_none());

    // A declared mapping is authoritative for that level.
    let mut mapped_model = native_model(Some(MistralReasoningProfile::ReasoningEffort));
    std::sync::Arc::make_mut(&mut mapped_model.spec)
        .preset
        .thinking_level_map
        .insert("low".to_owned(), Some("low".to_owned()));
    let args = completion_args(
        &mapped_model,
        &request(ReasoningConfig::Effort(crate::types::ReasoningEffort::Low)),
    );
    assert_eq!(args["reasoning_effort"], "low");

    let prompt_model = native_model(Some(MistralReasoningProfile::PromptMode));
    let args = completion_args(
        &prompt_model,
        &request(ReasoningConfig::Effort(crate::types::ReasoningEffort::High)),
    );
    assert_eq!(args["prompt_mode"], "reasoning");
    assert!(args.get("reasoning_effort").is_none());

    // Reasoning off emits neither native control.
    let args = completion_args(&prompt_model, &request(ReasoningConfig::Off));
    assert!(args.get("prompt_mode").is_none());
    assert!(args.get("reasoning_effort").is_none());
}

#[test]
fn undeclared_or_budget_reasoning_fails_closed() {
    for profile in [
        None,
        Some(MistralReasoningProfile::ReasoningEffort),
        Some(MistralReasoningProfile::PromptMode),
    ] {
        let model = native_model(profile);
        // A token budget has no native field on this wire, and a model
        // without a declared profile rejects every enabled control.
        assert!(build_request(&model, &request(ReasoningConfig::Budget(4096))).is_err());
        assert!(
            build_request(&model, &request(ReasoningConfig::Off)).is_ok(),
            "{profile:?}"
        );
        if profile.is_none() {
            assert!(build_request(
                &model,
                &request(ReasoningConfig::Effort(crate::types::ReasoningEffort::High))
            )
            .is_err());
        }
    }
    // Even a token-budget-capable model keeps the native rejection: the
    // Conversations `CompletionArgs` shape has no budget field.
    let mut budget_capable = native_model(None);
    std::sync::Arc::make_mut(&mut budget_capable.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap()
        .control = crate::types::ReasoningControl::TokenBudget;
    assert!(build_request(&budget_capable, &request(ReasoningConfig::Budget(4096))).is_err());
}
