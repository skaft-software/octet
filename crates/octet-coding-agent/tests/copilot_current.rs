#![allow(missing_docs)]

//! Deterministic GitHub Copilot host, lifecycle, and protocol fixtures.
//!
//! These tests use sentinel values only. They never invoke GitHub OAuth, read a
//! user credential, or persist a Copilot token.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use octet_ai::{
    AiClient, AiError, AuthError, CacheRetention, Capabilities, CompatibilityMode, Message,
    ModalitySet, ModelCatalog, ModelLimits, OutputFormat, OutputModalities, Protocol,
    ReasoningConfig, ReasoningMode, Request, StreamEvent, ToolChoice, UserMessage, UserPart,
};
use octet_sdk::provider::{
    builtin_provider_definitions, CopilotAvailabilityError, CopilotCredentialScheme,
    CopilotDeviceLogin, CopilotDeviceLoginStatus, CopilotDynamicHeader, CopilotEndpoint,
    CopilotHost, CopilotModel, CopilotProvider, CopilotSession, ProviderAccess,
    ProviderCatalogKind,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Fixture sentinels, not live credentials. The first two match the checked-in
// error response fixture so the client redaction boundary is exercised.
const PRIMARY_TOKEN: &str = "copilot-primary-token";
const DYNAMIC_HEADER: &str = "copilot-dynamic-header-token";
const REFRESHED_PRIMARY_TOKEN: &str = "copilot-refreshed-token";
const REFRESHED_DYNAMIC_HEADER: &str = "copilot-refreshed-header-token";

struct FixtureHost {
    availability: Option<CopilotAvailabilityError>,
    device_login: CopilotDeviceLogin,
    poll_statuses: Mutex<VecDeque<CopilotDeviceLoginStatus>>,
    exchanges: Mutex<VecDeque<Result<CopilotSession, CopilotAvailabilityError>>>,
    refreshes: Mutex<VecDeque<Result<CopilotSession, CopilotAvailabilityError>>>,
    discovery: Result<Vec<CopilotModel>, CopilotAvailabilityError>,
    exchange_calls: AtomicUsize,
    refresh_calls: AtomicUsize,
}

fn fixture_host(
    availability: Option<CopilotAvailabilityError>,
    exchanges: Vec<Result<CopilotSession, CopilotAvailabilityError>>,
    refreshes: Vec<Result<CopilotSession, CopilotAvailabilityError>>,
    discovery: Result<Vec<CopilotModel>, CopilotAvailabilityError>,
) -> Arc<FixtureHost> {
    Arc::new(FixtureHost {
        availability,
        device_login: CopilotDeviceLogin::new(
            url::Url::parse("https://github.example.test/login/device").unwrap(),
            "DEVICE-CODE-123",
            Duration::from_secs(600),
            Duration::from_secs(5),
        )
        .unwrap(),
        poll_statuses: Mutex::new(VecDeque::from([
            CopilotDeviceLoginStatus::Pending,
            CopilotDeviceLoginStatus::Authorized,
        ])),
        exchanges: Mutex::new(exchanges.into()),
        refreshes: Mutex::new(refreshes.into()),
        discovery,
        exchange_calls: AtomicUsize::new(0),
        refresh_calls: AtomicUsize::new(0),
    })
}

#[async_trait::async_trait]
impl CopilotHost for FixtureHost {
    async fn availability(&self) -> Result<(), CopilotAvailabilityError> {
        match self.availability {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn begin_device_login(&self) -> Result<CopilotDeviceLogin, CopilotAvailabilityError> {
        match self.availability {
            Some(error) => Err(error),
            None => Ok(self.device_login.clone()),
        }
    }

    async fn poll_device_login(
        &self,
    ) -> Result<CopilotDeviceLoginStatus, CopilotAvailabilityError> {
        match self.availability {
            Some(error) => Err(error),
            None => Ok(self
                .poll_statuses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(CopilotDeviceLoginStatus::Authorized)),
        }
    }

    async fn exchange(&self) -> Result<CopilotSession, CopilotAvailabilityError> {
        self.exchange_calls.fetch_add(1, Ordering::SeqCst);
        self.exchanges
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(CopilotAvailabilityError::TokenExchangeUnavailable))
    }

    async fn refresh(&self) -> Result<CopilotSession, CopilotAvailabilityError> {
        self.refresh_calls.fetch_add(1, Ordering::SeqCst);
        self.refreshes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(CopilotAvailabilityError::TokenRefreshUnavailable))
    }

    async fn discover_models(&self) -> Result<Vec<CopilotModel>, CopilotAvailabilityError> {
        self.discovery.clone()
    }
}

fn fixture_session(primary: &str, dynamic: &str, lifetime: Duration) -> CopilotSession {
    let dynamic_header = CopilotDynamicHeader::new(
        http::HeaderName::from_static("x-copilot-session"),
        http::HeaderValue::from_bytes(dynamic.as_bytes()).unwrap(),
    )
    .unwrap();
    CopilotSession::new(
        primary,
        CopilotCredentialScheme::Bearer,
        vec![dynamic_header],
        lifetime,
    )
    .unwrap()
}

fn fixture_model(id: &str, protocol: Protocol) -> CopilotModel {
    CopilotModel::new(
        id,
        protocol,
        Capabilities {
            responses_features: Default::default(),
            input_modalities: ModalitySet::none(),
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: true,
            reasoning: None,
            responses_lite: false,
            agent_delegation: None,
            structured_output: protocol == Protocol::OpenAiResponses,
            deferred_tool_loading: false,
        },
        ModelLimits {
            context_window: 128_000,
            max_output_tokens: 16_384,
        },
    )
}

fn provider_for(host: Arc<FixtureHost>, endpoint: &str) -> CopilotProvider {
    CopilotProvider::new(
        host,
        CopilotEndpoint::new(url::Url::parse(endpoint).unwrap()).unwrap(),
    )
    .unwrap()
}

fn text_request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("fixture prompt".to_owned())],
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
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: None,
    }
}

