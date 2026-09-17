#![allow(missing_docs)]

//! Public image-generation API regressions (ledger 1e.3): catalog, OpenRouter
//! adapter request/response, bounds, failure mapping, and option refusal.

use std::sync::{Arc, Mutex};

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use octet_ai::{
    AiClient, AiError, HookModelContext, ImageCancellation, ImageGenerationOptions,
    ImageGenerationRequest, ImageInput, ImageModel, ImageModelCatalog, ImageStopReason,
    ResponseHook, MAX_IMAGE_PROMPT_BYTES, OPENROUTER_API_KEY_VAR,
};

fn model_for(server: &MockServer) -> ImageModel {
    let raw = format!(
        r#"{{
            "version": 1,
            "models": [{{
                "id": "test/image-model",
                "name": "Test Image",
                "api": "openrouter-images",
                "provider": "openrouter",
                "base_url": "{}/",
                "input": ["text", "image"],
                "output": ["image", "text"],
                "cost": {{"input": 1000000, "output": 2000000, "cache_read": 100000, "cache_write": 250000}}
            }}]
        }}"#,
        server.uri()
    );
    let catalog = ImageModelCatalog::from_json(&raw).unwrap();
    catalog.resolve("openrouter", "test/image-model").unwrap()
}

fn options_with_key() -> ImageGenerationOptions {
    let mut options = ImageGenerationOptions::default();
    options.env.insert(
        OPENROUTER_API_KEY_VAR.to_owned(),
        "test-image-key".to_owned(),
    );
    options
}

fn image_payload() -> serde_json::Value {
    serde_json::json!({
        "id": "gen-1",
        "choices": [{
            "message": {
                "content": "rendered",
                "images": [
                    {"image_url": "data:image/png;base64,iVBORw0KGgo="},
                    {"image_url": {"url": "data:image/jpeg;base64,/9j/4A=="}}
                ]
            }
        }],
        "usage": {
            "prompt_tokens": 7,
            "completion_tokens": 2,
            "prompt_tokens_details": {"cached_tokens": 2}
        }
    })
}

#[tokio::test]
async fn openrouter_images_adapter_sends_a_bounded_body_and_parses_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(image_payload()))
        .mount(&server)
        .await;
    let model = model_for(&server);
    let client = AiClient::new();
    let request = ImageGenerationRequest::new(vec![
        ImageInput::Text("cat".to_owned()),
        ImageInput::Image {
            media_type: mime::IMAGE_PNG,
            data: bytes::Bytes::from_static(&[137, 80, 78, 71]),
        },
    ]);

    let response = client
        .generate_images_with_options(&model, request, options_with_key())
        .await
        .unwrap();
    assert_eq!(response.api, octet_ai::ImageApi::OpenRouterImages);
    assert_eq!(response.provider, "openrouter");
    assert_eq!(response.model, "test/image-model");
    assert_eq!(response.stop_reason, ImageStopReason::Stop);
    assert_eq!(response.response_id.as_deref(), Some("gen-1"));
    assert_eq!(response.images().len(), 2);
    assert_eq!(&response.images()[0].media_type, &mime::IMAGE_PNG);
    let usage = response.usage.expect("usage");
    assert_eq!(usage.total_tokens, 9);
    assert_eq!(usage.cache_read_tokens, 2);
    assert!(response.cost.is_some());

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-image-key")
    );
    assert!(requests[0].url.path().ends_with("/chat/completions"));
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "test/image-model");
    assert_eq!(body["stream"], false);
    assert_eq!(body["modalities"], serde_json::json!(["image", "text"]));
    assert_eq!(body["messages"][0]["content"][0]["text"], "cat");
    assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
}

