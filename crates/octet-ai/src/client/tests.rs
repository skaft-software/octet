//! Unit tests for `crate::client`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `client.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::client`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::diagnostics::{
    lifecycle_from_sse_comment, parse_provider_lifecycle, sanitize_diagnostic,
    truncate_transport_message, MAX_PROVIDER_DIAGNOSTIC_BYTES, MAX_PROVIDER_LIFECYCLE_DETAIL_BYTES,
};
use super::transport::{prepare_request_body, transient_connection_source};
use super::*;
use crate::stream::ProviderLifecycleState;

#[tokio::test]
async fn dropping_a_resumed_receiver_closes_a_quiet_http_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            assert_eq!(socket.read(&mut byte).await.unwrap(), 1);
            request.push(byte[0]);
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        // Only keepalive data: no decoded event will ever attempt send().
        socket.write_all(b"d\r\n: keepalive\n\n\r\n").await.unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
            .await
            .expect("cancelled body reader must release the connection")
            .unwrap();
        assert_eq!(closed, 0);
    });
    let resumer = ResponsesResume {
        http: reqwest::Client::new(),
        endpoint: format!("http://{address}/responses").parse().unwrap(),
        headers: http::HeaderMap::new(),
    };
    let receiver = resumer.open("resp_1", 1).await.unwrap();
    drop(receiver);
    server.await.unwrap();
}

#[tokio::test]
async fn client_generated_websocket_errors_fence_pool_before_publication() {
    let catalog = crate::catalog::ModelCatalog::builtin().unwrap();
    let id = catalog
        .models()
        .find(|model| model.protocol == Protocol::OpenAiResponses)
        .unwrap()
        .id
        .clone();
    let model = catalog.resolve(&id).unwrap();
    for deadline in [Duration::ZERO, Duration::from_secs(5)] {
        let pool = ResponsesWsPool::default();
        let (sender, receiver) = crate::responses_ws::event_channel(1);
        sender
            .send(Ok(serde_json::json!({
                "type": "error", "code": "invalid_request_error", "message": "invalid"
            })))
            .await
            .unwrap();
        // No actor exists to observe receiver closure or perform cleanup.
        // Both a client deadline and decoded provider error must fence the
        // session themselves before exposing failure to the consumer.
        let mut stream = responses_websocket_stream(
            pool.clone(),
            Some("poisoned".into()),
            model.clone(),
            None,
            receiver,
            Vec::new(),
            Vec::new(),
            false,
            CredentialRedactor::default(),
            Duration::from_secs(5),
            Duration::from_secs(5),
            deadline,
        );
        loop {
            match stream.next().await {
                Some(Err(_)) => break,
                Some(Ok(_)) => {}
                None => panic!("expected client failure"),
            }
        }
        let error = pool
            .request(
                Some("poisoned"),
                url::Url::parse("ws://127.0.0.1:9/").unwrap(),
                http::HeaderMap::new(),
                serde_json::json!({}),
                ResponsesWsLiveness::for_response_idle(Duration::from_secs(5)),
                Duration::from_secs(5),
                Some(DEFAULT_CONNECT_TIMEOUT),
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("disabled after an earlier failure"));
        drop(sender);
    }
}

#[tokio::test]
async fn declared_request_runtime_compresses_without_provider_identity() {
    let original = bytes::Bytes::from(vec![b'a'; 128 * 1024]);
    let mut headers = http::HeaderMap::new();
    let compressed = prepare_request_body(
        crate::types::RequestRuntime {
            body_encoding: crate::types::RequestBodyEncoding::Zstd,
            ..crate::types::RequestRuntime::default()
        },
        &mut headers,
        original.clone(),
    )
    .await;
    assert_eq!(headers[http::header::CONTENT_ENCODING], "zstd");
    assert!(compressed.len() < original.len() / 10);
    assert_eq!(
        zstd::stream::decode_all(compressed.as_ref()).unwrap(),
        original.as_ref()
    );

    let mut generic_headers = http::HeaderMap::new();
    let generic = prepare_request_body(
        crate::types::RequestRuntime::default(),
        &mut generic_headers,
        original.clone(),
    )
    .await;
    assert_eq!(generic, original);
    assert!(generic_headers
        .get(http::header::CONTENT_ENCODING)
        .is_none());
}

