#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use octet_ai::{
    AiClient, AiError, Auth, CacheCompatibility, CacheRetention, Capabilities,
    CompatibilityMode::Strict, Endpoint, EndpointId, EndpointTransport, Message, ModalitySet,
    Model, ModelId, ModelLimits, ModelSpec, OutputFormat, OutputModalities, Protocol,
    ReasoningConfig, ReasoningMode, Request, Response, StreamEvent, ToolChoice, ToolDef,
    UserMessage, UserPart,
};

fn fixture_model(base_url: &str) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("gemini-current-fixture".to_owned()),
            api_name: "gemini-2.5-flash".to_owned(),
            display_name: Some("Gemini current fixture".to_owned()),
            endpoint: EndpointId("google-current".to_owned()),
            protocol: Protocol::GoogleGenerativeAi,
            capabilities: Capabilities {
                responses_features: Default::default(),
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: true,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 1_000_000,
                max_output_tokens: 65_536,
            },
            pricing: None,
            cache: CacheCompatibility::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("google-current".to_owned()),
            base_url: url::Url::parse(base_url).expect("fixture endpoint URL"),
            auth: Auth::header(
                http::HeaderName::from_static("x-goog-api-key"),
                "fixture-key",
            ),
            default_headers: http::HeaderMap::new(),
            transport: EndpointTransport::Http,
            runtime: Default::default(),
            timeout: Duration::from_secs(2),
        }),
    }
}

fn fixture_request() -> Request {
    Request {
        system: Some("Use the available tool when needed.".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Look up Paris.".to_owned())],
        })],
        tools: vec![ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "lookup".to_owned(),
            description: "Look up a city.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
        }],
        tool_choice: ToolChoice::Named("lookup".to_owned()),
        max_output_tokens: Some(128),
        temperature: Some(0.2),
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: Strict,
        cache_retention: CacheRetention::Short,
        session_id: None,
    }
}

fn plain_fixture_request() -> Request {
    let mut request = fixture_request();
    request.tools.clear();
    request.tool_choice = ToolChoice::Auto;
    request.system = None;
    request
}

#[tokio::test]
async fn gemini_current_request_tool_stream_signature_and_usage_fixture() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:streamGenerateContent"))
        .and(header("x-goog-api-key", "fixture-key"))
        .and(body_string_contains("systemInstruction"))
        .and(body_string_contains("functionDeclarations"))
        .and(body_string_contains("parametersJsonSchema"))
        .and(body_string_contains("allowedFunctionNames"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"responseId\":\"gemini-current-1\",\"candidates\":[{\"index\":0,\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"lookup\",\"id\":\"call-1\",\"args\":{\"city\":\"Paris\"}},\"thoughtSignature\":\"opaque-signature\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":12,\"cachedContentTokenCount\":2,\"candidatesTokenCount\":5,\"thoughtsTokenCount\":1,\"totalTokenCount\":18}}\n\n",
                ),
        )
        .mount(&server)
        .await;

    let response: Response = AiClient::new()
        .complete(
            &fixture_model(&format!("{}/v1beta/", server.uri())),
            fixture_request(),
        )
        .await
        .expect("Gemini current loopback fixture");
    let metrics = response.inference.as_ref().unwrap();
    let client = metrics.client.as_ref().unwrap();
    assert_eq!(
        client.scope,
        Some(octet_ai::inference::ClientTimingScope::Request)
    );
    assert!(client.first_tool_arguments_ns.is_some());
    assert_eq!(
        metrics.server_unavailable,
        Some(octet_ai::inference::ServerTimingUnavailable::NotReported)
    );

    assert_eq!(response.response_id.as_deref(), Some("gemini-current-1"));
    assert_eq!(response.usage.input_tokens, 10);
    assert_eq!(response.usage.cache_read_tokens, 2);
    assert_eq!(response.usage.output_tokens, 6);
    assert_eq!(response.usage.reasoning_tokens, 1);
    assert_eq!(response.usage.total_tokens, 18);
    assert!(matches!(
        response.stop_reason,
        octet_ai::StopReason::ToolUse
    ));
    assert!(response.message.content.iter().any(|part| matches!(
        part,
        octet_ai::AssistantPart::ProviderMetadata(
            octet_ai::ProviderPartMetadata::GoogleThoughtSignature { signature }
        ) if signature == "opaque-signature"
    )));
    assert!(response.message.content.iter().any(|part| matches!(
        part,
        octet_ai::AssistantPart::ToolCall(call)
            if call.name == "lookup" && call.arguments_json == r#"{"city":"Paris"}"#
    )));
}

