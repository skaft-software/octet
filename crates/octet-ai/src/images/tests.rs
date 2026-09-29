//! Unit tests for `crate::images`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::images`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

fn fixture_model() -> ImageModel {
    let catalog = ImageModelCatalog::from_json(
        r#"{
            "version": 1,
            "models": [{
                "id": "test/image-model",
                "name": "Test Image",
                "api": "openrouter-images",
                "provider": "openrouter",
                "base_url": "https://openrouter.ai/api/v1/",
                "input": ["text", "image"],
                "output": ["image", "text"],
                "cost": {"input": 1000000, "output": 2000000, "cache_read": 100000, "cache_write": 250000}
            }]
        }"#,
    )
    .unwrap();
    catalog.resolve("openrouter", "test/image-model").unwrap()
}

fn unpriced_model() -> ImageModel {
    let catalog = ImageModelCatalog::from_json(
        r#"{
            "version": 1,
            "models": [{
                "id": "test/dynamic-router",
                "name": "Dynamic Router",
                "api": "openrouter-images",
                "provider": "openrouter",
                "base_url": "https://openrouter.ai/api/v1/",
                "input": ["text", "image"],
                "output": ["image", "text"],
                "cost": null
            }]
        }"#,
    )
    .unwrap();
    catalog
        .resolve("openrouter", "test/dynamic-router")
        .unwrap()
}

#[test]
fn builtin_catalog_loads_the_checked_in_snapshot() {
    let catalog = ImageModelCatalog::builtin().unwrap();
    assert!(!catalog.providers().is_empty());
    assert!(catalog
        .providers()
        .iter()
        .all(|provider| !catalog.models(provider).is_empty()));
    assert!(catalog.default_model().is_some());
}

#[test]
fn body_carries_ordered_inputs_and_modalities() {
    let model = fixture_model();
    let request = ImageGenerationRequest::new(vec![
        ImageInput::Text("draw a cat".to_owned()),
        ImageInput::Image {
            media_type: mime::IMAGE_PNG,
            data: bytes::Bytes::from_static(&[137, 80, 78, 71]),
        },
    ]);
    let body = build_openrouter_images_body(&model, &request).unwrap();
    assert_eq!(body["model"], "test/image-model");
    assert_eq!(body["stream"], false);
    assert_eq!(body["modalities"], serde_json::json!(["image", "text"]));
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
    assert_eq!(
        body["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,iVBORw=="
    );
}

#[test]
fn request_bounds_reject_empty_and_oversized_inputs() {
    assert!(ImageGenerationRequest::default().validate().is_err());
    let oversized = ImageGenerationRequest::text("x".repeat(MAX_IMAGE_PROMPT_BYTES + 1));
    assert!(oversized.validate().is_err());
    let empty_image = ImageGenerationRequest::new(vec![ImageInput::Image {
        media_type: mime::IMAGE_PNG,
        data: bytes::Bytes::new(),
    }]);
    assert!(empty_image.validate().is_err());
}

#[test]
fn response_parses_text_multiple_images_and_usage() {
    let model = fixture_model();
    let payload = serde_json::json!({
        "id": "gen-1",
        "choices": [{
            "message": {
                "content": "here you go",
                "images": [
                    {"image_url": "data:image/png;base64,iVBORw0KGgo="},
                    {"image_url": {"url": "data:image/jpeg;base64,/9j/4A=="}},
                    {"image_url": "https://example.invalid/not-inline.png"}
                ]
            }
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "prompt_tokens_details": {"cached_tokens": 30, "cache_write_tokens": 10}
        }
    });
    let response = parse_openrouter_images_response(&model, &payload).unwrap();
    assert_eq!(response.response_id.as_deref(), Some("gen-1"));
    assert_eq!(response.stop_reason, ImageStopReason::Stop);
    assert_eq!(response.images().len(), 2);
    assert_eq!(&response.images()[0].media_type, &mime::IMAGE_PNG);
    assert_eq!(&response.images()[1].media_type, &mime::IMAGE_JPEG);
    let usage = response.usage.unwrap();
    // Disjoint buckets: `cached_tokens` is the cache-read bucket, and the
    // uncached input is what remains of `prompt_tokens` after both cache
    // buckets (docs/telemetry.md; `openai_chat::map_usage`).
    assert_eq!(usage.input_tokens, 60);
    assert_eq!(usage.cache_read_tokens, 30);
    assert_eq!(usage.cache_write_tokens, 10);
    assert_eq!(usage.output_tokens, 10);
    assert_eq!(usage.total_tokens, 110);
    assert!(response.cost.is_some());
}

#[test]
fn unpriced_dynamic_router_reports_usage_without_a_cost() {
    let model = unpriced_model();
    let payload = serde_json::json!({
        "id": "gen-2",
        "choices": [{"message": {"images": [
            {"image_url": "data:image/png;base64,iVBORw0KGgo="}
        ]}}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2}
    });
    let response = parse_openrouter_images_response(&model, &payload).unwrap();
    assert!(response.usage.is_some());
    assert!(response.cost.is_none());
}

#[test]
fn response_skips_malformed_data_urls_and_rejects_over_cap_counts() {
    let model = fixture_model();
    let malformed = serde_json::json!({
        "choices": [{"message": {"images": [
            {"image_url": "data:image/png;base64,%%%"},
            {"image_url": 42}
        ]}}]
    });
    let response = parse_openrouter_images_response(&model, &malformed).unwrap();
    assert!(response.output.is_empty());

    let images: Vec<serde_json::Value> = (0..=MAX_GENERATED_IMAGES)
        .map(|_| serde_json::json!({"image_url": "data:image/png;base64,iVBORw0KGgo="}))
        .collect();
    let over_cap = serde_json::json!({"choices": [{"message": {"images": images}}]});
    assert!(parse_openrouter_images_response(&model, &over_cap).is_err());
}

#[test]
fn options_refuse_host_owned_fields() {
    let options = ImageGenerationOptions {
        max_retries: Some(1),
        ..Default::default()
    };
    assert!(options.validate().is_err());
    let options = ImageGenerationOptions {
        timeout_ms: Some(0),
        ..Default::default()
    };
    assert!(options.validate().is_err());
    let mut options = ImageGenerationOptions::default();
    options
        .headers
        .insert("authorization".to_owned(), "forged".to_owned());
    assert!(options.validate().is_err());
    let mut options = ImageGenerationOptions::default();
    options.env.insert("bad-name!".to_owned(), "x".to_owned());
    assert!(options.validate().is_err());
}

#[test]
fn catalog_rejects_unknown_and_malformed_records() {
    let error = ImageModelCatalog::from_json("{\"version\":1,\"models\":[]}").unwrap_err();
    assert!(matches!(error, ConfigError::Parse(_)));
    let unknown = ImageModelCatalog::from_json(
        r#"{"version":1,"models":[{"id":"x","name":"X","api":"openrouter-images","provider":"openrouter","base_url":"https://openrouter.ai/api/v1/","input":["text"],"output":["text"]}]}"#,
    )
    .unwrap_err();
    assert!(matches!(unknown, ConfigError::Parse(_)));
    let catalog = ImageModelCatalog::builtin().unwrap();
    assert!(matches!(
        catalog.resolve("openrouter", "missing/model"),
        Err(ConfigError::UnknownImageModel { .. })
    ));
}