#[tokio::test]
async fn transport_diagnostic_keeps_cause_but_removes_request_url() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let secret = "must-not-appear";
    let url = format!("http://{address}/private/catalog?token={secret}");
    let error = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .expect_err("the released listener must refuse the connection");
    let AiError::NetworkUnavailable(error) = request_open_transport_error(error, "request") else {
        unreachable!()
    };
    assert_eq!(error.phase, TransportPhase::Connect);
    assert!(error.message.starts_with("request connection failed:"));
    assert!(error.message.contains("refused") || error.message.contains("connect"));
    assert!(!error.message.contains(secret));
    assert!(!error.message.contains("/private/catalog"));
    assert!(!error.message.contains(&address.to_string()));
}

#[tokio::test]
async fn stalled_tls_connect_timeout_is_network_unavailable_before_post() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        // No TLS acknowledgement, so HTTP POST cannot have been sent.
        let _ = release_rx.await;
    });
    let error = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_millis(50))
        .build()
        .unwrap()
        .post(format!("https://{address}/responses"))
        .body("not accepted")
        .send()
        .await
        .unwrap_err();
    let error = request_open_transport_error(error, "request");
    assert!(
        matches!(error, AiError::NetworkUnavailable(ref transport)
        if transport.phase == TransportPhase::Connect && transport.timeout),
        "{error:?}"
    );
    drop(release);
    server.await.unwrap();
}

#[tokio::test]
async fn invalid_tls_handshake_is_not_network_unavailable() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut hello = [0_u8; 4096];
        assert!(socket.read(&mut hello).await.unwrap() > 0);
        socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
    });
    let error = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("https://{address}/responses"))
        .send()
        .await
        .unwrap_err();
    assert!(error.is_connect());
    let error = request_open_transport_error(error, "request");
    assert!(
        matches!(error, AiError::Transport(ref transport)
        if transport.phase == TransportPhase::Connect && !transport.timeout),
        "{error:?}"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn disconnect_after_post_is_not_network_unavailable() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let count = socket.read(&mut request).await.unwrap();
        assert!(count > 0, "POST reached the server before disconnect");
    });
    let error = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{address}/responses"))
        .body("may already be accepted")
        .send()
        .await
        .unwrap_err();
    assert!(!error.is_connect());
    assert!(matches!(
        request_open_transport_error(error, "request"),
        AiError::Transport(TransportError {
            phase: TransportPhase::ResponseHeaders,
            ..
        })
    ));
    server.await.unwrap();
}

#[test]
fn invalid_certificate_configuration_is_not_network_unavailable() {
    let certificate = reqwest::Certificate::from_der(b"invalid certificate").unwrap();
    let error = reqwest::Client::builder()
        .add_root_certificate(certificate)
        .build()
        .unwrap_err();
    assert!(!matches!(
        request_open_transport_error(error, "request"),
        AiError::NetworkUnavailable(_)
    ));
}

#[tokio::test]
async fn dns_failure_requires_typed_transient_evidence() {
    struct FailedDns(std::io::ErrorKind);
    impl reqwest::dns::Resolve for FailedDns {
        fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            let kind = self.0;
            Box::pin(async move {
                Err(Box::new(std::io::Error::new(kind, "connection refused"))
                    as Box<dyn std::error::Error + Send + Sync>)
            })
        }
    }
    for (kind, transient) in [
        (std::io::ErrorKind::TimedOut, true),
        (std::io::ErrorKind::NetworkUnreachable, true),
        (std::io::ErrorKind::NotFound, false),
        (std::io::ErrorKind::Other, false),
    ] {
        let error = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(FailedDns(kind)))
            .build()
            .unwrap()
            .post("http://offline.invalid/responses")
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect());
        assert_eq!(
            matches!(
                request_open_transport_error(error, "request"),
                AiError::NetworkUnavailable(_)
            ),
            transient,
            "{kind:?}"
        );
    }
}

#[test]
fn connection_source_classification_uses_io_kinds_not_messages() {
    for kind in [
        std::io::ErrorKind::ConnectionRefused,
        std::io::ErrorKind::ConnectionReset,
        std::io::ErrorKind::ConnectionAborted,
        std::io::ErrorKind::NetworkDown,
        std::io::ErrorKind::NetworkUnreachable,
        std::io::ErrorKind::HostUnreachable,
        std::io::ErrorKind::TimedOut,
    ] {
        assert!(transient_connection_source(&std::io::Error::new(
            kind,
            "invalid certificate"
        )));
    }
    for kind in [
        std::io::ErrorKind::InvalidData,
        std::io::ErrorKind::InvalidInput,
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::Other,
    ] {
        assert!(!transient_connection_source(&std::io::Error::new(
            kind,
            "connection refused; timed out"
        )));
    }
}