#[tokio::test]
async fn opencode_parallel_google_calls_survive_streaming_and_result_replay() {
    use octet_ai::{AssistantPart, StopReason, ToolResult, ToolResultPart};
    use wiremock::{Request as WireRequest, Respond};

    struct Script;
    impl Respond for Script {
        fn respond(&self, request: &WireRequest) -> ResponseTemplate {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let replay = body["contents"].as_array().unwrap().iter().any(|content| {
                content["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|part| part.get("functionResponse").is_some())
            });
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(if replay {
                    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Both cities received.\"}]},\"finishReason\":\"STOP\"}]}\n\n"
                } else {
                    include_str!("fixtures/google/parallel_tool_calls.sse")
                })
        }
    }

    for (provider, api_name, base_path) in [
        ("opencode", "gemini-4-argon", "/zen/v1/"),
        (
            "opencode-go",
            "gemini-4-argon-contributor",
            "/inference/go/v1/",
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!(
                "{base_path}models/{api_name}:streamGenerateContent"
            )))
            .and(header("authorization", "Bearer fixture-key"))
            .and(header("x-opencode-session", "parallel-session"))
            .respond_with(Script)
            .expect(2)
            .mount(&server)
            .await;
        let mut model = fixture_model(&format!("{}{base_path}", server.uri()));
        let spec = Arc::make_mut(&mut model.spec);
        spec.api_name = api_name.into();
        spec.id = ModelId(format!("{provider}/{api_name}"));
        spec.endpoint = EndpointId(format!("{provider}-google"));
        spec.cache.send_session_affinity_headers = true;
        let endpoint = Arc::make_mut(&mut model.endpoint);
        endpoint.id = model.spec.endpoint.clone();
        endpoint.auth = Auth::bearer("fixture-key");
        let mut request = fixture_request();
        request.session_id = Some("parallel-session".into());
        let client = AiClient::new();
        let events = client
            .stream(&model, request.clone())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::Started { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ToolCallStart { .. }))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
                .count(),
            2
        );
        let response = events
            .into_iter()
            .find_map(|event| match event {
                StreamEvent::Finished(response) => Some(response),
                _ => None,
            })
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.usage.total_tokens, 20);
        let calls = response
            .message
            .content
            .iter()
            .filter_map(|part| match part {
                AssistantPart::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.0, "call-paris");
        assert_eq!(calls[0].arguments_json, r#"{"city":"Paris"}"#);
        assert_eq!(calls[1].id.0, "call-berlin");
        assert_eq!(calls[1].arguments_json, r#"{"city":"Berlin"}"#);
        assert!(calls.iter().all(|call| call.argument_error.is_none()));
        request.messages.push(Message::Assistant(response.message));
        request.messages.push(Message::User(UserMessage {
            content: calls
                .iter()
                .enumerate()
                .map(|(position, call)| {
                    UserPart::ToolResult(ToolResult {
                        tool_call_id: call.id.clone(),
                        content: vec![ToolResultPart::Text(format!("result-{position}"))],
                        is_error: false,
                        added_tool_names: None,
                    })
                })
                .collect(),
        }));
        let final_response = client.complete(&model, request).await.unwrap();
        assert!(matches!(final_response.message.content.as_slice(),
            [AssistantPart::Text(text)] if text == "Both cities received."
        ));
        let requests = server.received_requests().await.unwrap();
        let replay: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        for (position, (id, city, signature)) in [
            ("call-paris", "Paris", "paris-signature"),
            ("call-berlin", "Berlin", "berlin-signature"),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                replay["contents"][1]["parts"][position],
                serde_json::json!({
                    "functionCall": {"id": id, "name": "lookup", "args": {"city": city}},
                    "thoughtSignature": signature,
                })
            );
            assert_eq!(
                replay["contents"][2]["parts"][position],
                serde_json::json!({
                    "functionResponse": {"id": id, "name": "lookup", "response": {"output": format!("result-{position}")}},
                })
            );
        }
        // Provider metadata is retained for same-model replay, never exposed as reasoning.
        assert!(replay["contents"][1]["parts"][0].get("thought").is_none());
    }
}

