#![allow(missing_docs)]

use futures_util::StreamExt;
use octet_ai::{
    AiClient, AiError, Auth, AwsCredentials, AwsSigV4Signer, CacheCompatibility, Capabilities,
    CompatibilityMode, Endpoint, EndpointId, Message, ModalitySet, Model, ModelId, ModelLimits,
    ModelSpec, OutputFormat, OutputModalities, Protocol, Request, StreamEvent, UserMessage,
    UserPart,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture_auth() -> Auth {
    let credentials = AwsCredentials::new(
        "fixture-access-key",
        "fixture-secret-key",
        Some(octet_ai::Secret::from("fixture-session-token")),
    )
    .unwrap();
    let signer = AwsSigV4Signer::new(credentials, "us-east-1", "bedrock")
        .unwrap()
        .with_clock(Arc::new(|| UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
    let signer_debug = format!("{signer:?}");
    assert!(signer_debug.contains("AwsSigV4Signer"));
    assert!(signer_debug.contains("Secret(<redacted>)"));
    assert!(!signer_debug.contains("fixture-access-key"));
    assert!(!signer_debug.contains("fixture-secret-key"));
    assert!(!signer_debug.contains("fixture-session-token"));

    Auth::request_signer(Arc::new(signer))
}

fn bedrock_model(base_url: &str, api_name: &str) -> Model {
    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("bedrock-fixture-model".to_owned()),
        endpoint: EndpointId("bedrock-fixture-endpoint".to_owned()),
        api_name: api_name.to_owned(),
        display_name: None,
        protocol: Protocol::BedrockConverse,
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
        },
        limits: ModelLimits {
            context_window: 200_000,
            max_output_tokens: 8_192,
        },
        pricing: None,
        cache: CacheCompatibility::default(),
    };
    let endpoint = Endpoint {
        id: EndpointId("bedrock-fixture-endpoint".to_owned()),
        base_url: url::Url::parse(base_url).unwrap(),
        auth: fixture_auth(),
        default_headers: http::HeaderMap::new(),
        transport: octet_ai::EndpointTransport::Http,
        runtime: octet_ai::RequestRuntime::default(),
        timeout: Duration::from_secs(2),
    };
    Model {
        spec: Arc::new(spec),
        endpoint: Arc::new(endpoint),
    }
}

fn text_request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("fixture request".to_owned())],
        })],
        tools: Vec::new(),
        tool_choice: octet_ai::ToolChoice::Auto,
        max_output_tokens: Some(64),
        temperature: None,
        stop: Vec::new(),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: octet_ai::CacheRetention::None,
        session_id: None,
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn frame(headers: &[(&str, &str)], payload: Value) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(name.len() as u8);
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7);
        header_bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        header_bytes.extend_from_slice(value.as_bytes());
    }
    let payload = serde_json::to_vec(&payload).unwrap();
    let total = 16 + header_bytes.len() + payload.len();
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(&(total as u32).to_be_bytes());
    bytes.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&crc32(&bytes).to_be_bytes());
    bytes.extend_from_slice(&header_bytes);
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(&crc32(&bytes).to_be_bytes());
    bytes
}

fn normal_stream() -> Vec<u8> {
    [
        frame(
            &[(":message-type", "event"), (":event-type", "messageStart")],
            json!({"role": "assistant"}),
        ),
        frame(
            &[
                (":message-type", "event"),
                (":event-type", "contentBlockDelta"),
            ],
            json!({"contentBlockIndex": 0, "delta": {"text": "hello"}}),
        ),
        frame(
            &[
                (":message-type", "event"),
                (":event-type", "contentBlockStop"),
            ],
            json!({"contentBlockIndex": 0}),
        ),
        frame(
            &[(":message-type", "event"), (":event-type", "messageStop")],
            json!({"stopReason": "end_turn"}),
        ),
        frame(
            &[(":message-type", "event"), (":event-type", "metadata")],
            json!({
                "usage": {
                    "inputTokens": 10,
                    "cacheReadInputTokens": 4,
                    "cacheWriteInputTokens": 2,
                    "outputTokens": 5,
                    "totalTokens": 17
                }
            }),
        ),
    ]
    .concat()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn converse_stream_fixture_signs_exact_wire_body_and_preserves_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(normal_stream()),
        )
        .mount(&server)
        .await;

    let model = bedrock_model(
        &format!("{}/", server.uri()),
        "anthropic.claude-3-7-sonnet-20250219-v1:0",
    );
    let mut stream = AiClient::new()
        .stream(&model, text_request())
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }

    let response = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Finished(response) => Some(response),
            _ => None,
        })
        .expect("Bedrock fixture must finish");
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::TextDelta { delta, .. } if delta == "hello"
    )));
    assert_eq!(response.usage.input_tokens, 10);
    assert_eq!(response.usage.cache_read_tokens, 4);
    assert_eq!(response.usage.cache_write_tokens, 2);
    assert_eq!(response.usage.output_tokens, 5);
    assert_eq!(response.usage.total_tokens, 17);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.url.path(),
        "/model/anthropic.claude-3-7-sonnet-20250219-v1%3A0/converse-stream"
    );
    let expected_body = serde_json::to_vec(&json!({
        "messages": [{
            "role": "user",
            "content": [{"text": "fixture request"}]
        }],
        "inferenceConfig": {"maxTokens": 64}
    }))
    .unwrap();
    assert_eq!(request.body, expected_body);
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["text"], "fixture request");
    assert_eq!(body["inferenceConfig"]["maxTokens"], 64);
    assert_eq!(
        request.headers["x-amz-content-sha256"].to_str().unwrap(),
        sha256_hex(&expected_body)
    );
    let authorization = request.headers[http::header::AUTHORIZATION]
        .to_str()
        .unwrap();
    assert!(authorization.contains("Credential=fixture-access-key/"));
    assert!(authorization.contains("/us-east-1/bedrock/aws4_request"));
    // Wiremock reconstructs received HeaderValues from serialized bytes, so
    // it cannot preserve the process-local sensitivity bit checked in auth.rs.
    assert_eq!(
        request.headers["x-amz-security-token"].to_str().unwrap(),
        "fixture-session-token"
    );
    assert_eq!(
        request.headers["accept"].to_str().unwrap(),
        "application/vnd.amazon.eventstream"
    );
}

