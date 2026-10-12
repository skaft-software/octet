#![allow(missing_docs)]

//! Loopback coverage for the redacted provider-request trace
//! (`OCTET_PROVIDER_REQUEST_TRACE`): a Codex-style server rejects a long
//! computer-use history with an opaque HTTP 400, and the trace must describe
//! the request shape and the rejection without carrying any request content.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use octet_ai::{
    AiClient, AssistantMessage, AssistantPart, Auth, Capabilities, CompatibilityMode::Strict,
    Endpoint, EndpointId, Media, Message, Modality, ModalitySet, Model, ModelId, ModelLimits,
    ModelSpec, OutputFormat, OutputModalities, Protocol, Request, RequestOverrides, ToolCall,
    ToolCallId, ToolResult, ToolResultPart, UserMessage, UserPart,
};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn responses_model(base_url: &str) -> Model {
    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("test-model".to_string()),
        endpoint: EndpointId("test-ep".to_string()),
        api_name: "gpt-6-luna".to_string(),
        display_name: None,
        protocol: Protocol::OpenAiResponses,
        capabilities: Capabilities {
            responses_features: Default::default(),
            input_modalities: ModalitySet::none().with(Modality::Image),
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
            max_output_tokens: 8_000,
        },
        pricing: None,
        cache: octet_ai::CacheCompatibility::default(),
    };
    let endpoint = Endpoint {
        id: EndpointId("test-ep".to_string()),
        base_url: url::Url::parse(base_url).unwrap(),
        auth: Auth::bearer("loopback-api-key"),
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

fn screenshot_turns(count: usize) -> Vec<Message> {
    let mut messages = vec![Message::User(UserMessage {
        content: vec![UserPart::Text("SECRET operator instruction".to_string())],
    })];
    for index in 0..count {
        let call_id = format!("call_{index}");
        messages.push(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: ToolCallId(call_id.clone()),
                name: "window".to_string(),
                arguments_json: "{\"action\":\"SECRET\"}".to_string(),
                argument_error: None,
            })],
            model: ModelId("test-model".to_string()),
            protocol: Protocol::OpenAiResponses,
        }));
        messages.push(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: ToolCallId(call_id),
                content: vec![
                    ToolResultPart::Text("SECRET window state".to_string()),
                    ToolResultPart::Media(Media::image_bytes(
                        bytes::Bytes::from(vec![0x89u8; 3_000]),
                        "image/png".parse().unwrap(),
                    )),
                ],
                is_error: false,
                added_tool_names: None,
            })],
        }));
    }
    messages
}

fn request(messages: Vec<Message>) -> Request {
    Request {
        system: None,
        messages,
        tools: vec![],
        tool_choice: octet_ai::ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: Strict,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    }
}

#[tokio::test]
async fn trace_describes_a_rejected_screenshot_history_without_its_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("{\"detail\":\"Bad Request\"}")
                .insert_header("x-request-id", "req_loopback_400"),
        )
        .mount(&server)
        .await;

    let trace_path =
        std::env::temp_dir().join(format!("octet-loopback-trace-{}.jsonl", std::process::id()));
    let overrides = RequestOverrides {
        env: BTreeMap::from([(
            "OCTET_PROVIDER_REQUEST_TRACE".to_string(),
            trace_path.to_string_lossy().into_owned(),
        )]),
        ..Default::default()
    };

    let model = responses_model(&server.uri());
    let outcome = AiClient::new()
        .stream_with_overrides(&model, request(screenshot_turns(3)), overrides)
        .await;
    assert!(outcome.is_err(), "the loopback server rejects the request");

    let written = std::fs::read_to_string(&trace_path).unwrap();
    std::fs::remove_file(&trace_path).unwrap();
    for secret in [
        "SECRET",
        "loopback-api-key",
        "iVBOR",
        "data:image",
        "base64",
    ] {
        assert!(!written.contains(secret), "{secret} leaked: {written}");
    }
    let lines: Vec<Value> = written
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{written}");

    let request = &lines[0];
    assert_eq!(request["event"], "request");
    assert_eq!(request["transport"], "http");
    assert_eq!(request["model"], "gpt-6-luna");
    assert_eq!(request["previous_response_id"], false);
    assert_eq!(request["item_types"]["function_call"], 3);
    assert_eq!(request["item_types"]["function_call_output"], 3);
    assert_eq!(request["images"]["count"], 3);
    assert_eq!(request["images"]["inline"], 3);
    // 3,000 raw bytes become 4,000 base64 characters plus the data-URL prefix.
    let prefix = "data:image/png;base64,".len() as u64;
    assert_eq!(
        request["images"]["largest_inline_wire_bytes"],
        4_000 + prefix
    );
    assert_eq!(request["reasoning"]["items"], 0);
    assert!(request["body_bytes"].as_u64().unwrap() > 12_000);

    let rejection = &lines[1];
    assert_eq!(rejection["event"], "response_error");
    assert_eq!(rejection["status"], 400);
    assert_eq!(rejection["provider_request_id"], "req_loopback_400");
    assert_eq!(rejection["id"], request["id"]);
}