// A signature is bound to the provider's original part. The scripted endpoint
// rejects changed signed text on continuation, as opposed to accepting any JSON
// and letting an assertion after a successful response miss the HTTP 400 path.
#[tokio::test]
async fn signed_delta_replay_with_large_write_uses_native_google_contract_on_every_route() {
    use octet_ai::types::ReasoningOptions;
    use octet_ai::{
        AssistantPart, ConstrainedSampling, ConstrainedSamplingStrict, ReasoningCapability,
        ReasoningControl, ReasoningEffort, ToolResult, ToolResultPart,
    };
    use serde_json::{json, Value};
    use wiremock::{Request as WireRequest, Respond};

    const CODE: &str = "line = \"quoted\";\n";
    let arguments = json!({"path": "large.txt", "content": CODE.repeat(16_384)});
    struct SignedReplay {
        arguments: Value,
    }
    impl Respond for SignedReplay {
        fn respond(&self, request: &WireRequest) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let contents = body["contents"].as_array().unwrap();
            if contents.len() == 1 {
                let mut sse = String::new();
                // Repeated, extending-prefix and shortening-prefix text chunks
                // are all deltas, for thought summaries as well as visible text.
                for thought in [true, false] {
                    for text in ["a", "a", "aa", "a"] {
                        sse.push_str(&format!("data: {}\n\n", json!({
                            "candidates": [{"content": {"parts": [{"text": text, "thought": thought}]}}]
                        })));
                    }
                    sse.push_str(&format!("data: {}\n\n", json!({
                        "candidates": [{"content": {"parts": [{"text": "", "thought": thought,
                            "thoughtSignature": if thought { "dGhvdWdodA==" } else { "dGV4dA==" }}]}}]
                    })));
                }
                sse.push_str(&format!("data: {}\n\n", json!({
                    "candidates": [{"content": {"parts": [{
                        "functionCall": {"id": "write-1", "name": "write", "args": self.arguments},
                        "thoughtSignature": "d3JpdGU="
                    }]}, "finishReason": "STOP"}]
                })));
                return ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse);
            }
            let expected = json!([
                {"text": "aaaaa", "thought": true, "thoughtSignature": "dGhvdWdodA=="},
                {"text": "aaaaa", "thoughtSignature": "dGV4dA=="},
                {"functionCall": {"id": "write-1", "name": "write", "args": self.arguments},
                 "thoughtSignature": "d3JpdGU="}
            ]);
            if contents[1]["parts"] != expected {
                return ResponseTemplate::new(400).set_body_json(json!({
                    "error": {"code": 400, "status": "INVALID_ARGUMENT",
                        "message": "synthetic signed-part replay mismatch"}
                }));
            }
            assert_eq!(
                contents[2]["parts"][0]["functionResponse"],
                json!({
                    "id": "write-1", "name": "write", "response": {"output": "written"}
                })
            );
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Written.\"}]},\"finishReason\":\"STOP\"}]}\n\n")
        }
    }

    for (provider, api_name, base_path) in [
        ("google", "gemini-4-argon", "/v1beta/"),
        ("opencode", "gemini-4-argon", "/zen/v1/"),
        (
            "opencode-go",
            "gemini-4-argon-contributor",
            "/inference/go/v1/",
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!(
                "{base_path}models/{api_name}:streamGenerateContent"
            )))
            .respond_with(SignedReplay {
                arguments: arguments.clone(),
            })
            .expect(2)
            .mount(&server)
            .await;
        let mut model = fixture_model(&format!("{}{base_path}", server.uri()));
        let spec = Arc::make_mut(&mut model.spec);
        spec.id = ModelId(format!("{provider}/{api_name}"));
        spec.api_name = api_name.into();
        spec.endpoint = EndpointId(format!("{provider}-google"));
        spec.cache.send_session_affinity_headers = provider != "google";
        spec.capabilities.reasoning = Some(ReasoningCapability {
            options: Some(ReasoningOptions {
                values: vec!["LOW".into(), "MEDIUM".into(), "HIGH".into()],
                default: Some("HIGH".into()),
            }),
            control: ReasoningControl::Effort,
            exposes_text: true,
            preserves_state: true,
            effort_budgets: None,
            openai_chat_mode: Default::default(),
            min_effort: ReasoningEffort::Low,
            max_effort: ReasoningEffort::High,
        });
        let endpoint = Arc::make_mut(&mut model.endpoint);
        endpoint.id = model.spec.endpoint.clone();
        if provider != "google" {
            endpoint.auth = Auth::bearer("fixture-key");
        }
        let mut request = fixture_request();
        request.reasoning = ReasoningConfig::Effort(ReasoningEffort::Medium);
        request.session_id = Some("signed-write".into());
        request.max_output_tokens = Some(65_536);
        request.tool_choice = ToolChoice::Auto;
        request.tools = vec![ToolDef {
            async_execution: false,
            name: "write".into(),
            description: "Write a file.".into(),
            parameters: json!({"type": "object", "properties": {
                "path": {"type": "string"}, "content": {"type": "string"}
            }, "required": ["path", "content"]}),
            constrained_sampling: Some(ConstrainedSampling::JsonSchema {
                strict: ConstrainedSamplingStrict::Require,
            }),
        }];
        let client = AiClient::new();
        let first = client.complete(&model, request.clone()).await.unwrap();
        let call = first
            .message
            .content
            .iter()
            .find_map(|part| match part {
                AssistantPart::ToolCall(call) => Some(call),
                _ => None,
            })
            .unwrap();
        assert!(call.argument_error.is_none());
        assert_eq!(
            serde_json::from_str::<Value>(&call.arguments_json).unwrap(),
            arguments
        );
        let id = call.id.clone();
        // Exercise the durable canonical representation, not just an in-memory
        // pass-through of provider JSON.
        let assistant =
            serde_json::from_str(&serde_json::to_string(&first.message).unwrap()).unwrap();
        request.messages.push(Message::Assistant(assistant));
        request.messages.push(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: id,
                content: vec![ToolResultPart::Text("written".into())],
                is_error: false,
                added_tool_names: None,
            })],
        }));
        let final_response = client.complete(&model, request).await.unwrap();
        assert!(matches!(final_response.message.content.as_slice(),
            [AssistantPart::Text(text)] if text == "Written."
        ));
        for request in server.received_requests().await.unwrap() {
            assert_eq!(request.url.query(), Some("alt=sse"));
            if provider != "google" {
                assert_eq!(request.headers["authorization"], "Bearer fixture-key");
                assert_eq!(request.headers["x-opencode-session"], "signed-write");
            } else {
                assert_eq!(request.headers["x-goog-api-key"], "fixture-key");
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(
                body["generationConfig"]["thinkingConfig"],
                json!({
                    "thinkingLevel": "MEDIUM", "includeThoughts": true
                })
            );
            assert!(body["generationConfig"]["thinkingConfig"]
                .get("thinkingBudget")
                .is_none());
            let declaration = &body["tools"][0]["functionDeclarations"][0];
            assert!(declaration.get("parameters").is_none());
            assert_eq!(
                declaration["parametersJsonSchema"]["additionalProperties"],
                false
            );
            assert_eq!(
                body["toolConfig"]["functionCallingConfig"]["mode"],
                "VALIDATED"
            );
        }
    }
}

