//! Shared Responses-path coverage; no provider calls or measured cache hits.

use std::sync::Arc;

use serde_json::{json, Value};

use super::{append_batch, history};
use crate::{
    CacheRetention, Model, ModelCatalog, Protocol, ReasoningConfig, ResponsesInput, ResponsesItem,
    ResponsesOptions, ResponsesRuntimeProfile, ServiceTier,
};

fn models() -> Vec<Model> {
    let catalog = ModelCatalog::builtin().unwrap();
    let models: Vec<_> = catalog
        .models()
        .filter(|spec| spec.protocol == Protocol::OpenAiResponses)
        .map(|spec| catalog.resolve(&spec.id).unwrap())
        .collect();
    for name in ["gpt-6-astra", "gpt-6-sol", "gpt-6.1-sol", "gpt-6-luna"] {
        assert!(models.iter().any(|model| model.spec.api_name == name));
    }
    models
}

fn request(model: &Model) -> crate::Request {
    let mut req = history();
    // Keep the older 128K route's fixture within its working context as well.
    if model.spec.limits.context_window < 200_000 {
        if let crate::Message::User(user) = &mut req.messages[0] {
            if let crate::UserPart::Text(text) = &mut user.content[0] {
                text.truncate(400_000);
            }
        }
    }
    req.reasoning = model
        .spec
        .capabilities
        .reasoning
        .as_ref()
        .and_then(crate::ReasoningCapability::default_selection)
        .unwrap_or(ReasoningConfig::Off);
    req.session_id = Some("responses-audit-session".into());
    req
}

fn body(model: &Model, req: &crate::Request) -> Value {
    serde_json::from_slice(
        &super::super::openai_responses::build_request(model, req)
            .unwrap()
            .body,
    )
    .unwrap()
}

#[test]
fn cache_audit_responses_canonical_batches_are_append_only_for_every_builtin_model() {
    for model in models() {
        for batch in [1, 32] {
            let mut req = request(&model);
            let mut previous: Option<Value> = None;
            let mut reused = 0;
            let mut total = 0;
            for turn in 0..9 {
                let unchanged = serde_json::to_value(&req).unwrap();
                let current = body(&model, &req);
                assert_eq!(current, body(&model, &req));
                assert_eq!(unchanged, serde_json::to_value(&req).unwrap());
                if let Some(previous) = &previous {
                    for key in [
                        "tools",
                        "reasoning",
                        "text",
                        "prompt_cache_key",
                        "prompt_cache_options",
                    ] {
                        assert_eq!(
                            previous.get(key),
                            current.get(key),
                            "{}: {key}",
                            model.spec.id.0
                        );
                    }
                    let old = previous["input"].as_array().unwrap();
                    let new = current["input"].as_array().unwrap();
                    assert_eq!(old, &new[..old.len()], "{} batch={batch}", model.spec.id.0);
                    let old_bytes: usize = old
                        .iter()
                        .map(|item| serde_json::to_vec(item).unwrap().len())
                        .sum();
                    let new_bytes: usize = new
                        .iter()
                        .map(|item| serde_json::to_vec(item).unwrap().len())
                        .sum();
                    let ratio = old_bytes as f64 / new_bytes as f64;
                    assert!(
                        ratio >= 0.98,
                        "{} batch={batch} turn={turn}: {ratio}",
                        model.spec.id.0
                    );
                    reused += old_bytes;
                    total += new_bytes;
                }
                previous = Some(current);
                append_batch(&mut req, Protocol::OpenAiResponses, turn, batch);
                let assistant_index = req.messages.len() - 2;
                if let crate::Message::Assistant(assistant) = &mut req.messages[assistant_index] {
                    assistant.model = model.spec.id.clone();
                }
            }
            println!("RESPONSES_PREFIX model={} batch={batch} requests=9 warmup=1 input_item_bytes={total} reusable_item_bytes={reused} efficiency={:.6}% live_provider_usage=NOT_RUN", model.spec.api_name, 100.0 * reused as f64 / total as f64);
        }
    }
}