#[test]
fn lifecycle_details_are_redacted_control_safe_and_bounded() {
    let mut headers = http::HeaderMap::new();
    headers.insert("authorization", "Bearer lifecycle-secret".parse().unwrap());
    let mut redactor = CredentialRedactor::default();
    redactor.include_header_values(&headers);
    let detail = format!("Bearer lifecycle-secret \x1b{}", "é".repeat(200));

    let lifecycle = parse_provider_lifecycle(&format!("loading; {detail}"), &redactor)
        .expect("known lifecycle state");
    assert_eq!(lifecycle.state, ProviderLifecycleState::Loading);
    let detail = lifecycle.detail.expect("nonempty detail");
    assert!(detail.len() <= MAX_PROVIDER_LIFECYCLE_DETAIL_BYTES);
    assert!(detail.is_char_boundary(detail.len()));
    assert!(detail.contains("[REDACTED]"));
    assert!(!detail.contains("lifecycle-secret"));
    assert!(!detail.chars().any(char::is_control));
    assert!(lifecycle_from_sse_comment("ordinary keepalive", &redactor).is_none());
    assert!(parse_provider_lifecycle("unknown; ignored", &redactor).is_none());
}

#[test]
fn provider_diagnostics_are_control_safe_and_post_sanitize_bounded() {
    let input = format!("\x1b\x07\u{202e}{}", "é".repeat(3_000));
    let output = sanitize_diagnostic(
        &CredentialRedactor::default(),
        &input,
        MAX_PROVIDER_DIAGNOSTIC_BYTES,
    );
    assert!(output.len() <= MAX_PROVIDER_DIAGNOSTIC_BYTES);
    assert!(output.is_char_boundary(output.len()));
    assert!(output.ends_with('…'));
    assert!(!output.chars().any(char::is_control));
    assert!(!output.contains('\u{202e}'));
    assert!(output.contains(r"\u{1b}"));
    assert!(output.contains(r"\u{7}"));
    assert!(output.contains(r"\u{202e}"));
}

#[test]
fn transport_diagnostic_truncation_preserves_utf8_boundaries() {
    let mut message = format!("{}étail", "a".repeat(511));
    truncate_transport_message(&mut message, 512);
    assert_eq!(message.len(), 511);
    assert!(message.chars().all(|character| character == 'a'));
}

#[test]
fn structured_http_error_sanitization_is_bounded_valid_json() {
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "error": {
            "message": "\"\\\u{1b}\n界".repeat(1000),
            "code": "\\\"".repeat(1000),
            "type": "\\\"".repeat(1000),
        },
        "request": {"prompt": "private prompt"},
    }))
    .unwrap();
    let error = sanitize_ai_error(
        &CredentialRedactor::default(),
        AiError::Http(HttpError {
            status: http::StatusCode::BAD_REQUEST,
            request_id: None,
            retry_after: None,
            provider_code: None,
            body_snippet: Some(body),
            retryable: false,
        }),
    );
    let AiError::Http(http) = error else {
        unreachable!()
    };
    let body = http.body_snippet.unwrap();
    assert!(body.len() <= 4096, "{}", body.len());
    assert!(serde_json::from_str::<serde_json::Value>(&body).is_ok());
    assert!(!body.contains("private prompt"));
    assert!(!body.contains('\x1b'));
}