async fn collect_events(model: &octet_ai::Model) -> Vec<StreamEvent> {
    let mut stream = AiClient::new()
        .stream(model, text_request())
        .await
        .expect("Copilot stream fixture should open");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("Copilot stream fixture should decode"));
    }
    events
}

fn usage_total(events: &[StreamEvent]) -> u64 {
    events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Usage(usage) => Some(usage.total_tokens),
            _ => None,
        })
        .expect("stream fixture should report usage")
}

#[tokio::test]
async fn device_fixture_stays_host_owned_and_out_of_standalone_catalogs() {
    let host = fixture_host(
        None,
        vec![Ok(fixture_session(
            PRIMARY_TOKEN,
            DYNAMIC_HEADER,
            Duration::from_secs(600),
        ))],
        vec![],
        Ok(vec![fixture_model("device-model", Protocol::OpenAiChat)]),
    );
    let provider = provider_for(Arc::clone(&host), "https://api.example.test/");

    let login = provider
        .begin_device_login()
        .await
        .expect("host should provide the device display payload");
    assert_eq!(login.user_code(), "DEVICE-CODE-123");
    assert_eq!(
        login.verification_uri().as_str(),
        "https://github.example.test/login/device"
    );
    assert_eq!(
        provider.poll_device_login().await.unwrap(),
        CopilotDeviceLoginStatus::Pending
    );
    assert_eq!(
        provider.poll_device_login().await.unwrap(),
        CopilotDeviceLoginStatus::Authorized
    );
    provider.exchange().await.unwrap();
    assert_eq!(host.exchange_calls.load(Ordering::SeqCst), 1);

    assert!(matches!(
        provider.definition().authentication(),
        ProviderAccess::HostOwned { integration } if integration == "github-copilot"
    ));
    assert_eq!(
        provider.definition().catalog(),
        ProviderCatalogKind::Subscription
    );
    assert!(!builtin_provider_definitions()
        .iter()
        .any(|definition| definition.id() == "github-copilot"));

    let diagnostic = format!("{provider:?}{login:?}");
    assert!(!diagnostic.contains(PRIMARY_TOKEN));
    assert!(!diagnostic.contains(DYNAMIC_HEADER));
    assert!(!format!("{login:?}").contains("DEVICE-CODE-123"));
    assert!(!format!("{login:?}").contains("github.example.test"));
}