#[test]
fn cache_audit_responses_service_tiers_preserve_authoritative_replay_and_cache_controls() {
    for model in models() {
        let mut req = request(&model);
        let mut input = crate::responses::encode_canonical_responses_input(
            &model,
            req.system.as_deref(),
            &req.messages,
            req.compatibility,
        )
        .into_items();
        input.push(
            ResponsesItem::new(json!({
                "type":"reasoning", "id":"rs_exact", "encrypted_content":"opaque-reasoning",
                "summary":[], "future_provider_field":{"keep":true}
            }))
            .unwrap(),
        );
        input.push(
            ResponsesItem::new(json!({
                "type":"message", "id":"msg_exact", "role":"assistant", "phase":"final_answer",
                "content":[{"type":"output_text","text":"reviewed","annotations":[]}],
                "future_provider_field":[1,2,3]
            }))
            .unwrap(),
        );
        // Reopening a journal also crosses a serde boundary.
        let input: ResponsesInput =
            serde_json::from_value(serde_json::to_value(ResponsesInput::new(input)).unwrap())
                .unwrap();
        req.responses = Some(ResponsesOptions::full_replay(input.clone()));
        let expected_input = serde_json::to_value(&input).unwrap();
        let public = body(&model, &req);
        assert_eq!(public["input"], expected_input);
        for tier in [
            ServiceTier::Auto,
            ServiceTier::Default,
            ServiceTier::Flex,
            ServiceTier::Priority,
        ] {
            req.responses.as_mut().unwrap().service_tier = Some(tier);
            assert!(matches!(
                super::super::openai_responses::build_request(&model, &req),
                Err(crate::AiError::Unsupported(
                    crate::UnsupportedError::ServiceTier
                ))
            ));
        }
        let mut declared = model.clone();
        Arc::make_mut(&mut declared.endpoint)
            .runtime
            .responses_profile = ResponsesRuntimeProfile::Codex;
        for retention in [
            CacheRetention::None,
            CacheRetention::Short,
            CacheRetention::Long,
        ] {
            req.cache_retention = retention;
            req.responses.as_mut().unwrap().service_tier = None;
            let baseline = body(&declared, &req);
            for tier in [
                ServiceTier::Auto,
                ServiceTier::Default,
                ServiceTier::Flex,
                ServiceTier::Priority,
            ] {
                req.responses.as_mut().unwrap().service_tier = Some(tier);
                let original = serde_json::to_value(&req).unwrap();
                let mut current = body(&declared, &req);
                assert_eq!(current["service_tier"], tier.wire_value());
                assert_eq!(current["input"], expected_input);
                assert_eq!(original, serde_json::to_value(&req).unwrap());
                current.as_object_mut().unwrap().remove("service_tier");
                assert_eq!(
                    current, baseline,
                    "{}: {tier:?} {retention:?}",
                    model.spec.id.0
                );
                // This profile does not inherit public OpenAI's explicit-cache-mode authority.
                assert!(current.get("prompt_cache_options").is_none());
            }
        }
    }
}

#[tokio::test]
async fn cache_audit_responses_terminal_cache_usage_is_model_and_tier_independent() {
    use super::super::{harness, openai_responses};
    for mut model in models() {
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            ResponsesRuntimeProfile::Codex;
        for terminal in ["response.completed", "response.incomplete"] {
            for tier in ["default", "flex", "priority"] {
                let data = format!(
                    "data: {}\n\ndata: {}\n\n",
                    json!({"type":"response.created","response":{"id":"response","usage":{"input_tokens":1}}}),
                    json!({"type":terminal,"response":{
                        "id":"response", "service_tier":tier, "incomplete_details":{"reason":"max_output_tokens"},
                        "output":[], "usage":{
                            "input_tokens":150_000, "input_tokens_details":{"cached_tokens":147_000,"cache_write_tokens":2_999},
                            "output_tokens":19, "output_tokens_details":{"reasoning_tokens":7}
                        }
                    }})
                );
                for chunk in [0, 1, 17] {
                    let events = harness::drive(
                        &model,
                        openai_responses::decode_stream_event,
                        data.as_bytes(),
                        chunk,
                    )
                    .await
                    .unwrap();
                    let usage = &harness::finished(&events).usage;
                    assert_eq!(usage.input_tokens, 1);
                    assert_eq!(usage.cache_read_tokens, 147_000);
                    assert_eq!(usage.cache_write_tokens, 2_999);
                    assert_eq!(
                        usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens,
                        150_000
                    );
                    assert_eq!(usage.output_tokens, 19);
                    assert_eq!(usage.reasoning_tokens, 7);
                    assert_eq!(usage.total_tokens, 150_019);
                }
            }
        }
    }
}