async fn assert_google_files_validation_precedes_credentials(strict_schema: bool) {
    let client = blocked_gemini_test_client();
    let model = gemini_files_test_model();
    let mut request = crate::test_fixtures::base_request();
    request.messages = vec![crate::Message::User(crate::UserMessage {
        content: vec![crate::UserPart::Media(crate::Media::audio_bytes(
            bytes::Bytes::from(vec![0; 12_000_000]),
            crate::AudioFormat::Wav,
        ))],
    })];
    if strict_schema {
        request.tools = vec![crate::ToolDef {
            name: "choice".into(),
            description: String::new(),
            async_execution: false,
            parameters: serde_json::json!({"type": "object", "properties": {
                "value": {"oneOf": [{"type": "string"}, {"type": "number"}]}
            }}),
            constrained_sampling: Some(crate::ConstrainedSampling::JsonSchema {
                strict: crate::ConstrainedSamplingStrict::Require,
            }),
        }];
    } else {
        request.temperature = Some(f32::NAN);
    }
    for api_key in [None, Some(crate::Secret::from("fake-key"))] {
        let result = client
            .stream_once(
                &model,
                request.clone(),
                &Default::default(),
                &HostRequestOptions {
                    api_key,
                    ..Default::default()
                },
            )
            .await;
        let error = result.err().expect("local admission must reject");
        if strict_schema {
            assert!(
                matches!(
                    error,
                    AiError::Unsupported(crate::UnsupportedError::ConstrainedSampling(_))
                ),
                "{error:?}"
            );
        } else {
            assert!(
                matches!(
                    error,
                    AiError::Validation(crate::ValidationError::InvalidTemperature)
                ),
                "{error:?}"
            );
        }
    }
}

#[tokio::test]
async fn google_files_invalid_temperature_precedes_credentials() {
    assert_google_files_validation_precedes_credentials(false).await;
}

#[tokio::test]
async fn google_files_strict_schema_precedes_credentials() {
    assert_google_files_validation_precedes_credentials(true).await;
}

#[test]
fn http_diagnostics_classify_but_never_quote_short_or_transformed_echoes() {
    for (message, expected) in [
        (
            "maximum context length is 128000 tokens; BOB!",
            "exceeds the context window",
        ),
        ("rejected: BOB!", "Check the model and request settings"),
    ] {
        let error = sanitize_ai_error(
            &CredentialRedactor::default(),
            AiError::Http(HttpError {
                status: http::StatusCode::BAD_REQUEST,
                request_id: None,
                retry_after: None,
                provider_code: Some("Bob".into()),
                body_snippet: Some(
                    serde_json::json!({"error": {"detail": message, "code": "Bob", "type": "boB"}})
                        .to_string(),
                ),
                retryable: false,
            }),
        );
        let AiError::Http(http) = error else {
            unreachable!()
        };
        assert!(http.provider_code.is_none());
        let body = http.body_snippet.unwrap();
        assert!(body.contains(expected));
        assert!(!body.to_ascii_lowercase().contains("bob"));
        assert!(!body.contains("128000"));
    }
}

#[tokio::test]
async fn google_files_proxy_preflight_precedes_credentials_and_upload() {
    let direct = wiremock::MockServer::start().await;
    let client = AiClient::try_with_proxy_environment(std::collections::BTreeMap::from([(
        "HTTP_PROXY".into(),
        "socks5://127.0.0.1:1080".into(),
    )]))
    .unwrap();
    let mut model = crate::ModelCatalog::builtin()
        .unwrap()
        .resolve(&crate::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).protocol = Protocol::GoogleGenerativeAi;
    Arc::make_mut(&mut model.spec).capabilities.input_modalities =
        crate::ModalitySet::none().with(crate::Modality::Audio);
    let upload = url::Url::parse(&format!("{}/upload/v1beta/files", direct.uri())).unwrap();
    let error = client
        .validate_google_upload_proxy(&model, &upload)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("audio file preparation") && error.contains("proxy"),
        "{error}"
    );
    assert!(direct.received_requests().await.unwrap().is_empty());
}

