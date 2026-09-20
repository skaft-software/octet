#![allow(missing_docs)]

//! Proxy environment consumption by the real HTTP transport (ledger 1b.3).
//!
//! The resolver's own unit tests live with its declaration; these tests prove
//! that the actual `AiClient` HTTP request path sends an absolute-form request
//! to a configured proxy, honors `NO_PROXY` root/subdomain exclusion, and fails
//! closed on a malformed proxy value before dispatch.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use octet_ai::{
    AiClient, AiError, Auth, Capabilities, CompatibilityMode::Strict, Endpoint, EndpointId,
    Message, ModalitySet, Model, ModelId, ModelLimits, ModelSpec, OutputFormat, OutputModalities,
    Protocol, ReasoningConfig, ReasoningMode, Request, RequestOverrides, ToolChoice, UserMessage,
    UserPart,
};

const SSE_BODY: &str = "data: {\"id\": \"chatcmpl-proxy\", \"choices\": [{\"delta\": {\"content\": \"proxied\"}}]}\n\n\
                        data: {\"id\": \"chatcmpl-proxy\", \"choices\": [{\"delta\": {}, \"finish_reason\": \"stop\"}]}\n\n\
                        data: [DONE]\n\n";

fn test_model(base_url: &str) -> Model {
    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("proxy-model".to_string()),
        endpoint: EndpointId("proxy-ep".to_string()),
        api_name: "gpt-4-test".to_string(),
        display_name: None,
        protocol: Protocol::OpenAiChat,
        capabilities: Capabilities {
            input_modalities: ModalitySet::none(),
            output_modalities: ModalitySet::none(),
            tools: false,
            parallel_tool_calls: false,
            reasoning: None,
            responses_lite: false,
            agent_delegation: None,
            structured_output: false,
            deferred_tool_loading: false,
        },
        limits: ModelLimits {
            context_window: 10000,
            max_output_tokens: 2000,
        },
        pricing: None,
        cache: octet_ai::CacheCompatibility::default(),
    };
    let endpoint = Endpoint {
        id: EndpointId("proxy-ep".to_string()),
        base_url: url::Url::parse(base_url).unwrap(),
        auth: Auth::bearer("proxy-key"),
        default_headers: http::HeaderMap::new(),
        transport: octet_ai::EndpointTransport::Http,
        runtime: octet_ai::RequestRuntime::default(),
        timeout: Duration::from_secs(5),
    };
    Model {
        spec: Arc::new(spec),
        endpoint: Arc::new(endpoint),
    }
}

fn request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".to_string())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: Strict,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    }
}

fn websocket_preferred_responses_model(base_url: &str) -> Model {
    let mut model = test_model(base_url);
    let mut spec = (*model.spec).clone();
    spec.protocol = Protocol::OpenAiResponses;
    model.spec = Arc::new(spec);
    let mut endpoint = (*model.endpoint).clone();
    endpoint.transport = octet_ai::EndpointTransport::WebSocketPreferred;
    model.endpoint = Arc::new(endpoint);
    model
}

fn proxy_overrides(proxy: &str, no_proxy: Option<&str>) -> RequestOverrides {
    let mut env = BTreeMap::from([("HTTP_PROXY".to_owned(), proxy.to_owned())]);
    if let Some(no_proxy) = no_proxy {
        env.insert("NO_PROXY".to_owned(), no_proxy.to_owned());
    }
    RequestOverrides {
        env,
        ..Default::default()
    }
}

/// Minimal recording HTTP proxy: captures the request head and answers with a
/// fixed SSE stream. No TLS, no forwarding, no network beyond loopback.
async fn spawn_recording_proxy() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let sink = sink.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match socket.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(read) => {
                            buffer.extend_from_slice(&chunk[..read]);
                            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                            if buffer.len() > 64 * 1024 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                sink.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buffer).to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    SSE_BODY.len(),
                    SSE_BODY
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (addr, captured)
}