#[tokio::test]
async fn gemini_current_error_and_drop_cancellation_fixtures() {
    let error_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"responseId\":\"error-1\",\"error\":{\"code\":400,\"status\":\"INVALID_ARGUMENT\",\"message\":\"fixture rejection\"}}\n\n",
                ),
        )
        .mount(&error_server)
        .await;
    let error = AiClient::new()
        .complete(
            &fixture_model(&format!("{}/v1beta/", error_server.uri())),
            plain_fixture_request(),
        )
        .await
        .expect_err("provider error must not become a successful response");
    let AiError::StreamFailure { inner, progress } = error else {
        panic!("expected annotated Google stream failure");
    };
    assert!(matches!(
        inner.as_ref(),
        AiError::Provider(provider)
            if provider.code.as_deref() == Some("400")
                && provider.kind.as_deref() == Some("INVALID_ARGUMENT")
                && provider.message == "fixture rejection"
                && provider.request_id.as_deref() == Some("error-1")
    ));
    assert_eq!(progress.provider_events, 1);
    assert_eq!(progress.decoded_events, 0);
    assert_eq!(progress.content_bytes, 0);
    assert_eq!(progress.buffered_bytes, 0);
    assert!(progress.first_body_seen);
    let last_event_ms = progress
        .last_event_ms
        .expect("provider error must retain stream timing metadata");
    assert!(progress.elapsed_ms >= last_event_ms);

    let cancel_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"responseId\":\"cancel-1\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"first\"}]}}]}\n\ndata: {not-json}\n\n",
                ),
        )
        .mount(&cancel_server)
        .await;
    let mut stream = AiClient::new()
        .stream(
            &fixture_model(&format!("{}/v1beta/", cancel_server.uri())),
            plain_fixture_request(),
        )
        .await
        .expect("stream headers");
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::Started { .. }))
    ));
    // ResponseStream owns the HTTP body; dropping it is the provider-neutral
    // cancellation boundary and must not decode the trailing malformed frame.
    drop(stream);
}
