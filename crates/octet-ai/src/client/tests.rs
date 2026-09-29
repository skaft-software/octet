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
