use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::json;

use super::*;
use crate::{
    AiClient, Auth, Capabilities, Diagnostic, Endpoint, EndpointId, EndpointTransport,
    HookModelContext, HostRequestOptions, HostStreamModel, HostStreamTransport, Message,
    ModalitySet, ModelId, ModelLimits, ModelSpec, Protocol, ProviderRequestContext,
    ProviderRequestHook, Request, RequestOverrides, ResponseHook, ResponseStream, StopReason,
    StreamEvent, UserMessage, UserPart,
};

const CHAT_BODY: &str = concat!(
    "data: {\"id\":\"custom-response\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
    "data: {\"id\":\"custom-response\",\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":17,\"completion_tokens\":5,\"total_tokens\":22}}\n\n",
    "data: [DONE]\n\n",
);

type Observations = Arc<Mutex<Vec<&'static str>>>;

fn model(protocol: Protocol) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            id: ModelId("custom/helper-model".into()),
            endpoint: EndpointId("custom/helper".into()),
            api_name: "wire-model".into(),
            display_name: None,
            protocol,
            capabilities: Capabilities {
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: false,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
                responses_features: Default::default(),
            },
            limits: ModelLimits {
                context_window: 10000,
                max_output_tokens: 2000,
            },
            pricing: None,
            cache: Default::default(),
            preset: Default::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("custom/helper".into()),
            base_url: "https://custom.invalid/v1/".parse().unwrap(),
            auth: Auth::None,
            default_headers: http::HeaderMap::new(),
            transport: EndpointTransport::Http,
            runtime: Default::default(),
            timeout: Duration::from_secs(1),
        }),
    }
}

fn client() -> AiClient {
    AiClient::try_with_proxy_environment(Default::default()).unwrap()
}

fn request() -> Request {
    Request {
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".into())],
        })],
        ..crate::test_fixtures::base_request()
    }
}

fn response(status: u16, body: reqwest::Body) -> HostHttpResponse {
    http::Response::builder()
        .status(status)
        .header("content-type", "text/event-stream")
        .header("x-custom-response", "yes")
        .body(body)
        .unwrap()
}

fn streaming_body(text: &str) -> reqwest::Body {
    let chunks: Vec<_> = text
        .as_bytes()
        .chunks(7)
        .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)))
        .collect();
    reqwest::Body::wrap_stream(futures_util::stream::iter(chunks))
}

struct Sender {
    requests: Arc<Mutex<Vec<HostHttpRequest>>>,
    response: Mutex<Option<HostHttpResponse>>,
    observations: Observations,
    tracked: AiClient,
}

#[async_trait::async_trait]
impl HostHttpSender for Sender {
    async fn send(&self, request: HostHttpRequest) -> Result<HostHttpResponse, AiError> {
        assert!(self.tracked.request_may_have_been_sent());
        self.observations.lock().unwrap().push("send");
        self.requests.lock().unwrap().push(request);
        Ok(self.response.lock().unwrap().take().unwrap())
    }
}

fn sender(
    tracked: &AiClient,
    response: HostHttpResponse,
    requests: &Arc<Mutex<Vec<HostHttpRequest>>>,
    observations: &Observations,
) -> Arc<dyn HostHttpSender> {
    Arc::new(Sender {
        requests: requests.clone(),
        response: Mutex::new(Some(response)),
        observations: observations.clone(),
        tracked: tracked.clone(),
    })
}

struct Pipeline {
    tracked: AiClient,
    observations: Observations,
}

#[async_trait::async_trait]
impl ProviderRequestHook for Pipeline {
    async fn before_request(
        &self,
        context: &ProviderRequestContext,
        mut payload: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, AiError> {
        assert!(!self.tracked.request_may_have_been_sent());
        assert_eq!(context.model.provider, "custom/helper");
        assert_eq!(payload["model"], "wire-model");
        assert_eq!(payload["messages"][0]["content"], "go");
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["top_p"], 0.75);
        payload["host_hook"] = json!(true);
        self.observations.lock().unwrap().push("payload");
        Ok(Some(payload))
    }