#[tokio::test]
async fn image_failures_map_to_typed_http_errors_and_error_results() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string("upstream exploded")
                .insert_header("retry-after", "17"),
        )
        .mount(&server)
        .await;
    let model = model_for(&server);
    let client = AiClient::new();

    let error = client
        .generate_images_with_options(
            &model,
            ImageGenerationRequest::text("cat"),
            options_with_key(),
        )
        .await
        .err()
        .expect("500 must be an error");
    match error {
        AiError::Http(http) => {
            assert_eq!(http.status, http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(http.retry_after, Some(std::time::Duration::from_secs(17)));
            assert!(http.retryable);
        }
        other => panic!("unexpected image error: {other:?}"),
    }

    let reported = client
        .generate_images_reporting(
            &model,
            ImageGenerationRequest::text("cat"),
            options_with_key(),
        )
        .await;
    assert_eq!(reported.stop_reason, ImageStopReason::Error);
    assert!(reported.error_message.is_some());
    assert!(reported.output.is_empty());
}

#[tokio::test]
async fn image_bounds_and_host_owned_options_fail_before_dispatch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(image_payload()))
        .mount(&server)
        .await;
    let model = model_for(&server);
    let client = AiClient::new();

    let oversized = ImageGenerationRequest::text("x".repeat(MAX_IMAGE_PROMPT_BYTES + 1));
    assert!(client
        .generate_images_with_options(&model, oversized, options_with_key())
        .await
        .is_err());

    let mut retries = options_with_key();
    retries.max_retries = Some(2);
    assert!(client
        .generate_images_with_options(&model, ImageGenerationRequest::text("cat"), retries)
        .await
        .is_err());

    let mut metadata = options_with_key();
    metadata
        .runtime
        .metadata
        .insert("user".to_owned(), serde_json::json!("abc"));
    assert!(client
        .generate_images_with_options(&model, ImageGenerationRequest::text("cat"), metadata)
        .await
        .is_err());

    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn image_cancellation_reports_aborted_before_dispatch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(image_payload()))
        .mount(&server)
        .await;
    let model = model_for(&server);
    let client = AiClient::new();
    let cancellation = ImageCancellation::new();
    cancellation.cancel();
    assert!(cancellation.is_cancelled());

    let mut options = options_with_key();
    options.cancel = Some(cancellation.clone());
    let reported = client
        .generate_images_reporting(&model, ImageGenerationRequest::text("cat"), options)
        .await;
    assert_eq!(reported.stop_reason, ImageStopReason::Aborted);
    assert!(reported.error_message.is_some());

    let mut options = options_with_key();
    options.cancel = Some(cancellation);
    let error = client
        .generate_images_with_options(&model, ImageGenerationRequest::text("cat"), options)
        .await
        .err()
        .expect("cancelled request must not dispatch");
    assert!(matches!(error, AiError::Canceled));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn image_response_hook_observes_the_provider_response() {
    struct Observe(Arc<Mutex<Option<u16>>>);
    impl ResponseHook for Observe {
        fn on_response(
            &self,
            status: http::StatusCode,
            _headers: &http::HeaderMap,
            model: &HookModelContext,
        ) {
            assert_eq!(model.api, "openrouter-images");
            *self.0.lock().unwrap() = Some(status.as_u16());
        }
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(image_payload()))
        .mount(&server)
        .await;
    let model = model_for(&server);
    let observed = Arc::new(Mutex::new(None));
    let mut options = options_with_key();
    options.runtime.on_response = Some(Arc::new(Observe(observed.clone())));
    let client = AiClient::new();
    client
        .generate_images_with_options(&model, ImageGenerationRequest::text("cat"), options)
        .await
        .unwrap();
    assert_eq!(*observed.lock().unwrap(), Some(200));
}

#[test]
fn image_catalog_exposes_the_builtin_snapshot_models() {
    let catalog = ImageModelCatalog::builtin().unwrap();
    let models = catalog.models("openrouter");
    assert!(!models.is_empty());
    assert!(models.iter().all(|model| model
        .spec
        .output
        .contains(&octet_ai::ImageModality::Image)));
    let dynamic = catalog
        .resolve("openrouter", "openrouter/auto")
        .expect("dynamic router is a valid image route");
    assert!(dynamic.spec.cost.is_none());
    let priced = catalog
        .resolve("openrouter", "google/gemini-3-pro-image")
        .expect("priced image route");
    assert!(priced.spec.cost.is_some());
}
