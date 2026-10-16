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

struct ChangeSystem(std::sync::atomic::AtomicUsize);

impl octet_ai::PayloadHook for ChangeSystem {
    fn on_payload(
        &self,
        mut payload: Value,
        _: &octet_ai::HookModelContext,
    ) -> Result<Option<Value>, octet_ai::AiError> {
        let turn = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        payload["messages"][0]["content"] =
            serde_json::json!(format!("SECRET volatile hook {turn}"));
        Ok(Some(payload))
    }
}

#[tokio::test]
async fn trace_locates_hook_mutations_and_correlates_terminal_cache_usage() {
    use futures_util::StreamExt as _;
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)
        .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":1,\"cached_tokens\":98}}\n\ndata: [DONE]\n\n")
        .insert_header("content-type", "text/event-stream")).mount(&server).await;
    let mut model = responses_model(&server.uri());
    Arc::make_mut(&mut model.spec).protocol = Protocol::OpenAiChat;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let trace_path = std::env::temp_dir().join(format!(
        "octet-cache-hook-trace-{}-{nonce}.jsonl",
        std::process::id()
    ));
    let overrides = RequestOverrides {
        env: BTreeMap::from([(
            "OCTET_PROVIDER_REQUEST_TRACE".into(),
            trace_path.to_string_lossy().into_owned(),
        )]),
        ..Default::default()
    };
    let hook = Arc::new(ChangeSystem(std::sync::atomic::AtomicUsize::new(0)));
    let client = AiClient::new();
    for _ in 0..2 {
        let mut req = request(vec![Message::User(UserMessage {
            content: vec![UserPart::Text("SECRET stable user".into())],
        })]);
        req.system = Some("SECRET stable system".into());
        req.session_id = Some("SECRET session identity".into());
        let mut stream = client
            .clone()
            .stream_with_host_options(
                &model,
                req,
                overrides.clone(),
                octet_ai::HostRequestOptions {
                    on_payload: Some(hook.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let written = std::fs::read_to_string(&trace_path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&trace_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::remove_file(trace_path).unwrap();
    assert!(!written.contains("SECRET"));
    assert!(!written.contains("loopback-api-key"));
    assert!(!written.contains(&server.uri()));
    let lines: Vec<Value> = written
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let stage = |name: &str| {
        lines
            .iter()
            .filter(|line| line["event"] == "cache_prefix" && line["stage"] == name)
            .collect::<Vec<_>>()
    };
    for name in ["canonical", "encoded"] {
        let stages = stage(name);
        assert_eq!(stages[1]["raw_common_prefix_bytes"], stages[0]["bytes"]);
    }
    let stages = stage("wire_http");
    assert_ne!(stages[0]["digest"], stages[1]["digest"]);
    assert_eq!(stages[1]["first_changed_segment"], "/messages/0");
    assert_eq!(stages[1]["previous_id"], stages[0]["id"]);
    let requests = server.received_requests().await.unwrap();
    let actual = requests[0]
        .body
        .iter()
        .zip(&requests[1].body)
        .position(|(a, b)| a != b)
        .unwrap();
    assert_eq!(stages[1]["raw_common_prefix_bytes"], actual);
    let usage: Vec<_> = lines
        .iter()
        .filter(|line| line["event"] == "cache_usage")
        .collect();
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[1]["id"], stages[1]["id"]);
    assert_eq!(usage[1]["total_input_tokens"], 100);
    assert_eq!(usage[1]["fresh_input_tokens"], 2);
    assert_eq!(usage[1]["cache_efficiency"], 0.98);
}

struct MoveResponsesBreakpoint(std::sync::atomic::AtomicUsize);

impl octet_ai::PayloadHook for MoveResponsesBreakpoint {
    fn on_payload(
        &self,
        mut payload: Value,
        _: &octet_ai::HookModelContext,
    ) -> Result<Option<Value>, octet_ai::AiError> {
        let turn = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let block = if turn == 0 {
            &mut payload["input"][0]["content"][0]
        } else {
            &mut payload["input"][2]["output"][0]
        };
        block["prompt_cache_breakpoint"] = serde_json::json!({"mode":"explicit"});
        Ok(Some(payload))
    }
}

#[tokio::test]
async fn responses_flex_trace_distinguishes_breakpoint_hooks_from_history_changes() {
    use futures_util::StreamExt as _;
    let server = MockServer::start().await;
    // Synthetic usage exercises normalization, not actual provider cache hits.
    Mock::given(method("POST")).and(path("responses"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string("data: {\"type\":\"response.created\",\"response\":{\"id\":\"response\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"response\",\"service_tier\":\"flex\",\"output\":[],\"usage\":{\"input_tokens\":100,\"input_tokens_details\":{\"cached_tokens\":98,\"cache_write_tokens\":1},\"output_tokens\":5,\"output_tokens_details\":{\"reasoning_tokens\":2}}}}\n\n")
            .insert_header("content-type", "text/event-stream"))
        .expect(2).mount(&server).await;
    let mut model = responses_model(&server.uri());
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Codex;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let trace_path = std::env::temp_dir().join(format!(
        "octet-responses-flex-trace-{}-{nonce}.jsonl",
        std::process::id()
    ));
    let overrides = RequestOverrides {
        env: BTreeMap::from([(
            "OCTET_PROVIDER_REQUEST_TRACE".into(),
            trace_path.to_string_lossy().into_owned(),
        )]),
        ..Default::default()
    };
    let input: octet_ai::ResponsesInput = serde_json::from_value(serde_json::json!([
        {"type":"message","role":"user","content":[{"type":"input_text","text":"SECRET prompt"}]},
        {"type":"function_call","call_id":"SECRET call","name":"read","arguments":"{\"path\":\"SECRET path\"}"},
        {"type":"function_call_output","call_id":"SECRET call","output":[{"type":"input_text","text":"SECRET result"}]}
    ])).unwrap();
    let mut req = request(vec![Message::User(UserMessage {
        content: vec![UserPart::Text("SECRET prompt".into())],
    })]);
    req.responses = Some(
        octet_ai::ResponsesOptions::full_replay(input)
            .with_service_tier(octet_ai::ServiceTier::Flex),
    );
    req.session_id = Some("SECRET stable session".into());
    let client = AiClient::new();
    let hook = Arc::new(MoveResponsesBreakpoint(
        std::sync::atomic::AtomicUsize::new(0),
    ));
    for _ in 0..2 {
        let mut stream = client
            .stream_with_host_options(
                &model,
                req.clone(),
                overrides.clone(),
                octet_ai::HostRequestOptions {
                    on_payload: Some(hook.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let written = std::fs::read_to_string(&trace_path).unwrap();
    std::fs::remove_file(&trace_path).unwrap();
    assert!(!written.contains("SECRET"));
    assert!(!written.contains("loopback-api-key"));
    assert!(!written.contains(&server.uri()));
    let lines: Vec<Value> = written
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for name in ["canonical", "encoded", "wire_http"] {
        let stages: Vec<_> = lines
            .iter()
            .filter(|line| line["event"] == "cache_prefix" && line["stage"] == name)
            .collect();
        assert_eq!(stages.len(), 2);
        assert_eq!(
            stages[1]["prefix_common_bytes"],
            stages[0]["prefix_view_bytes"]
        );
        assert_eq!(stages[1]["previous_suffix_estimated_tokens"], 0);
        if name == "wire_http" {
            assert_ne!(stages[0]["digest"], stages[1]["digest"]);
            assert_eq!(
                stages[0]["cache_controls"]["breakpoints"],
                serde_json::json!(["/input/0/content/0"])
            );
            assert_eq!(
                stages[1]["cache_controls"]["breakpoints"],
                serde_json::json!(["/input/2/output/0"])
            );
            let usage = lines
                .iter()
                .find(|line| line["event"] == "cache_usage" && line["id"] == stages[1]["id"])
                .unwrap();
            assert_eq!(usage["uncached_input_tokens"], 1);
            assert_eq!(usage["cache_write_tokens"], 1);
            assert_eq!(usage["total_input_tokens"], 100);
            assert_eq!(usage["cache_efficiency"], 0.98);
        } else {
            assert_eq!(stages[0]["digest"], stages[1]["digest"]);
        }
    }
    let received = server.received_requests().await.unwrap();
    for request in received {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["service_tier"], "flex");
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
    let prefixes: Vec<_> = lines
        .iter()
        .filter(|line| line["event"] == "cache_prefix")
        .collect();
    assert_eq!(prefixes.len(), 3, "{written}");
    assert_eq!(prefixes[0]["stage"], "canonical");
    assert_eq!(prefixes[1]["stage"], "encoded");
    assert_eq!(prefixes[2]["stage"], "wire_http");
    let lines: Vec<_> = lines
        .iter()
        .filter(|line| line["event"] != "cache_prefix")
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
    assert_eq!(prefixes[2]["id"], request["id"]);
}