    async fn before_headers(
        &self,
        _context: &ProviderRequestContext,
        headers: &mut http::HeaderMap,
    ) -> Result<(), AiError> {
        assert!(!self.tracked.request_may_have_been_sent());
        headers.insert("x-host-hook", http::HeaderValue::from_static("yes"));
        headers.insert(
            "x-custom-marker",
            http::HeaderValue::from_static("extension-owned-only"),
        );
        self.observations.lock().unwrap().push("headers");
        Ok(())
    }

    async fn after_response(
        &self,
        _context: &ProviderRequestContext,
        status: http::StatusCode,
        headers: &http::HeaderMap,
    ) -> Result<(), AiError> {
        assert_eq!(status, http::StatusCode::OK);
        assert_eq!(headers["x-custom-response"], "yes");
        self.observations.lock().unwrap().push("after");
        Ok(())
    }
}

struct ObserveResponse(Observations);

impl ResponseHook for ObserveResponse {
    fn on_response(
        &self,
        status: http::StatusCode,
        headers: &http::HeaderMap,
        _model: &HookModelContext,
    ) {
        assert_eq!(status, http::StatusCode::OK);
        assert_eq!(headers["x-custom-response"], "yes");
        self.0.lock().unwrap().push("response");
    }
}

#[tokio::test]
async fn custom_http_encodes_hooks_and_accounts_through_native_stream() {
    let tracked = client().track_request_dispatch();
    let observations = Observations::default();
    let requests = Arc::default();
    let pipeline = Arc::new(Pipeline {
        tracked: tracked.clone(),
        observations: observations.clone(),
    });
    let client = tracked.with_provider_request_hooks(vec![pipeline]);
    let mut overrides = RequestOverrides::default();
    overrides
        .sampling_params
        .insert("top_p".into(), json!(0.75));
    let mut stream = client
        .stream_custom_http_with_host_options(
            &model(Protocol::OpenAiChat),
            request(),
            overrides,
            HostRequestOptions {
                on_response: Some(Arc::new(ObserveResponse(observations.clone()))),
                ..Default::default()
            },
            sender(
                &tracked,
                response(200, streaming_body(CHAT_BODY)),
                &requests,
                &observations,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        *observations.lock().unwrap(),
        ["payload", "headers", "send", "response", "after"]
    );
    let mut text = String::new();
    let mut usage_events = 0;
    let mut finished = None;
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            StreamEvent::TextDelta { delta, .. } => text.push_str(&delta),
            StreamEvent::Usage(usage) => {
                usage_events += 1;
                assert_eq!(usage.total_tokens, 22);
            }
            StreamEvent::Finished(response) => finished = Some(response),
            _ => {}
        }
    }
    let finished = finished.unwrap();
    assert_eq!(text, "hello world");
    assert_eq!(finished.stop_reason, StopReason::MaxTokens);
    assert_eq!(finished.usage.input_tokens, 17);
    assert_eq!(finished.usage.output_tokens, 5);
    assert_eq!(usage_events, 1);
    assert_eq!(finished.response_id.as_deref(), Some("custom-response"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, http::Method::POST);
    assert_eq!(
        requests[0].url.as_str(),
        "https://custom.invalid/v1/chat/completions"
    );
    assert_eq!(requests[0].headers["content-type"], "application/json");
    assert_eq!(requests[0].headers["x-host-hook"], "yes");
    assert_eq!(
        requests[0].headers["x-custom-marker"],
        "extension-owned-only"
    );
    assert!(!requests[0].headers.contains_key("authorization"));
    let payload: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(payload["host_hook"], true);
    assert_eq!(payload["messages"][0]["content"], "go");
    assert!(!format!("{:?}", requests[0]).contains("extension-owned-only"));
}

