use super::*;
use octet_ai::{ProviderRequestContext, ProviderRequestHook};

#[derive(Default)]
struct WirePipeline {
    observations: Mutex<Vec<(String, &'static str)>>,
}

impl WirePipeline {
    fn observe(&self, context: &ProviderRequestContext, phase: &'static str) {
        assert_eq!(context.model.id, "hook-model");
        assert_eq!(context.model.provider, "hook-ep");
        assert_eq!(context.model.api, "openai-chat");
        self.observations.lock().unwrap().push((context.operation_id.clone(), phase));
    }
}

#[async_trait]
impl ProviderRequestHook for WirePipeline {
    async fn before_request(
        &self,
        context: &ProviderRequestContext,
        mut payload: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, AiError> {
        tokio::task::yield_now().await;
        self.observe(context, "payload");
        assert_eq!(payload["model"], "gpt-4-test");
        assert_eq!(payload["messages"][0]["content"], "go");
        assert!(payload.get("output_format").is_none(), "not canonical Request JSON");
        payload["wire_only_field"] = serde_json::json!({"nested":[1,"two"]});
        Ok(Some(payload))
    }

    async fn before_headers(
        &self,
        context: &ProviderRequestContext,
        headers: &mut http::HeaderMap,
    ) -> Result<(), AiError> {
        self.observe(context, "headers");
        assert!(!headers.contains_key("authorization"));
        headers.remove("x-remove");
        headers.insert("x-insert", http::HeaderValue::from_static("pipeline"));
        Ok(())
    }

    async fn after_response(
        &self,
        context: &ProviderRequestContext,
        status: http::StatusCode,
        headers: &http::HeaderMap,
    ) -> Result<(), AiError> {
        self.observe(context, "response");
        assert!(status == http::StatusCode::OK || status == http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(headers.get("x-marker").unwrap(), "observed");
        Ok(())
    }
}

#[tokio::test]
async fn async_pipeline_mutates_real_wire_and_observes_before_first_body_event() {
    let server = MockServer::start().await;
    sse_mock(&server).await;
    let mut model = test_model(&format!("{}/", server.uri()), Auth::bearer("authoritative"));
    Arc::make_mut(&mut model.endpoint).default_headers.insert("x-remove", http::HeaderValue::from_static("old"));
    let hook = Arc::new(WirePipeline::default());
    let original = AiClient::new();
    let client = original.with_provider_request_hooks(vec![hook.clone()]);
    assert!(!original.has_provider_request_hooks());
    for attempt in 0..2 {
        let mut stream = client.stream(&model, request()).await.unwrap();
        // Opening has completed the response observation without polling body.
        assert_eq!(hook.observations.lock().unwrap().len(), (attempt + 1) * 3);
        while let Some(event) = stream.next().await { event.unwrap(); }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(request.headers.get("authorization").unwrap(), "Bearer authoritative");
        assert!(!request.headers.contains_key("x-remove"));
        assert_eq!(request.headers.get("x-insert").unwrap(), "pipeline");
        let payload: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(payload["wire_only_field"], serde_json::json!({"nested":[1,"two"]}));
    }
    let observations = hook.observations.lock().unwrap();
    assert_eq!(observations.iter().map(|(_, phase)| *phase).collect::<Vec<_>>(),
        ["payload", "headers", "response", "payload", "headers", "response"]);
    assert_eq!(observations[0].0, observations[2].0);
    assert_eq!(observations[3].0, observations[5].0);
    assert_ne!(observations[0].0, observations[3].0);
}

#[tokio::test]
async fn async_response_observes_real_error_headers_once_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(429)
        .insert_header("x-marker", "observed")
        .set_body_string("{\"error\":{\"message\":\"limited\"}}"))
        .mount(&server).await;
    let model = test_model(&format!("{}/", server.uri()), Auth::None);
    let hook = Arc::new(WirePipeline::default());
    let client = AiClient::new().with_provider_request_hooks(vec![hook.clone()]);
    assert!(client.stream(&model, request()).await.is_err());
    assert_eq!(hook.observations.lock().unwrap().len(), 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

struct ReservedMutation { repeated_value_only: bool }
#[async_trait]
impl ProviderRequestHook for ReservedMutation {
    async fn before_headers(&self, _: &ProviderRequestContext, headers: &mut http::HeaderMap) -> Result<(), AiError> {
        if self.repeated_value_only {
            headers.remove("x-amz-security-token");
            headers.append("x-amz-security-token", http::HeaderValue::from_static("first"));
            headers.append("x-amz-security-token", http::HeaderValue::from_static("forged-second"));
        } else {
            headers.remove("host");
        }
        Ok(())
    }
}

#[tokio::test]
async fn async_reserved_header_removal_and_repeated_value_rewrite_fail_presend() {
    let server = MockServer::start().await;
    for repeated_value_only in [false, true] {
        let mut model = test_model(&format!("{}/", server.uri()), Auth::None);
        let headers = &mut Arc::make_mut(&mut model.endpoint).default_headers;
        headers.insert("host", http::HeaderValue::from_static("original"));
        headers.append("x-amz-security-token", http::HeaderValue::from_static("first"));
        headers.append("x-amz-security-token", http::HeaderValue::from_static("second"));
        let client = AiClient::new().with_provider_request_hooks(vec![Arc::new(ReservedMutation { repeated_value_only })]).track_request_dispatch();
        let error = client.stream(&model, request()).await.err().unwrap();
        assert!(matches!(error, AiError::Config(octet_ai::ConfigError::ReservedHeader(_))));
        assert!(!client.request_may_have_been_sent());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

struct HoldPayload(Arc<tokio::sync::Notify>);
#[async_trait]
impl ProviderRequestHook for HoldPayload {
    async fn before_request(&self, _: &ProviderRequestContext, _: serde_json::Value) -> Result<Option<serde_json::Value>, AiError> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn dropping_async_hook_opening_cancels_before_dispatch() {
    let server = MockServer::start().await;
    let model = test_model(&format!("{}/", server.uri()), Auth::None);
    let entered = Arc::new(tokio::sync::Notify::new());
    let client = AiClient::new().with_provider_request_hooks(vec![Arc::new(HoldPayload(entered.clone()))]).track_request_dispatch();
    {
        let opening = client.stream(&model, request());
        tokio::pin!(opening);
        tokio::select! {
            _ = &mut opening => panic!("held hook unexpectedly completed"),
            _ = entered.notified() => {},
        }
    }
    assert!(!client.request_may_have_been_sent());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn async_hook_deadline_fails_closed_and_does_not_log_payload() {
    let server = MockServer::start().await;
    let mut model = test_model(&format!("{}/", server.uri()), Auth::None);
    Arc::make_mut(&mut model.endpoint).timeout = Duration::from_millis(10);
    let client = AiClient::new().with_provider_request_hooks(vec![Arc::new(HoldPayload(Arc::new(tokio::sync::Notify::new())))]);
    let error = client.stream(&model, request()).await.err().unwrap();
    assert!(matches!(error, AiError::Config(octet_ai::ConfigError::Parse(ref message)) if message == "provider hook deadline exceeded"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn opaque_provider_transport_refuses_pipeline_without_fake_callbacks() {
    let model = test_model("http://127.0.0.1:1/", Auth::None);
    let hook = Arc::new(WirePipeline::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let client = AiClient::new().with_provider_request_hooks(vec![hook.clone()]);
    client.register_host_stream_transport(model.endpoint.id.clone(), Arc::new(CountingTransport { calls: calls.clone() }));
    assert!(client.stream(&model, request()).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(hook.observations.lock().unwrap().is_empty());
}

struct OrderedPayload { stage: u64, seen: Arc<Mutex<Vec<u64>>> }
#[async_trait]
impl ProviderRequestHook for OrderedPayload {
    async fn before_request(&self, _: &ProviderRequestContext, mut payload: serde_json::Value) -> Result<Option<serde_json::Value>, AiError> {
        assert_eq!(payload["stage"].as_u64().unwrap_or(0), self.stage - 1);
        self.seen.lock().unwrap().push(self.stage);
        payload["stage"] = self.stage.into();
        Ok(Some(payload))
    }
}

#[tokio::test]
async fn client_and_request_async_hooks_apply_in_order() {
    let server = MockServer::start().await;
    sse_mock(&server).await;
    let model = test_model(&format!("{}/", server.uri()), Auth::None);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = AiClient::new().with_provider_request_hooks(vec![Arc::new(OrderedPayload { stage: 1, seen: seen.clone() })]);
    let options = HostRequestOptions { provider_hooks: vec![Arc::new(OrderedPayload { stage: 2, seen: seen.clone() })], ..Default::default() };
    client.complete_with_host_options(&model, request(), RequestOverrides::default(), options).await.unwrap();
    assert_eq!(*seen.lock().unwrap(), [1, 2]);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap()["stage"], 2);
}

struct ScalarPayload;
#[async_trait]
impl ProviderRequestHook for ScalarPayload {
    async fn before_request(&self, _: &ProviderRequestContext, _: serde_json::Value) -> Result<Option<serde_json::Value>, AiError> {
        Ok(Some(serde_json::json!("private-scalar-value")))
    }
}

#[tokio::test]
async fn malformed_async_payload_is_refused_without_echoing_private_value() {
    let model = test_model("http://127.0.0.1:1/", Auth::None);
    let client = AiClient::new().with_provider_request_hooks(vec![Arc::new(ScalarPayload)]).track_request_dispatch();
    let error = client.stream(&model, request()).await.err().unwrap();
    assert!(!error.to_string().contains("private-scalar-value"));
    assert!(!client.request_may_have_been_sent());
}