#[tokio::test]
async fn unavailable_discovery_and_unsupported_protocol_fixtures_fail_closed() {
    let unavailable = provider_for(
        fixture_host(
            Some(CopilotAvailabilityError::LoginRequired),
            vec![],
            vec![],
            Ok(vec![fixture_model("unavailable", Protocol::OpenAiChat)]),
        ),
        "https://api.example.test/",
    );
    let mut catalog = ModelCatalog::default();
    assert_eq!(
        unavailable.register_models(&mut catalog).await.unwrap_err(),
        CopilotAvailabilityError::LoginRequired
    );
    assert_eq!(catalog.models().count(), 0);
    assert!(!catalog.has_endpoint(&octet_ai::EndpointId(
        "github-copilot-chat".to_owned()
    )));

    let discovery_error = provider_for(
        fixture_host(
            None,
            vec![Ok(fixture_session(
                PRIMARY_TOKEN,
                DYNAMIC_HEADER,
                Duration::from_secs(600),
            ))],
            vec![],
            Err(CopilotAvailabilityError::ModelDiscoveryUnavailable),
        ),
        "https://api.example.test/",
    );
    let mut catalog = ModelCatalog::default();
    assert_eq!(
        discovery_error
            .register_models(&mut catalog)
            .await
            .unwrap_err(),
        CopilotAvailabilityError::ModelDiscoveryUnavailable
    );
    assert_eq!(catalog.models().count(), 0);
    assert!(!catalog.has_endpoint(&octet_ai::EndpointId(
        "github-copilot-chat".to_owned()
    )));

    let unsupported = provider_for(
        fixture_host(
            None,
            vec![Ok(fixture_session(
                PRIMARY_TOKEN,
                DYNAMIC_HEADER,
                Duration::from_secs(600),
            ))],
            vec![],
            Ok(vec![fixture_model(
                "claude-looking-model",
                Protocol::AnthropicMessages,
            )]),
        ),
        "https://api.example.test/",
    );
    let mut catalog = ModelCatalog::default();
    assert_eq!(
        unsupported.register_models(&mut catalog).await.unwrap_err(),
        CopilotAvailabilityError::UnsupportedModelProtocol
    );
    assert_eq!(catalog.models().count(), 0);
    assert!(!catalog.has_endpoint(&octet_ai::EndpointId(
        "github-copilot-chat".to_owned()
    )));
}

#[tokio::test]
async fn protocol_metadata_fixture_selects_routes_without_model_name_heuristics() {
    let host = fixture_host(
        None,
        vec![Ok(fixture_session(
            PRIMARY_TOKEN,
            DYNAMIC_HEADER,
            Duration::from_secs(600),
        ))],
        vec![],
        Ok(vec![
            fixture_model("gpt-looking-chat", Protocol::OpenAiChat).with_display_name("Chat"),
            fixture_model("claude-looking-responses", Protocol::OpenAiResponses)
                .with_display_name("Responses"),
        ]),
    );
    let provider = provider_for(host, "https://api.example.test/");
    let mut catalog = ModelCatalog::default();
    provider.register_models(&mut catalog).await.unwrap();

    let chat = catalog
        .resolve(&octet_ai::ModelId(
            "github-copilot/gpt-looking-chat".to_owned(),
        ))
        .unwrap();
    assert_eq!(chat.spec.protocol, Protocol::OpenAiChat);
    assert_eq!(chat.endpoint.id.0, "github-copilot-chat");

    let responses = catalog
        .resolve(&octet_ai::ModelId(
            "github-copilot/claude-looking-responses".to_owned(),
        ))
        .unwrap();
    assert_eq!(responses.spec.protocol, Protocol::OpenAiResponses);
    assert_eq!(responses.endpoint.id.0, "github-copilot-responses");
}