#[tokio::test]
async fn converse_stream_without_message_stop_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(frame(
                    &[(":message-type", "event"), (":event-type", "messageStart")],
                    json!({"role": "assistant"}),
                )),
        )
        .mount(&server)
        .await;

    let model = bedrock_model(&format!("{}/", server.uri()), "fixture-model");
    let mut stream = AiClient::new()
        .stream(&model, text_request())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::Started { .. }))
    ));
    let error = stream
        .next()
        .await
        .expect("missing messageStop must be surfaced")
        .unwrap_err();
    let AiError::StreamFailure { inner, .. } = error else {
        panic!("expected annotated Bedrock stream failure");
    };
    assert!(matches!(inner.as_ref(), AiError::Decode(_)));
}

#[tokio::test]
async fn converse_stream_exception_is_structured_without_live_aws() {
    let server = MockServer::start().await;
    let body = frame(
        &[
            (":message-type", "exception"),
            (":exception-type", "ThrottlingException"),
        ],
        json!({"message": "fixture throttled"}),
    );
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(body),
        )
        .mount(&server)
        .await;

    let model = bedrock_model(&format!("{}/", server.uri()), "fixture-model");
    let mut stream = AiClient::new()
        .stream(&model, text_request())
        .await
        .unwrap();
    let error = stream
        .next()
        .await
        .expect("exception must be surfaced")
        .unwrap_err();
    let AiError::StreamFailure { inner, progress } = error else {
        panic!("expected annotated Bedrock stream failure");
    };
    assert!(matches!(
        inner.as_ref(),
        AiError::Provider(provider)
            if provider.code.as_deref() == Some("ThrottlingException")
                && provider.kind.as_deref() == Some("bedrock_event_stream")
                && provider.message == "fixture throttled"
    ));
    assert_eq!(progress.provider_events, 1);
    assert!(progress.first_body_seen);
}

#[tokio::test]
async fn converse_stream_crc_failure_is_terminal() {
    let server = MockServer::start().await;
    let mut body = normal_stream();
    let last = body.len() - 1;
    body[last] ^= 0xff;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(body),
        )
        .mount(&server)
        .await;

    let model = bedrock_model(&format!("{}/", server.uri()), "fixture-model");
    let mut stream = AiClient::new()
        .stream(&model, text_request())
        .await
        .unwrap();
    let mut failure = None;
    while let Some(event) = stream.next().await {
        match event {
            Ok(_) => {}
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let Some(AiError::StreamFailure { inner, .. }) = failure else {
        panic!("expected CRC failure");
    };
    assert!(matches!(inner.as_ref(), AiError::Decode(_)));
}

#[tokio::test]
async fn dropping_bedrock_stream_closes_the_response_without_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let closed = Arc::new(AtomicBool::new(false));
    let closed_for_server = closed.clone();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        let event = frame(
            &[(":message-type", "event"), (":event-type", "messageStart")],
            json!({"role": "assistant"}),
        );
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.amazon.eventstream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            event.len()
        );
        if socket.write_all(headers.as_bytes()).await.is_err()
            || socket.write_all(&event).await.is_err()
            || socket.write_all(b"\r\n").await.is_err()
        {
            return;
        }
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => {
                    closed_for_server.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(_) => {}
            }
        }
    });

    let model = bedrock_model(&format!("http://{address}/"), "fixture-model");
    let mut stream = AiClient::new()
        .stream(&model, text_request())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::Started { .. }))
    ));
    drop(stream);

    for _ in 0..20 {
        if closed.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        closed.load(Ordering::SeqCst),
        "dropping a Bedrock response stream must close its HTTP body"
    );
}