#[tokio::test]
async fn custom_http_never_uses_preferred_websocket_or_persists_on_client() {
    let tracked = client().track_request_dispatch();
    let mut model = model(Protocol::OpenAiResponses);
    Arc::make_mut(&mut model.endpoint).transport = EndpointTransport::WebSocketPreferred;
    let requests = Arc::default();
    let observations = Observations::default();
    for _ in 0..2 {
        let stream = tracked
            .stream_custom_http_with_host_options(
                &model,
                request(),
                RequestOverrides::default(),
                HostRequestOptions::default(),
                sender(
                    &tracked,
                    response(200, streaming_body("")),
                    &requests,
                    &observations,
                ),
            )
            .await
            .unwrap();
        drop(stream);
    }
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request.url.path() == "/v1/responses"));
}

struct NoCredentials;

#[async_trait::async_trait]
impl crate::CredentialResolver for NoCredentials {
    async fn resolve(&self) -> Result<crate::ResolvedCredential, crate::AuthError> {
        panic!("custom sender must never resolve a native credential")
    }
}

#[async_trait::async_trait]
impl crate::RequestSigner for NoCredentials {
    async fn sign(
        &self,
        _request: &crate::SigningRequest,
    ) -> Result<crate::SignedRequestHeaders, crate::AuthError> {
        panic!("custom sender must never invoke a native signer")
    }
}

struct NoTransport;

#[async_trait::async_trait]
impl HostStreamTransport for NoTransport {
    async fn stream(
        &self,
        _model: HostStreamModel,
        _request: Request,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        panic!("custom HTTP sender cannot bypass a registered host route")
    }
}

#[tokio::test]
async fn custom_http_refuses_native_credentials_defaults_and_host_routes_before_dispatch() {
    let auths = [
        Auth::bearer("native-key"),
        Auth::BearerEnv {
            var: "NATIVE_KEY".into(),
        },
        Auth::Header {
            name: http::HeaderName::from_static("x-api-key"),
            value: "native-key".into(),
        },
        Auth::HeaderEnv {
            name: http::HeaderName::from_static("x-api-key"),
            var: "NATIVE_KEY".into(),
        },
        Auth::HeaderBearerEnv {
            name: http::HeaderName::from_static("x-native-key"),
            var: "NATIVE_KEY".into(),
        },
        Auth::Dynamic(Arc::new(NoCredentials)),
        Auth::RequestSigner(Arc::new(NoCredentials)),
    ];
    for case in 0..auths.len() + 5 {
        let tracked = client().track_request_dispatch();
        let mut model = model(Protocol::OpenAiChat);
        let mut options = HostRequestOptions::default();
        if case < auths.len() {
            Arc::make_mut(&mut model.endpoint).auth = auths[case].clone();
        } else {
            match case - auths.len() {
                0 => {
                    Arc::make_mut(&mut model.endpoint)
                        .default_headers
                        .insert("x-native-key", http::HeaderValue::from_static("native-key"));
                }
                1 => {
                    Arc::make_mut(&mut model.spec)
                        .preset
                        .headers
                        .insert("x-native-key".into(), "native-key".into());
                }
                2 => tracked.register_host_stream_transport(
                    model.endpoint.id.clone(),
                    Arc::new(NoTransport),
                ),
                3 => options.fetch = Some(Arc::new(NoTransport)),
                4 => options.api_key = Some("native-key".into()),
                _ => unreachable!(),
            }
        }
        let requests = Arc::default();
        let result = tracked
            .stream_custom_http_with_host_options(
                &model,
                request(),
                RequestOverrides::default(),
                options,
                sender(
                    &tracked,
                    response(200, "".into()),
                    &requests,
                    &Observations::default(),
                ),
            )
            .await;
        assert!(result.is_err(), "refusal case {case}");
        assert!(!tracked.request_may_have_been_sent());
        assert!(requests.lock().unwrap().is_empty());
    }
}

struct RedactionMarker;

impl crate::HeaderTransform for RedactionMarker {
    fn transform_headers(
        &self,
        headers: &mut http::HeaderMap,
        _model: &HookModelContext,
    ) -> Result<(), AiError> {
        headers.insert(
            "x-custom-marker",
            http::HeaderValue::from_static("extension-secret"),
        );
        Ok(())
    }
}