#[tokio::test]
async fn chat_and_responses_stream_fixtures_preserve_route_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("chat/completions"))
        .and(header("authorization", format!("Bearer {PRIMARY_TOKEN}")))
        .and(header("x-copilot-session", DYNAMIC_HEADER))
        .and(body_string_contains("chat-fixture"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(include_str!(
                    "../fixtures/providers/github-copilot/chat_stream.sse"
                )),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .and(header("authorization", format!("Bearer {PRIMARY_TOKEN}")))
        .and(header("x-copilot-session", DYNAMIC_HEADER))
        .and(body_string_contains("responses-fixture"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(include_str!(
                    "../fixtures/providers/github-copilot/responses_stream.sse"
                )),
        )
        .mount(&server)
        .await;

    let host = fixture_host(
        None,
        vec![Ok(fixture_session(
            PRIMARY_TOKEN,
            DYNAMIC_HEADER,
            Duration::from_secs(600),
        ))],
        vec![],
        Ok(vec![
            fixture_model("chat-fixture", Protocol::OpenAiChat),
            fixture_model("responses-fixture", Protocol::OpenAiResponses),
        ]),
    );
    let provider = provider_for(host, &server.uri());
    let mut catalog = ModelCatalog::default();
    provider.register_models(&mut catalog).await.unwrap();

    let chat = catalog
        .resolve(&octet_ai::ModelId(
            "github-copilot/chat-fixture".to_owned(),
        ))
        .unwrap();
    let chat_events = collect_events(&chat).await;
    assert!(chat_events.iter().any(
        |event| matches!(event, StreamEvent::TextDelta { delta, .. } if delta == "hello")
    ));
    assert_eq!(usage_total(&chat_events), 10);
    assert!(matches!(chat_events.last(), Some(StreamEvent::Finished(_))));

    let responses = catalog
        .resolve(&octet_ai::ModelId(
            "github-copilot/responses-fixture".to_owned(),
        ))
        .unwrap();
    let response_events = collect_events(&responses).await;
    assert!(response_events.iter().any(|event| matches!(
        event,
        StreamEvent::TextDelta { delta, .. } if delta == "hello from Responses"
    )));
    assert_eq!(usage_total(&response_events), 12);
    assert!(matches!(
        response_events.last(),
        Some(StreamEvent::Finished(_))
    ));
}

#[tokio::test]
async fn refresh_fixture_replaces_stale_primary_and_dynamic_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("chat/completions"))
        .and(header(
            "authorization",
            format!("Bearer {REFRESHED_PRIMARY_TOKEN}"),
        ))
        .and(header("x-copilot-session", REFRESHED_DYNAMIC_HEADER))
        .and(body_string_contains("refresh-fixture"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(include_str!(
                    "../fixtures/providers/github-copilot/chat_stream.sse"
                )),
        )
        .mount(&server)
        .await;

    let host = fixture_host(
        None,
        vec![Ok(fixture_session(
            PRIMARY_TOKEN,
            DYNAMIC_HEADER,
            Duration::from_secs(1),
        ))],
        vec![Ok(fixture_session(
            REFRESHED_PRIMARY_TOKEN,
            REFRESHED_DYNAMIC_HEADER,
            Duration::from_secs(600),
        ))],
        Ok(vec![fixture_model("refresh-fixture", Protocol::OpenAiChat)]),
    );
    let provider = provider_for(Arc::clone(&host), &server.uri());
    let mut catalog = ModelCatalog::default();
    provider.register_models(&mut catalog).await.unwrap();
    let model = catalog
        .resolve(&octet_ai::ModelId(
            "github-copilot/refresh-fixture".to_owned(),
        ))
        .unwrap();

    let events = collect_events(&model).await;
    assert_eq!(usage_total(&events), 10);
    assert_eq!(host.exchange_calls.load(Ordering::SeqCst), 1);
    assert_eq!(host.refresh_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refresh_error_fixture_discards_the_stale_session() {
    let host = fixture_host(
        None,
        vec![Ok(fixture_session(
            PRIMARY_TOKEN,
            DYNAMIC_HEADER,
            Duration::from_secs(1),
        ))],
        vec![Err(CopilotAvailabilityError::TokenRefreshUnavailable)],
        Ok(vec![fixture_model(
            "refresh-failure-fixture",
            Protocol::OpenAiChat,
        )]),
    );
    let provider = provider_for(Arc::clone(&host), "https://api.example.test/");
    let mut catalog = ModelCatalog::default();
    provider.register_models(&mut catalog).await.unwrap();
    let model = catalog
        .resolve(&octet_ai::ModelId(
            "github-copilot/refresh-failure-fixture".to_owned(),
        ))
        .unwrap();

    let first = match AiClient::new().stream(&model, text_request()).await {
        Ok(_) => panic!("refresh failure fixture should reject the request"),
        Err(error) => error,
    };
    assert!(matches!(first, AiError::Auth(AuthError::Resolve)));

    // The failed refresh removes the expired session. The next request must try
    // a fresh exchange, not retry a stale token through the refresh seam.
    let second = match AiClient::new().stream(&model, text_request()).await {
        Ok(_) => panic!("exchange fallback fixture should reject the request"),
        Err(error) => error,
    };
    assert!(matches!(second, AiError::Auth(AuthError::Resolve)));
    assert_eq!(host.refresh_calls.load(Ordering::SeqCst), 1);
    assert_eq!(host.exchange_calls.load(Ordering::SeqCst), 2);
    let diagnostics = format!("{first:?}{second:?}");
    assert!(!diagnostics.contains(PRIMARY_TOKEN));
    assert!(!diagnostics.contains(DYNAMIC_HEADER));
}

#[tokio::test]
async fn error_fixture_redacts_primary_and_dynamic_credentials_everywhere() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("chat/completions"))
        .and(header("authorization", format!("Bearer {PRIMARY_TOKEN}")))
        .and(header("x-copilot-session", DYNAMIC_HEADER))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("content-type", "application/json")
                .set_body_string(include_str!(
                    "../fixtures/providers/github-copilot/error.json"
                )),
        )
        .mount(&server)
        .await;

    let session = fixture_session(PRIMARY_TOKEN, DYNAMIC_HEADER, Duration::from_secs(600));
    let host = fixture_host(
        None,
        vec![Ok(session.clone())],
        vec![],
        Ok(vec![fixture_model("error-fixture", Protocol::OpenAiChat)]),
    );
    let provider = provider_for(host, &server.uri());
    let mut catalog = ModelCatalog::default();
    provider.register_models(&mut catalog).await.unwrap();
    let model = catalog
        .resolve(&octet_ai::ModelId(
            "github-copilot/error-fixture".to_owned(),
        ))
        .unwrap();

    let error = match AiClient::new().stream(&model, text_request()).await {
        Ok(_) => panic!("error fixture should reject the request"),
        Err(error) => error,
    };
    let error_diagnostics = format!("{error:?} {error}");
    assert!(error_diagnostics.contains("[REDACTED]"));
    assert!(!error_diagnostics.contains(PRIMARY_TOKEN));
    assert!(!error_diagnostics.contains(DYNAMIC_HEADER));

    let model_debug = format!("{model:?}{:?}", model.endpoint);
    let catalog_metadata = serde_json::to_string(model.spec.as_ref()).unwrap();
    let all_debug = format!("{provider:?}{session:?}{model_debug}{catalog_metadata}");
    assert!(!all_debug.contains(PRIMARY_TOKEN));
    assert!(!all_debug.contains(DYNAMIC_HEADER));
    assert!(model.endpoint.auth.is_configured());
}