#[tokio::test]
async fn http_transport_sends_absolute_form_requests_through_the_proxy() {
    let (proxy_addr, captured) = spawn_recording_proxy().await;
    let model = test_model("http://provider.invalid/");
    let client = AiClient::new();
    let response = client
        .complete_with_overrides(
            &model,
            request(),
            proxy_overrides(&format!("http://{proxy_addr}"), None),
        )
        .await
        .unwrap();
    assert_eq!(response.response_id.as_deref(), Some("chatcmpl-proxy"));

    let heads = captured.lock().unwrap().clone();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].starts_with("POST http://provider.invalid/chat/completions HTTP/1.1\r\n"),
        "absolute-form request line expected, got: {}",
        heads[0].lines().next().unwrap_or_default()
    );
}

#[tokio::test]
async fn no_proxy_excludes_loopback_and_root_domains_without_proxying() {
    let (proxy_addr, captured) = spawn_recording_proxy().await;
    let proxy = format!("http://{proxy_addr}");

    // Loopback target excluded: the request reaches the loopback server.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SSE_BODY)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;
    let model = test_model(&format!("{}/", server.uri()));
    let client = AiClient::new();
    client
        .complete_with_overrides(
            &model,
            request(),
            proxy_overrides(&proxy, Some("127.0.0.1")),
        )
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // A `.domain` entry excludes the domain root and every subdomain. The
    // target does not exist, so the request must fail without ever reaching the
    // proxy.
    let model = test_model("http://api.proxy-holder.invalid/");
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        client.complete_with_overrides(
            &model,
            request(),
            proxy_overrides(&proxy, Some(".proxy-holder.invalid")),
        ),
    )
    .await
    .expect("direct DNS failure must not hang");
    assert!(error.is_err());
    assert!(captured.lock().unwrap().is_empty());
}

#[tokio::test]
async fn all_proxy_fallback_and_lowercase_names_reach_the_transport() {
    let (proxy_addr, captured) = spawn_recording_proxy().await;
    let model = test_model("http://provider.invalid/");
    let client = AiClient::new();
    let env = BTreeMap::from([("all_proxy".to_owned(), format!("http://{proxy_addr}"))]);
    let response = client
        .complete_with_overrides(
            &model,
            request(),
            RequestOverrides {
                env,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(response.response_id.as_deref(), Some("chatcmpl-proxy"));
    assert_eq!(captured.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn proxied_websocket_preferred_route_uses_the_proxy_over_http() {
    let (proxy_addr, captured) = spawn_recording_proxy().await;
    let model = websocket_preferred_responses_model("http://provider.invalid/");
    let client = AiClient::new();
    // The WebSocket handshake is deliberately not attempted while a proxy is
    // selected; the request must reach the proxy over HTTP instead.
    let stream = client
        .stream_with_overrides(
            &model,
            request(),
            proxy_overrides(&format!("http://{proxy_addr}"), None),
        )
        .await
        .expect("proxy-selected transport must open over HTTP");
    drop(stream);
    let heads = captured.lock().unwrap().clone();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].starts_with("POST http://provider.invalid/responses HTTP/1.1\r\n"),
        "expected the proxied Responses POST, got: {}",
        heads[0].lines().next().unwrap_or_default()
    );
}

#[tokio::test]
async fn malformed_proxy_environment_fails_closed_before_dispatch() {
    let (_proxy_addr, captured) = spawn_recording_proxy().await;
    let model = test_model("http://provider.invalid/");
    let client = AiClient::new();
    for malformed in [
        "socks5://127.0.0.1:1080".to_owned(),
        "not a url at all".to_owned(),
    ] {
        let error = client
            .complete_with_overrides(&model, request(), proxy_overrides(&malformed, None))
            .await
            .expect_err("malformed proxy must fail closed");
        assert!(matches!(error, AiError::Config(_)));
    }
    assert!(captured.lock().unwrap().is_empty());

    // A valid proxy that is not listening still fails at connect, never falls
    // back to direct egress.
    let dead = proxy_overrides("http://127.0.0.1:1", None);
    assert!(client
        .complete_with_overrides(&model, request(), dead)
        .await
        .is_err());
    assert!(captured.lock().unwrap().is_empty());
}