#[tokio::test]
async fn custom_http_preserves_bounded_error_body_and_credential_redaction() {
    let tracked = client().track_request_dispatch();
    let requests = Arc::default();
    let body = format!("extension-secret {}", "x".repeat(8192));
    let error = tracked
        .stream_custom_http_with_host_options(
            &model(Protocol::OpenAiChat),
            request(),
            RequestOverrides::default(),
            HostRequestOptions {
                transform_headers: Some(Arc::new(RedactionMarker)),
                ..Default::default()
            },
            sender(
                &tracked,
                response(429, streaming_body(&body)),
                &requests,
                &Observations::default(),
            ),
        )
        .await
        .err()
        .unwrap();
    let AiError::Http(error) = error else {
        panic!("expected HTTP error")
    };
    assert_eq!(error.status, http::StatusCode::TOO_MANY_REQUESTS);
    let snippet = error.body_snippet.unwrap();
    assert!(snippet.len() <= 4096);
    assert!(!snippet.contains("extension-secret"));
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn custom_http_rejects_malformed_and_oversized_native_sse() {
    for body in [
        "data: {bad-json}\n\n".into(),
        format!("data: {}", "x".repeat(2 * 1024 * 1024 + 1)),
    ] {
        let tracked = client().track_request_dispatch();
        let mut stream = tracked
            .stream_custom_http_with_host_options(
                &model(Protocol::OpenAiChat),
                request(),
                RequestOverrides::default(),
                HostRequestOptions::default(),
                sender(
                    &tracked,
                    response(200, reqwest::Body::from(body)),
                    &Arc::default(),
                    &Observations::default(),
                ),
            )
            .await
            .unwrap();
        let mut failed = false;
        while let Some(event) = stream.next().await {
            match event {
                Err(_) => failed = true,
                Ok(StreamEvent::Finished(_)) => panic!("invalid SSE cannot settle usage"),
                _ => {}
            }
        }
        assert!(failed);
    }
}

struct QuietBody {
    prefix: Option<Bytes>,
    dropped: Arc<AtomicBool>,
    polls: Arc<AtomicUsize>,
}

impl Stream for QuietBody {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        match self.prefix.take() {
            Some(prefix) => Poll::Ready(Some(Ok(prefix))),
            None => Poll::Pending,
        }
    }
}