#[tokio::test]
async fn cancel_fixture_closes_an_inflight_http_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let disconnected = Arc::new(AtomicBool::new(false));
    let observed_disconnect = Arc::clone(&disconnected);
    let mut server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(index) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
        }

        let event = "data: {\"id\":\"copilot-cancel\",\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: keep-alive\r\n\r\n{:x}\r\n{}\r\n",
            event.len(),
            event
        );
        if socket.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        let mut tail = [0_u8; 128];
        match socket.read(&mut tail).await {
            Ok(0) | Err(_) => observed_disconnect.store(true, Ordering::SeqCst),
            Ok(_) => {}
        }
    });

    let host = fixture_host(
        None,
        vec![Ok(fixture_session(
            PRIMARY_TOKEN,
            DYNAMIC_HEADER,
            Duration::from_secs(600),
        ))],
        vec![],
        Ok(vec![fixture_model("cancel-fixture", Protocol::OpenAiChat)]),
    );
    let provider = provider_for(host, &format!("http://{address}/"));
    let mut catalog = ModelCatalog::default();
    provider.register_models(&mut catalog).await.unwrap();
    let model = catalog
        .resolve(&octet_ai::ModelId(
            "github-copilot/cancel-fixture".to_owned(),
        ))
        .unwrap();

    let mut stream = AiClient::new()
        .stream(&model, text_request())
        .await
        .expect("cancel fixture should open");
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::Started { .. }))
    ));
    drop(stream);

    if tokio::time::timeout(Duration::from_secs(2), &mut server)
        .await
        .is_err()
    {
        server.abort();
    }
    assert!(
        disconnected.load(Ordering::SeqCst),
        "dropping a Copilot stream must close the transport fixture"
    );
}