fn gemini_files_test_model() -> Model {
    let mut model = crate::ModelCatalog::builtin()
        .unwrap()
        .resolve(&crate::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let endpoint = Arc::make_mut(&mut model.endpoint);
    endpoint.id.0 = "gemini".into();
    endpoint.base_url = "https://generativelanguage.googleapis.com/v1beta/"
        .parse()
        .unwrap();
    endpoint.auth = crate::auth::Auth::HeaderEnv {
        name: http::HeaderName::from_static("x-goog-api-key"),
        var: "OCTET_TEST_GEMINI_FILES_KEY".into(),
    };
    let spec = Arc::make_mut(&mut model.spec);
    spec.endpoint = model.endpoint.id.clone();
    spec.protocol = Protocol::GoogleGenerativeAi;
    spec.api_name = "gemini-test".into();
    spec.capabilities.input_modalities = crate::ModalitySet::none().with(crate::Modality::Audio);
    model
}

fn blocked_gemini_test_client() -> AiClient {
    // Block egress even if request admission regresses.
    AiClient::try_with_proxy_environment(std::collections::BTreeMap::from([(
        "HTTPS_PROXY".into(),
        "socks5://127.0.0.1:1".into(),
    )]))
    .unwrap()
}

#[tokio::test]
async fn gemini_files_rejects_an_oversized_inference_body_before_any_upload() {
    let mut request = crate::test_fixtures::base_request();
    // Bounded JSON encoding counts all escaping in the final inference body.
    request.system = Some("\0".repeat(3_400_000));
    request.messages = vec![crate::Message::User(crate::UserMessage {
        content: vec![crate::UserPart::Media(crate::Media::audio_bytes(
            bytes::Bytes::from(vec![42; 12_000_000]),
            crate::AudioFormat::Wav,
        ))],
    })];
    let result = blocked_gemini_test_client()
        .stream_once(
            &gemini_files_test_model(),
            request,
            &Default::default(),
            &HostRequestOptions::default(),
        )
        .await;
    assert!(matches!(
        result,
        Err(AiError::Validation(crate::ValidationError::RequestBytes {
            limit: crate::recorded_audio::GOOGLE_INLINE_BYTES,
            ..
        }))
    ));
}

#[tokio::test]
async fn gemini_payload_veto_precedes_files_and_credentials() {
    struct Refuse;
    impl crate::PayloadHook for Refuse {
        fn on_payload(
            &self,
            body: serde_json::Value,
            _: &crate::HookModelContext,
        ) -> Result<Option<serde_json::Value>, AiError> {
            assert!(body["contents"][0]["parts"][0].get("inlineData").is_some());
            Err(crate::ConfigError::Parse("test payload veto".into()).into())
        }
    }
    let model = gemini_files_test_model();
    let mut request = crate::test_fixtures::base_request();
    request.messages = vec![crate::Message::User(crate::UserMessage {
        content: vec![crate::UserPart::Media(crate::Media::audio_bytes(
            bytes::Bytes::from(vec![42; 12_000_000]),
            crate::AudioFormat::Wav,
        ))],
    })];
    // A deliberately unusable proxy is an independent guard against network
    // access. The old order hit this proxy check before the payload veto.
    let client = blocked_gemini_test_client();
    let result = client
        .stream_once(
            &model,
            request,
            &Default::default(),
            &HostRequestOptions {
                on_payload: Some(Arc::new(Refuse)),
                ..Default::default()
            },
        )
        .await;
    assert!(
        matches!(result, Err(AiError::Config(crate::ConfigError::Parse(message))) if message == "test payload veto")
    );
}

#[tokio::test]
async fn google_files_rejects_an_oversized_clip_before_uploading_earlier_clips() {
    let model = gemini_files_test_model();
    let clip = |len| {
        crate::UserPart::Media(crate::Media::audio_bytes(
            bytes::Bytes::from(vec![42; len]),
            crate::AudioFormat::Wav,
        ))
    };
    let mut request = crate::test_fixtures::base_request();
    request.messages = vec![crate::Message::User(crate::UserMessage {
        content: vec![clip(12_000_000), clip(21 * 1024 * 1024)],
    })];
    let error = blocked_gemini_test_client()
        .stream_once(
            &model,
            request,
            &Default::default(),
            &HostRequestOptions::default(),
        )
        .await
        .err()
        .expect("oversized later clip must refuse before files or credentials")
        .to_string();
    assert!(error.contains("20 MiB"), "{error}");
}

#[test]
fn http_slow_down_with_rate_ramp_preserves_only_retry_evidence() {
    for (kind, retry) in [("slow_down", true), ("insufficient_quota", false)] {
        let error = sanitize_ai_error(&CredentialRedactor::default(), AiError::Http(HttpError {
            status: http::StatusCode::TOO_MANY_REQUESTS, request_id: None, retry_after: None, provider_code: Some("slow_down".into()), retryable: true,
            body_snippet: Some(serde_json::json!({"error":{"type":kind,"message":"Your request rate increased too quickly (private details)"}}).to_string()),
        }));
        let AiError::Http(http) = error else {
            unreachable!()
        };
        assert_eq!(http.is_safe_to_retry(), retry);
        assert!(!http.to_string().contains("private details"));
    }
}