impl Drop for QuietBody {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn dropping_custom_http_stream_drops_the_owned_quiet_body() {
    let tracked = client().track_request_dispatch();
    let dropped = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let body = reqwest::Body::wrap_stream(QuietBody {
        prefix: Some(Bytes::from_static(
            b"data: {\"id\":\"quiet\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        )),
        dropped: dropped.clone(),
        polls: polls.clone(),
    });
    let mut stream = tracked
        .stream_custom_http_with_host_options(
            &model(Protocol::OpenAiChat),
            request(),
            RequestOverrides::default(),
            HostRequestOptions::default(),
            sender(
                &tracked,
                response(200, body),
                &Arc::default(),
                &Observations::default(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    loop {
        if matches!(
            stream.next().await.unwrap().unwrap(),
            StreamEvent::TextDelta { .. }
        ) {
            break;
        }
    }
    drop(stream);
    assert!(dropped.load(Ordering::SeqCst));
}

fn wire_request() -> HostHttpRequest {
    HostHttpRequest {
        method: http::Method::POST,
        url: "https://custom.invalid/v1/chat/completions"
            .parse()
            .unwrap(),
        headers: http::HeaderMap::new(),
        body: Bytes::from_static(b"{}"),
    }
}

#[tokio::test]
async fn one_shot_sender_cannot_dispatch_twice() {
    let tracked = client().track_request_dispatch();
    tracked.mark_request_dispatch();
    let requests = Arc::default();
    let sender = OneShotHostHttpSender::new(sender(
        &tracked,
        response(200, "".into()),
        &requests,
        &Observations::default(),
    ));
    drop(sender.send(wire_request()).await.unwrap());
    assert!(sender.send(wire_request()).await.is_err());
    assert_eq!(requests.lock().unwrap().len(), 1);
}

struct AddAuthHeader;

#[async_trait::async_trait]
impl ProviderRequestHook for AddAuthHeader {
    async fn before_headers(
        &self,
        _context: &ProviderRequestContext,
        headers: &mut http::HeaderMap,
    ) -> Result<(), AiError> {
        headers.insert("x-api-key", http::HeaderValue::from_static("native-key"));
        Ok(())
    }
}

#[tokio::test]
async fn final_auth_headers_from_overrides_or_hooks_never_reach_callback() {
    for hook in [false, true] {
        let tracked = client().track_request_dispatch();
        let requests = Arc::default();
        let mut overrides = RequestOverrides::default();
        let mut options = HostRequestOptions::default();
        if hook {
            options.provider_hooks.push(Arc::new(AddAuthHeader));
        } else {
            overrides
                .headers
                .insert("authorization".into(), "Bearer native-key".into());
        }
        let result = tracked
            .stream_custom_http_with_host_options(
                &model(Protocol::OpenAiChat),
                request(),
                overrides,
                options,
                sender(
                    &tracked,
                    response(200, "".into()),
                    &requests,
                    &Observations::default(),
                ),
            )
            .await;
        assert!(result.is_err());
        assert!(!tracked.request_may_have_been_sent());
        assert!(requests.lock().unwrap().is_empty());
    }
    for name in [
        "authorization",
        "proxy-authorization",
        "x-api-key",
        "api-key",
        "x-goog-api-key",
        "x-auth-token",
        "cookie",
        "set-cookie",
        "x-amz-security-token",
        "x-amz-date",
    ] {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::HeaderName::from_static(name),
            http::HeaderValue::from_static("native-key"),
        );
        assert!(validate_custom_http_headers(&headers).is_err());
    }
    let mut sensitive = http::HeaderValue::from_static("native-key");
    sensitive.set_sensitive(true);
    let mut headers = http::HeaderMap::new();
    headers.insert("x-private", sensitive);
    assert!(validate_custom_http_headers(&headers).is_err());
}

struct OpeningGuard(Arc<AtomicBool>);

impl Drop for OpeningGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct QuietOpening(Arc<AtomicBool>);

#[async_trait::async_trait]
impl HostHttpSender for QuietOpening {
    async fn send(&self, _request: HostHttpRequest) -> Result<HostHttpResponse, AiError> {
        let _guard = OpeningGuard(self.0.clone());
        futures_util::future::pending().await
    }
}

#[tokio::test]
async fn dropping_custom_opening_cancels_the_inflight_callback() {
    let tracked = client().track_request_dispatch();
    let dropped = Arc::new(AtomicBool::new(false));
    let model = model(Protocol::OpenAiChat);
    let mut opening = Box::pin(tracked.stream_custom_http_with_host_options(
        &model,
        request(),
        RequestOverrides::default(),
        HostRequestOptions::default(),
        Arc::new(QuietOpening(dropped.clone())),
    ));
    assert!(futures_util::poll!(opening.as_mut()).is_pending());
    assert!(tracked.request_may_have_been_sent());
    assert!(!dropped.load(Ordering::SeqCst));
    drop(opening);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn custom_opening_obeys_native_header_timeout_without_retry() {
    let tracked = client().track_request_dispatch();
    let dropped = Arc::new(AtomicBool::new(false));
    let mut model = model(Protocol::OpenAiChat);
    Arc::make_mut(&mut model.endpoint).timeout = Duration::from_millis(20);
    let error = tracked
        .stream_custom_http_with_host_options(
            &model,
            request(),
            RequestOverrides::default(),
            HostRequestOptions::default(),
            Arc::new(QuietOpening(dropped.clone())),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error,
        AiError::Transport(crate::TransportError {
            phase: crate::TransportPhase::ResponseHeaders,
            timeout: true,
            ..
        })
    ));
    assert!(tracked.request_may_have_been_sent());
    assert!(dropped.load(Ordering::SeqCst));
}

#[derive(Clone, Copy)]
enum ContinuationMode {
    Custom,
    MissingSender,
    Native,
    Both,
    ChangedSpec,
    Auth,
    Defaults,
}

struct ContinuationRoute {
    mode: ContinuationMode,
    sender: Arc<dyn HostHttpSender>,
}

struct ContinuationHook;

#[async_trait::async_trait]
impl ProviderRequestHook for ContinuationHook {
    async fn before_request(
        &self,
        _context: &ProviderRequestContext,
        mut payload: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, AiError> {
        let count = payload["continuation_hook"].as_u64().unwrap_or(0);
        payload["continuation_hook"] = json!(count + 1);
        Ok(Some(payload))
    }
}

#[async_trait::async_trait]
impl HostStreamTransport for ContinuationRoute {
    async fn stream(
        &self,
        _model: HostStreamModel,
        _request: Request,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        panic!("continuation routes must use stream_native")
    }

    fn native_model(&self) -> Option<Model> {
        matches!(self.mode, ContinuationMode::Native | ContinuationMode::Both)
            .then(|| model(Protocol::OpenAiChat))
    }

    fn custom_http_model(&self, selected: &Model) -> Option<Model> {
        if matches!(self.mode, ContinuationMode::Native) {
            return None;
        }
        let mut custom = selected.clone();
        let endpoint = Arc::make_mut(&mut custom.endpoint);
        endpoint.id = EndpointId("private-custom-http".into());
        endpoint.base_url = "https://custom.invalid/v1/".parse().unwrap();
        endpoint.auth = Auth::None;
        endpoint.default_headers.clear();
        endpoint.runtime = Default::default();
        Arc::make_mut(&mut custom.spec).endpoint = endpoint.id.clone();
        match self.mode {
            ContinuationMode::ChangedSpec => {
                Arc::make_mut(&mut custom.spec).limits.max_output_tokens += 1
            }
            ContinuationMode::Auth => {
                Arc::make_mut(&mut custom.endpoint).auth = Auth::bearer("native-key")
            }
            ContinuationMode::Defaults => {
                Arc::make_mut(&mut custom.endpoint)
                    .default_headers
                    .insert("x-native-key", http::HeaderValue::from_static("native-key"));
            }
            _ => {}
        }
        Some(custom)
    }

    async fn stream_native(
        &self,
        _model: HostStreamModel,
        request: Request,
        _diagnostics: Vec<Diagnostic>,
        continuation: crate::NativeRequestContinuation,
    ) -> Result<ResponseStream, AiError> {
        if matches!(self.mode, ContinuationMode::MissingSender) {
            continuation.stream(request, Vec::new()).await
        } else {
            continuation
                .stream_custom_http(
                    request,
                    vec![Arc::new(ContinuationHook)],
                    self.sender.clone(),
                    None,
                )
                .await
        }
    }
}

fn selected_host_model() -> Model {
    let mut selected = model(Protocol::OpenAiChat);
    Arc::make_mut(&mut selected.endpoint).id = EndpointId("custom/registration".into());
    Arc::make_mut(&mut selected.spec).endpoint = selected.endpoint.id.clone();
    selected
}

#[tokio::test]
async fn custom_continuation_preserves_run_hooks_options_and_native_accounting() {
    for fetch_override in [false, true] {
        let tracked = client().track_request_dispatch();
        let requests = Arc::default();
        let observations = Observations::default();
        let route = Arc::new(ContinuationRoute {
            mode: ContinuationMode::Custom,
            sender: sender(
                &tracked,
                response(200, streaming_body(CHAT_BODY)),
                &requests,
                &observations,
            ),
        });
        let selected = selected_host_model();
        let mut options = HostRequestOptions {
            on_response: Some(Arc::new(ObserveResponse(observations.clone()))),
            transform_headers: Some(Arc::new(RedactionMarker)),
            provider_hooks: vec![Arc::new(ContinuationHook)],
            ..Default::default()
        };
        if fetch_override {
            options.fetch = Some(route);
        } else {
            tracked.register_host_stream_transport(selected.endpoint.id.clone(), route);
        }
        let mut overrides = RequestOverrides::default();
        overrides
            .sampling_params
            .insert("top_p".into(), json!(0.75));
        overrides.timeout_ms = Some(500);
        let hooked = tracked.with_provider_request_hooks(vec![Arc::new(ContinuationHook)]);
        let mut stream = hooked
            .stream_with_host_options(&selected, request(), overrides, options)
            .await
            .unwrap();
        let mut finished = false;
        while let Some(event) = stream.next().await {
            if let StreamEvent::Finished(response) = event.unwrap() {
                assert_eq!(response.usage.total_tokens, 22);
                assert_eq!(response.stop_reason, StopReason::MaxTokens);
                finished = true;
            }
        }
        assert!(finished);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].headers["x-custom-marker"], "extension-secret");
        let payload: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(payload["top_p"], 0.75);
        assert_eq!(payload["continuation_hook"], 3);
        assert_eq!(*observations.lock().unwrap(), ["send", "response"]);
    }
}

#[tokio::test]
async fn custom_continuation_rejects_wrong_lane_projection_auth_and_private_host_route() {
    let modes = [
        ContinuationMode::MissingSender,
        ContinuationMode::Native,
        ContinuationMode::Both,
        ContinuationMode::ChangedSpec,
        ContinuationMode::Auth,
        ContinuationMode::Defaults,
        ContinuationMode::Custom,
        ContinuationMode::Custom,
    ];
    for (index, mode) in modes.into_iter().enumerate() {
        let tracked = client().track_request_dispatch();
        let requests = Arc::default();
        let selected = selected_host_model();
        let route = Arc::new(ContinuationRoute {
            mode,
            sender: sender(
                &tracked,
                response(200, "".into()),
                &requests,
                &Observations::default(),
            ),
        });
        tracked.register_host_stream_transport(selected.endpoint.id.clone(), route);
        let mut options = HostRequestOptions::default();
        if index == 6 {
            tracked.register_host_stream_transport(
                EndpointId("private-custom-http".into()),
                Arc::new(NoTransport),
            );
        }
        if index == 7 {
            options.api_key = Some("native-key".into());
        }
        assert!(
            tracked
                .stream_with_host_options(
                    &selected,
                    request(),
                    RequestOverrides::default(),
                    options
                )
                .await
                .is_err(),
            "refusal case {index}"
        );
        assert!(requests.lock().unwrap().is_empty());
    }
}

struct ChatProfileRoute {
    native: bool,
    baseline: crate::CustomOpenAiChatProfile,
    override_profile: Option<crate::CustomOpenAiChatProfile>,
    sender: Arc<dyn HostHttpSender>,
}

#[async_trait::async_trait]
impl HostStreamTransport for ChatProfileRoute {
    async fn stream(
        &self,
        _: HostStreamModel,
        _: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        panic!("profile routes must use the actual consuming continuation")
    }

    fn native_model(&self) -> Option<Model> {
        self.native.then(|| model(Protocol::OpenAiChat))
    }

    fn custom_http_model(&self, selected: &Model) -> Option<Model> {
        if self.native {
            return None;
        }
        let mut custom = selected.clone();
        let endpoint = Arc::make_mut(&mut custom.endpoint);
        endpoint.id = EndpointId("private-chat-profile".into());
        endpoint.base_url = "https://custom.invalid/v1/".parse().unwrap();
        endpoint.runtime.openai_chat_profile =
            crate::OpenAiChatRuntimeProfile::PiCustom(self.baseline);
        Arc::make_mut(&mut custom.spec).endpoint = endpoint.id.clone();
        Some(custom)
    }

    async fn stream_native(
        &self,
        _: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
        continuation: crate::NativeRequestContinuation,
    ) -> Result<ResponseStream, AiError> {
        continuation
            .stream_custom_http(
                request,
                Vec::new(),
                self.sender.clone(),
                self.override_profile,
            )
            .await
    }
}

#[tokio::test]
async fn custom_chat_continuation_uses_captured_baseline_or_actual_helper_override() {
    let baseline = crate::CustomOpenAiChatProfile {
        supports_developer_role: false,
        use_max_tokens: true,
        supports_strict_mode: false,
    };
    let alternate = crate::CustomOpenAiChatProfile {
        supports_developer_role: true,
        use_max_tokens: false,
        supports_strict_mode: true,
    };
    for override_profile in [None, Some(alternate)] {
        let tracked = client().track_request_dispatch();
        let requests = Arc::default();
        let mut selected = selected_host_model();
        Arc::make_mut(&mut selected.spec).capabilities.reasoning =
            Some(crate::test_fixtures::reasoning_capability());
        let selected_spec = selected.spec.clone();
        let profile = override_profile.unwrap_or(baseline);
        let route = Arc::new(ChatProfileRoute {
            native: false,
            baseline,
            override_profile,
            sender: sender(
                &tracked,
                response(200, streaming_body(CHAT_BODY)),
                &requests,
                &Observations::default(),
            ),
        });
        tracked.register_host_stream_transport(selected.endpoint.id.clone(), route);
        let request = Request {
            system: Some("actual native profile system".into()),
            max_output_tokens: Some(77),
            session_id: Some("native-chat-profile-session".into()),
            tools: vec![crate::ToolDef {
                name: "native_tool".into(),
                description: "actual native definition".into(),
                parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
                async_execution: false,
                constrained_sampling: Some(crate::ConstrainedSampling::JsonSchema {
                    strict: crate::ConstrainedSamplingStrict::Prefer,
                }),
            }],
            ..request()
        };
        let before = serde_json::to_value(&request).unwrap();
        let mut stream = tracked
            .stream_with_host_options(
                &selected,
                request.clone(),
                RequestOverrides::default(),
                HostRequestOptions::default(),
            )
            .await
            .unwrap();
        let mut finished = 0;
        while let Some(event) = stream.next().await {
            if let StreamEvent::Finished(response) = event.unwrap() {
                assert_eq!(response.usage.total_tokens, 22);
                assert_eq!(response.stop_reason, StopReason::MaxTokens);
                finished += 1;
            }
        }
        assert_eq!(finished, 1);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let payload: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(payload["model"], "wire-model");
        assert_eq!(
            payload["messages"][0]["role"],
            if profile.supports_developer_role {
                "developer"
            } else {
                "system"
            }
        );
        assert_eq!(payload["tool_choice"], "auto");
        let tool = &payload["tools"][0]["function"];
        assert_eq!(tool["name"], "native_tool");
        if profile.supports_strict_mode {
            assert_eq!(tool["strict"], true);
        } else {
            assert!(tool.get("strict").is_none());
        }
        assert_eq!(
            payload[if profile.use_max_tokens {
                "max_tokens"
            } else {
                "max_completion_tokens"
            }],
            77
        );
        assert!(payload
            .get(if profile.use_max_tokens {
                "max_completion_tokens"
            } else {
                "max_tokens"
            })
            .is_none());
        assert_eq!(serde_json::to_value(&request).unwrap(), before);
        assert_eq!(
            serde_json::to_value(&*selected.spec).unwrap(),
            serde_json::to_value(&*selected_spec).unwrap()
        );
    }
}

#[tokio::test]
async fn custom_chat_continuation_refuses_profile_on_native_and_responses_routes_before_send() {
    let profile = crate::CustomOpenAiChatProfile {
        supports_developer_role: false,
        use_max_tokens: true,
        supports_strict_mode: false,
    };
    for native in [false, true] {
        let tracked = client().track_request_dispatch();
        let requests = Arc::default();
        let mut selected = selected_host_model();
        if !native {
            Arc::make_mut(&mut selected.spec).protocol = Protocol::OpenAiResponses;
        }
        let route = Arc::new(ChatProfileRoute {
            native,
            baseline: profile,
            override_profile: Some(profile),
            sender: sender(
                &tracked,
                response(200, "".into()),
                &requests,
                &Observations::default(),
            ),
        });
        tracked.register_host_stream_transport(selected.endpoint.id.clone(), route);
        let result = tracked
            .stream_with_host_options(
                &selected,
                request(),
                RequestOverrides::default(),
                HostRequestOptions::default(),
            )
            .await;
        assert!(matches!(result, Err(AiError::Config(_))));
        assert!(requests.lock().unwrap().is_empty());
    }
}
