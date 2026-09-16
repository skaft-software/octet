#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use octet_ai::responses::encode_responses_replay;
use octet_ai::*;
use serde_json::{json, Value};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn model(url: &str, protocol: Protocol) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("fixture".into()),
            endpoint: EndpointId("fixture".into()),
            api_name: "fixture".into(),
            display_name: None,
            protocol,
            capabilities: Capabilities {
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 10000,
                max_output_tokens: 1000,
            },
            pricing: None,
            cache: CacheCompatibility::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("fixture".into()),
            base_url: url.parse().unwrap(),
            auth: Auth::bearer("fixture-secret"),
            default_headers: Default::default(),
            transport: EndpointTransport::Http,
            runtime: RequestRuntime::default(),
            timeout: Duration::from_secs(2),
        }),
    }
}

fn request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".into())],
        })],
        tools: vec![ToolDef {
            name: "language".into(),
            description: "a grammar".into(),
            parameters: json!({"type":"object", "properties":{"source":{"type":"string"}},
                "required":["source"], "additionalProperties":false}),
            constrained_sampling: Some(ConstrainedSampling::Grammar {
                variants: GrammarVariants {
                    openai_lark: Some("start: /[\\s\\S]*/".into()),
                    openai_regex: None,
                },
            }),
        }],
        tool_choice: ToolChoice::Named("language".into()),
        max_output_tokens: Some(100),
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

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

fn custom_sse(protocol: Protocol, text: &str) -> String {
    let midpoint = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(0, |(index, _)| index);
    let (first, rest) = text.split_at(midpoint);
    match protocol {
        Protocol::OpenAiChat => format!(
            "{}data: [DONE]\n\n",
            sse(&[
                // The provider may stream the input before the call id/name.
                json!({"id":"chat", "choices":[{"delta":{"tool_calls":[{
                "index":0, "type":"custom", "custom":{"input":first}}]}}]}),
                json!({"id":"chat", "choices":[{"delta":{"tool_calls":[{
                "index":0, "id":"call_1", "custom":{"name":"language","input":rest}}]}}]}),
                json!({"id":"chat", "choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            ])
        ),
        Protocol::OpenAiResponses => sse(&[
            json!({"type":"response.created","response":{"id":"resp_1"}}),
            json!({"type":"response.output_item.added","output_index":0,
                "item":{"type":"custom_tool_call","id":"ct_1","call_id":"call_1","name":"language","input":""}}),
            json!({"type":"response.custom_tool_call_input.delta","output_index":0,"delta":first}),
            json!({"type":"response.custom_tool_call_input.delta","output_index":0,"delta":rest}),
            json!({"type":"response.custom_tool_call_input.done","output_index":0,"input":text}),
            json!({"type":"response.output_item.done","output_index":0,
                "item":{"type":"custom_tool_call","id":"ct_1","input":text}}),
            json!({"type":"response.completed","response":{"output":[{
                "type":"custom_tool_call","id":"ct_1","call_id":"call_1","name":"language","input":text}],
                "usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5}}}),
        ]),
        _ => unreachable!(),
    }
}

async fn serve(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

fn tool_result() -> UserMessage {
    UserMessage {
        content: vec![UserPart::ToolResult(ToolResult {
            tool_call_id: ToolCallId("call_1".into()),
            content: vec![ToolResultPart::Text("accepted".into())],
            is_error: false,
            added_tool_names: None,
        })],
    }
}

#[tokio::test]
async fn grammar_custom_calls_decode_and_replay_on_real_http_for_both_openai_codecs() {
    let text = "quote \" slash \\ newline\n雪";
    for protocol in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        let server = MockServer::start().await;
        serve(&server, custom_sse(protocol, text)).await;
        let model = model(&server.uri(), protocol);
        let client =
            AiClient::with_http_client(reqwest::Client::builder().no_proxy().build().unwrap());
        let mut req = request();
        let mut stream = client.stream(&model, req.clone()).await.unwrap();
        let mut json_deltas = String::new();
        let mut ends = 0;
        let mut response = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                StreamEvent::ToolCallArgsDelta { delta, .. } => json_deltas.push_str(&delta),
                StreamEvent::ToolCallEnd { argument_error, .. } => {
                    assert!(argument_error.is_none());
                    ends += 1;
                }
                StreamEvent::Finished(value) => response = Some(value),
                _ => {}
            }
        }
        assert_eq!(ends, 1, "duplicate done events cannot re-execute a tool");
        assert_eq!(
            serde_json::from_str::<Value>(&json_deltas).unwrap(),
            json!({"source":text})
        );
        let response = response.unwrap();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        let AssistantPart::ToolCall(call) = &response.message.content[0] else {
            panic!("missing call")
        };
        assert_eq!(call.id.0, "call_1");
        assert_eq!(
            serde_json::from_str::<Value>(&call.arguments_json).unwrap(),
            json!({"source":text})
        );
        req.messages
            .push(Message::Assistant(response.message.clone()));
        req.messages.push(Message::User(tool_result()));
        client.complete(&model, req.clone()).await.unwrap();
        if protocol == Protocol::OpenAiResponses {
            let replay = vec![
                ResponsesReplayItem::Output(response.responses_output.unwrap()),
                ResponsesReplayItem::User(tool_result()),
            ];
            req.responses = Some(ResponsesOptions::full_replay(encode_responses_replay(
                &model, None, &replay,
            )));
            client.complete(&model, req).await.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        let first: Value = requests[0].body_json().unwrap();
        assert_eq!(first["tools"][0]["type"], "custom");
        assert_eq!(first["tool_choice"]["type"], "custom");
        for captured in &requests[1..] {
            let body: Value = captured.body_json().unwrap();
            if protocol == Protocol::OpenAiChat {
                assert_eq!(
                    body["messages"][1]["tool_calls"][0]["custom"]["input"],
                    text
                );
                assert_eq!(body["messages"][1]["tool_calls"][0]["type"], "custom");
                assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
            } else {
                let input = body["input"].as_array().unwrap();
                assert!(input
                    .iter()
                    .any(|item| item["type"] == "custom_tool_call" && item["input"] == text));
                assert!(input
                    .iter()
                    .any(|item| item["type"] == "custom_tool_call_output"
                        && item["call_id"] == "call_1"));
                assert!(!input
                    .iter()
                    .any(|item| item["type"] == "function_call_output"));
            }
        }
    }
}

#[tokio::test]
async fn grammar_terminal_only_input_is_backfilled_but_changed_input_fails_closed() {
    for (delta, terminal, succeeds) in [("", "\"雪\n", true), ("abc", "abd", false)] {
        let server = MockServer::start().await;
        serve(&server, sse(&[
            json!({"type":"response.created","response":{"id":"r"}}),
            json!({"type":"response.output_item.added","output_index":0,
                "item":{"type":"custom_tool_call","id":"c","call_id":"call_1","name":"language"}}),
            json!({"type":"response.custom_tool_call_input.delta","output_index":0,"delta":delta}),
            json!({"type":"response.custom_tool_call_input.done","output_index":0,"input":terminal}),
            json!({"type":"response.completed","response":{}}),
        ])).await;
        let client =
            AiClient::with_http_client(reqwest::Client::builder().no_proxy().build().unwrap());
        let result = client
            .complete(&model(&server.uri(), Protocol::OpenAiResponses), request())
            .await;
        assert_eq!(result.is_ok(), succeeds, "{result:?}");
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "decode errors must never retry"
        );
    }
}

#[tokio::test]
async fn proxy_environment_is_consumed_and_no_proxy_bypasses_it_without_a_second_policy() {
    let proxy = MockServer::start().await;
    let origin = MockServer::start().await;
    serve(&proxy, custom_sse(Protocol::OpenAiChat, "through proxy")).await;
    serve(&origin, custom_sse(Protocol::OpenAiChat, "direct")).await;
    let client = AiClient::try_with_proxy_environment(std::collections::BTreeMap::from([
        ("http_proxy".into(), proxy.uri()),
        ("HTTP_PROXY".into(), "http://127.0.0.1:1".into()),
        ("no_proxy".into(), "127.0.0.1".into()),
    ]))
    .unwrap();
    // .invalid never resolves: receipt at the loopback proxy proves the route.
    client
        .complete(
            &model("http://target.invalid/v1/", Protocol::OpenAiChat),
            request(),
        )
        .await
        .unwrap();
    client
        .complete(&model(&origin.uri(), Protocol::OpenAiChat), request())
        .await
        .unwrap();
    let received = proxy.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].headers["host"], "target.invalid");
    assert_eq!(origin.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn unsupported_proxy_fails_before_auth_or_dispatch_and_never_leaks_userinfo() {
    let server = MockServer::start().await;
    let client = AiClient::try_with_proxy_environment(std::collections::BTreeMap::from([(
        "ALL_PROXY".into(),
        "socks5://private-user:private-password@127.0.0.1:1".into(),
    )]))
    .unwrap()
    .track_request_dispatch();
    let mut model = model(&server.uri(), Protocol::OpenAiChat);
    Arc::make_mut(&mut model.endpoint).auth = Auth::BearerEnv {
        var: "OCTET_PARITY_MISSING_AUTH".into(),
    };
    let error = client.complete(&model, request()).await.unwrap_err();
    assert!(
        matches!(error, AiError::Config(_)),
        "proxy must fail before auth: {error:?}"
    );
    assert!(!client.request_may_have_been_sent());
    assert!(!format!("{error:?}").contains("private-"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn preferred_websocket_obeys_proxy_via_http_and_does_not_prewarm_directly() {
    let proxy = MockServer::start().await;
    serve(&proxy, custom_sse(Protocol::OpenAiResponses, "proxied")).await;
    let client = AiClient::try_with_proxy_environment(std::collections::BTreeMap::from([(
        "ALL_PROXY".into(),
        proxy.uri(),
    )]))
    .unwrap();
    let mut model = model("http://target.invalid/v1/", Protocol::OpenAiResponses);
    Arc::make_mut(&mut model.endpoint).transport = EndpointTransport::WebSocketPreferred;
    let mut req = request();
    req.session_id = Some("session".into());
    client.prewarm_responses(&model, req.clone()).await.unwrap();
    assert!(proxy.received_requests().await.unwrap().is_empty());
    client.complete(&model, req).await.unwrap();
    let requests = proxy.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("upgrade"));
}

#[tokio::test]
async fn proxy_http_error_echoes_are_redacted_and_never_retried() {
    use base64::Engine as _;
    let proxy = MockServer::start().await;
    let basic = base64::engine::general_purpose::STANDARD.encode("proxy-user:proxy secret");
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(407)
                .set_body_string(format!("proxy-user proxy secret proxy%20secret {basic}")),
        )
        .mount(&proxy)
        .await;
    let mut url: url::Url = proxy.uri().parse().unwrap();
    url.set_username("proxy-user").unwrap();
    url.set_password(Some("proxy secret")).unwrap();
    let client = AiClient::try_with_proxy_environment(std::collections::BTreeMap::from([(
        "HTTP_PROXY".into(),
        url.to_string(),
    )]))
    .unwrap();
    let error = client
        .complete(
            &model("http://target.invalid/", Protocol::OpenAiChat),
            request(),
        )
        .await
        .unwrap_err();
    let diagnostic = format!("{error:?}");
    assert!(!diagnostic.contains("proxy-user"));
    assert!(!diagnostic.contains("proxy secret"));
    assert!(!diagnostic.contains("proxy%20secret"));
    assert!(!diagnostic.contains(&basic));
    assert_eq!(proxy.received_requests().await.unwrap().len(), 1);
}

fn priced_responses_model(url: &str, api_name: &str, profile: ResponsesRuntimeProfile) -> Model {
    let mut model = model(url, Protocol::OpenAiResponses);
    let spec = Arc::make_mut(&mut model.spec);
    spec.api_name = api_name.into();
    spec.pricing = Some(Pricing {
        input: TokenRate(1_000_000),
        output: TokenRate(2_000_000),
        cache_read: TokenRate(500_000),
        cache_write_5m: TokenRate(1_250_000),
        cache_write_1h: None,
        reasoning: Some(TokenRate(3_000_000)),
        tiers: vec![],
    });
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile = profile;
    model
}

fn tier_terminal(echo: Option<&str>, usage: bool, incomplete: bool) -> Value {
    let mut response = json!({"id":"priced","output":[]});
    if let Some(echo) = echo {
        response["service_tier"] = echo.into();
    }
    if usage {
        response["usage"] = json!({"input_tokens":10,"output_tokens":4,"total_tokens":14,
            "input_tokens_details":{"cached_tokens":2},"output_tokens_details":{"reasoning_tokens":1}});
    }
    if incomplete {
        response["incomplete_details"] = json!({"reason":"max_output_tokens"});
    }
    json!({"type":if incomplete {"response.incomplete"} else {"response.completed"},"response":response})
}

fn tier_request(tier: Option<ServiceTier>) -> Request {
    let mut req = request();
    req.tools.clear();
    req.tool_choice = ToolChoice::Auto;
    req.responses = tier.map(|tier| {
        ResponsesOptions::full_replay(ResponsesInput::default()).with_service_tier(tier)
    });
    req
}

#[tokio::test]
async fn service_tier_wire_request_and_terminal_echo_settle_qualified_costs() {
    use ResponsesRuntimeProfile::{Codex, Default as Ordinary};
    use ServiceTier::{Auto, Default as Standard, Flex, Priority};
    for (profile, api_name, requested, echoed, expected) in [
        (Codex, "gpt-5.5", Some(Priority), Some("default"), Some(45)),
        (Codex, "gpt-5.5", Some(Priority), None, Some(45)),
        (Codex, "gpt-5.4", Some(Priority), Some("priority"), Some(36)),
        (Codex, "gpt-5.5", Some(Flex), Some("default"), Some(9)),
        (Codex, "gpt-5.5", Some(Priority), Some("flex"), Some(9)),
        (Codex, "gpt-5.5", Some(Standard), Some("default"), Some(18)),
        (Codex, "gpt-5.5", Some(Auto), None, None),
        (Codex, "gpt-5.5", Some(Auto), Some("default"), Some(18)),
        (Codex, "gpt-5.5", Some(Priority), Some("future-tier"), None),
        (Ordinary, "gpt-5.5", None, Some("priority"), None),
        (Ordinary, "gpt-5.5", None, None, Some(18)),
    ] {
        let server = MockServer::start().await;
        serve(
            &server,
            sse(&[
                json!({"type":"response.created","response":{"id":"priced"}}),
                tier_terminal(echoed, true, false),
            ]),
        )
        .await;
        let model = priced_responses_model(&server.uri(), api_name, profile);
        let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
        let response = client
            .complete(&model, tier_request(requested))
            .await
            .unwrap();
        assert_eq!(
            response.cost.map(|cost| cost.total),
            expected,
            "{profile:?}/{api_name}/{requested:?}/{echoed:?}"
        );
        assert_eq!(response.usage.total_tokens, 14);
        if expected.is_none() {
            assert!(response
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "unpriced_responses_tier"));
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = requests[0].body_json().unwrap();
        assert_eq!(
            body.get("service_tier").and_then(Value::as_str),
            requested.map(ServiceTier::wire_value)
        );
        assert_eq!(body["store"], false, "pricing must not change retention");
    }
}

#[tokio::test]
async fn service_tier_incomplete_is_priced_and_missing_usage_is_not_zero() {
    for has_usage in [false, true] {
        let server = MockServer::start().await;
        serve(
            &server,
            sse(&[
                json!({"type":"response.created","response":{"id":"priced"}}),
                tier_terminal(Some("default"), has_usage, true),
            ]),
        )
        .await;
        let model =
            priced_responses_model(&server.uri(), "gpt-5.5", ResponsesRuntimeProfile::Codex);
        let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
        let response = client
            .complete(&model, tier_request(Some(ServiceTier::Priority)))
            .await
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::MaxTokens);
        assert_eq!(
            response.cost.map(|cost| cost.total),
            has_usage.then_some(45)
        );
    }
}

#[tokio::test]
async fn service_tier_websocket_carries_request_qualifier_into_terminal_pricing() {
    use futures_util::SinkExt;
    use tokio_tungstenite::{accept_async, tungstenite::Message as WsMessage};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(socket).await.unwrap();
        let incoming = websocket.next().await.unwrap().unwrap();
        let body: Value = serde_json::from_str(incoming.to_text().unwrap()).unwrap();
        assert_eq!(body["service_tier"], "priority");
        assert_eq!(body["store"], false);
        websocket
            .send(WsMessage::Text(
                json!({"type":"response.created","response":{"id":"priced"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(WsMessage::Text(
                tier_terminal(Some("default"), true, false)
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let mut model = priced_responses_model(
        &format!("http://{address}/"),
        "gpt-5.5",
        ResponsesRuntimeProfile::Codex,
    );
    Arc::make_mut(&mut model.endpoint).transport = EndpointTransport::WebSocketPreferred;
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    let mut req = tier_request(Some(ServiceTier::Priority));
    req.session_id = Some("priced-session".into());
    let response = tokio::time::timeout(Duration::from_secs(5), client.complete(&model, req))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.cost.unwrap().total, 45);
    server.await.unwrap();
}

#[tokio::test]
async fn grammar_authoritative_terminal_cannot_change_a_closed_call() {
    let server = MockServer::start().await;
    let mut wire = custom_sse(Protocol::OpenAiResponses, "original");
    // Replace only the terminal; preceding deltas and done events still agree.
    wire = wire
        .split("\n\n")
        .filter(|event| !event.contains("response.completed"))
        .filter(|event| !event.is_empty())
        .map(|event| format!("{event}\n\n"))
        .collect();
    wire.push_str(&sse(&[json!({"type":"response.completed","response":{"output":[{
        "type":"custom_tool_call","id":"ct_1","call_id":"call_1","name":"language","input":"changed"}]}})]));
    serve(&server, wire).await;
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    assert!(client
        .complete(&model(&server.uri(), Protocol::OpenAiResponses), request())
        .await
        .is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

fn reasoning_capability() -> ReasoningCapability {
    serde_json::from_value(json!({"control":"effort", "exposes_text":true,
        "preserves_state":false, "min_effort":"low", "max_effort":"high"}))
    .unwrap()
}

#[tokio::test]
async fn model_preset_config_is_consumed_and_public_model_projection_redacts_headers() {
    let server = MockServer::start().await;
    serve(&server, custom_sse(Protocol::OpenAiChat, "ok")).await;
    let original = model(&server.uri(), Protocol::OpenAiChat);
    let mut serialized = serde_json::to_value(&*original.spec).unwrap();
    serialized["preset"] = json!({"sampling_params":{"top_p":0.7,"temperature":0.4},
        "headers":{"x-model-secret":"secret-preset-value", "x-order":"model", "authorization":"Bearer model-attempt"}, "vllm_priority":-3});
    let config: CatalogConfig = serde_json::from_value(json!({
        "endpoints":[{"id":"fixture", "base_url":format!("{}/", server.uri()),
            "auth":{"kind":"none"}, "default_headers":{"x-order":"endpoint"}}],
        "models":[serialized]
    }))
    .unwrap();
    let catalog = ModelCatalog::from_config(config).unwrap();
    let mut selected = catalog.resolve(&ModelId("fixture".into())).unwrap().clone();
    Arc::make_mut(&mut selected.endpoint).auth = Auth::bearer("authoritative-value");
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    client.complete(&selected, request()).await.unwrap();
    let mut explicit = request();
    explicit.temperature = Some(0.9);
    client.complete(&selected, explicit).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert!((first["temperature"].as_f64().unwrap() - 0.4).abs() < 0.0001);
    assert!((second["temperature"].as_f64().unwrap() - 0.9).abs() < 0.0001);
    assert_eq!(first["top_p"], 0.7);
    assert_eq!(first["priority"], -3);
    assert_eq!(requests[0].headers["x-order"], "model");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer authoritative-value"
    );
    assert_eq!(requests[0].headers["x-model-secret"], "secret-preset-value");
    assert_eq!(selected.spec.preset.sampling_params["temperature"], 0.4);
    assert!(!serde_json::to_string(&*selected.spec)
        .unwrap()
        .contains("secret-preset-value"));
    assert!(!format!("{:?}", selected.spec).contains("secret-preset-value"));
    let overrides = RequestOverrides {
        env: std::collections::BTreeMap::from([("KEY".into(), "override-secret".into())]),
        ..Default::default()
    };
    assert!(!format!("{overrides:?}").contains("override-secret"));
}

#[tokio::test]
async fn model_presets_reject_structural_sampling_overrides_before_dispatch() {
    let server = MockServer::start().await;
    let client = AiClient::try_with_proxy_environment(Default::default())
        .unwrap()
        .track_request_dispatch();
    for (name, value) in [
        ("tools", json!([])),
        ("store", json!(true)),
        ("service_tier", json!("priority")),
        ("max_output_tokens", json!(999999)),
        ("temperature", json!(3.0)),
        ("prompt_cache_retention", json!("24h")),
        ("background", json!(true)),
        ("prompt_mode", json!("reasoning")),
        ("functions", json!([{"name":"hidden"}])),
        ("function_call", json!({"name":"hidden"})),
        ("web_search_options", json!({})),
        ("prompt", json!({"id":"stored-prompt"})),
        ("future_structural_control", json!(true)),
    ] {
        let mut selected = model(&server.uri(), Protocol::OpenAiChat);
        Arc::make_mut(&mut selected.spec)
            .preset
            .sampling_params
            .insert(name.into(), value);
        Arc::make_mut(&mut selected.endpoint).auth = Auth::BearerEnv {
            var: "OCTET_PARITY_MISSING_AUTH".into(),
        };
        let error = client.complete(&selected, request()).await.unwrap_err();
        assert!(matches!(error, AiError::Config(_)), "{name}: {error:?}");
        assert!(!client.request_may_have_been_sent());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn string_thinking_preset_controls_request_and_same_model_replay() {
    let server = MockServer::start().await;
    serve(&server, custom_sse(Protocol::OpenAiChat, "ok")).await;
    let mut selected = model(&server.uri(), Protocol::OpenAiChat);
    let spec = Arc::make_mut(&mut selected.spec);
    spec.capabilities.reasoning = Some(reasoning_capability());
    spec.preset.thinking_format = Some(ThinkingFormat::StringThinking);
    spec.preset
        .thinking_level_map
        .insert("high".into(), Some("enabled".into()));
    spec.preset
        .thinking_level_map
        .insert("off".into(), Some("disabled".into()));
    let mut req = request();
    req.reasoning = ReasoningConfig::Effort(ReasoningEffort::High);
    req.messages.push(Message::Assistant(AssistantMessage {
        model: selected.spec.id.clone(),
        protocol: Protocol::OpenAiChat,
        content: vec![
            AssistantPart::Reasoning(ReasoningPart {
                text: Some("prior reasoning".into()),
                state: None,
            }),
            AssistantPart::Text("prior answer".into()),
        ],
    }));
    req.messages.push(Message::User(UserMessage {
        content: vec![UserPart::Text("continue".into())],
    }));
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    client.complete(&selected, req).await.unwrap();
    client.complete(&selected, request()).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["thinking"], "enabled");
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["messages"][1]["reasoning_content"], "prior reasoning");
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["thinking"], "disabled");
}

#[tokio::test]
async fn chat_template_budget_variables_and_omissions_reach_the_wire() {
    let server = MockServer::start().await;
    serve(&server, custom_sse(Protocol::OpenAiChat, "ok")).await;
    let mut selected = model(&server.uri(), Protocol::OpenAiChat);
    let spec = Arc::make_mut(&mut selected.spec);
    spec.limits.max_output_tokens = 8000;
    let mut cap = reasoning_capability();
    cap.control = ReasoningControl::TokenBudget;
    cap.effort_budgets = Some(ReasoningEffortBudgets {
        minimal: 1024,
        low: 1024,
        medium: 2048,
        high: 2048,
        xhigh: 4096,
        max: 4096,
    });
    spec.capabilities.reasoning = Some(cap);
    spec.preset.thinking_format = Some(ThinkingFormat::ChatTemplate);
    spec.preset.thinking_token_budget_field = Some(ThinkingTokenBudgetField::LlamaCpp);
    spec.preset.chat_template_kwargs = Some(
        serde_json::from_value(json!({
            "enabled":{"$var":"thinking.enabled"}, "budget":{"$var":"thinking.budget"},
            "conditional":{"$var":"thinking.enabled", "omitWhenOff":true}, "literal":[1,2]
        }))
        .unwrap(),
    );
    let mut req = request();
    req.reasoning = ReasoningConfig::Budget(2048);
    req.max_output_tokens = Some(5000);
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    client.complete(&selected, req).await.unwrap();
    client.complete(&selected, request()).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let on: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(on["thinking_budget_tokens"], 2048);
    assert_eq!(
        on["chat_template_kwargs"],
        json!({"enabled":true,"budget":2048,"conditional":true,"literal":[1,2]})
    );
    let off: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert!(off.get("thinking_budget_tokens").is_none());
    assert_eq!(
        off["chat_template_kwargs"],
        json!({"enabled":false,"literal":[1,2]})
    );
}

#[tokio::test]
async fn responses_model_preset_can_declare_output_limit_unsupported() {
    let server = MockServer::start().await;
    serve(&server, custom_sse(Protocol::OpenAiResponses, "ok")).await;
    let mut selected = model(&server.uri(), Protocol::OpenAiResponses);
    Arc::make_mut(&mut selected.spec)
        .preset
        .supports_max_output_tokens = Some(false);
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    client.complete(&selected, request()).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(body.get("max_output_tokens").is_none());
}

#[tokio::test]
async fn mistral_declared_profiles_emit_native_chat_controls_and_keep_thinking_replay() {
    let server = MockServer::start().await;
    let wire = sse(&[
        json!({"id":"mistral","choices":[{"delta":{"content":[{"type":"thinking","thinking":[{"type":"text","text":"reasoning"}]}]}}]}),
        json!({"id":"mistral","choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}]}),
    ]) + "data: [DONE]\n\n";
    serve(&server, wire).await;
    let mut selected = model(&server.uri(), Protocol::OpenAiChat);
    Arc::make_mut(&mut selected.endpoint)
        .runtime
        .openai_chat_profile = OpenAiChatRuntimeProfile::Mistral;
    Arc::make_mut(&mut selected.spec).capabilities.reasoning = Some(reasoning_capability());
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    for profile in [
        MistralReasoningProfile::ReasoningEffort,
        MistralReasoningProfile::PromptMode,
    ] {
        Arc::make_mut(&mut selected.spec).preset.mistral_reasoning = Some(profile);
        let mut req = request();
        req.tools.clear();
        req.tool_choice = ToolChoice::Auto;
        req.reasoning = ReasoningConfig::Effort(ReasoningEffort::High);
        let response = client.complete(&selected, req.clone()).await.unwrap();
        assert!(response
            .message
            .content
            .iter()
            .any(|p| matches!(p, AssistantPart::Reasoning(_))));
        req.messages.push(Message::Assistant(response.message));
        req.messages.push(Message::User(UserMessage {
            content: vec![UserPart::Text("continue".into())],
        }));
        client.complete(&selected, req).await.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    let effort: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let prompt: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let replay: Value = serde_json::from_slice(&requests[3].body).unwrap();
    assert_eq!(effort["reasoning_effort"], "high");
    assert!(effort.get("prompt_mode").is_none());
    assert_eq!(prompt["prompt_mode"], "reasoning");
    assert!(prompt.get("reasoning_effort").is_none());
    assert_eq!(replay["messages"][1]["content"][0]["type"], "thinking");
    Arc::make_mut(&mut selected.endpoint)
        .runtime
        .openai_chat_profile = OpenAiChatRuntimeProfile::Default;
    assert!(client.complete(&selected, request()).await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn model_header_secrets_are_redacted_from_provider_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("secret-model-header-value"))
        .mount(&server)
        .await;
    let mut selected = model(&server.uri(), Protocol::OpenAiChat);
    Arc::make_mut(&mut selected.spec)
        .preset
        .headers
        .insert("x-private-model".into(), "secret-model-header-value".into());
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    let error = client.complete(&selected, request()).await.unwrap_err();
    assert!(!format!("{error:?}").contains("secret-model-header-value"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn websocket_prewarm_reuses_only_matching_model_headers_and_authoritative_auth() {
    use futures_util::SinkExt;
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::{
            handshake::server::{Request as UpgradeRequest, Response as UpgradeResponse},
            Message as WsMessage,
        },
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handshakes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = handshakes.clone();
    let server = tokio::spawn(async move {
        let mut handlers = Vec::new();
        for _ in 0..3 {
            let (socket, _) = listener.accept().await.unwrap();
            let captured = captured.clone();
            handlers.push(tokio::spawn(async move {
                let mut websocket = accept_hdr_async(
                    socket,
                    move |req: &UpgradeRequest, response: UpgradeResponse| {
                        captured.lock().unwrap().push((
                            req.headers()["x-model-secret"].to_str().unwrap().to_owned(),
                            req.headers()["authorization"].to_str().unwrap().to_owned(),
                        ));
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                while let Some(Ok(message)) = websocket.next().await {
                    match message {
                        WsMessage::Text(_) => {
                            websocket
                                .send(WsMessage::Text(
                                    json!({"type":"response.created","response":{"id":"preset"}})
                                        .to_string()
                                        .into(),
                                ))
                                .await
                                .unwrap();
                            websocket
                                .send(WsMessage::Text(
                                    tier_terminal(None, true, false).to_string().into(),
                                ))
                                .await
                                .unwrap();
                        }
                        WsMessage::Ping(value) => {
                            websocket.send(WsMessage::Pong(value)).await.unwrap();
                        }
                        WsMessage::Close(_) => break,
                        _ => {}
                    }
                }
            }));
        }
        handlers
    });
    let mut selected = model(&format!("http://{address}/"), Protocol::OpenAiResponses);
    Arc::make_mut(&mut selected.endpoint).transport = EndpointTransport::WebSocketPreferred;
    let preset = &mut Arc::make_mut(&mut selected.spec).preset;
    preset
        .headers
        .insert("x-model-secret".into(), "first-model-value".into());
    preset
        .headers
        .insert("authorization".into(), "Bearer model-attempt".into());
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    let mut req = tier_request(None);
    req.session_id = Some("same-session".into());
    tokio::time::timeout(Duration::from_secs(5), async {
        client
            .prewarm_responses(&selected, req.clone())
            .await
            .unwrap();
        client.complete(&selected, req.clone()).await.unwrap();
        client.complete(&selected, req.clone()).await.unwrap();
        Arc::make_mut(&mut selected.spec)
            .preset
            .headers
            .insert("x-model-secret".into(), "second-model-value".into());
        client.complete(&selected, req.clone()).await.unwrap();
        Arc::make_mut(&mut selected.endpoint).auth = Auth::bearer("rotated-auth");
        client.complete(&selected, req).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        *handshakes.lock().unwrap(),
        vec![
            (
                "first-model-value".to_owned(),
                "Bearer fixture-secret".to_owned()
            ),
            (
                "second-model-value".to_owned(),
                "Bearer fixture-secret".to_owned()
            ),
            (
                "second-model-value".to_owned(),
                "Bearer rotated-auth".to_owned()
            ),
        ]
    );
    for handler in tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
    {
        handler.abort();
    }
}

#[tokio::test]
async fn canonical_stop_beats_preset_and_responses_refuses_chat_only_sampling_before_auth() {
    let server = MockServer::start().await;
    serve(&server, custom_sse(Protocol::OpenAiChat, "ok")).await;
    let mut selected = model(&server.uri(), Protocol::OpenAiChat);
    Arc::make_mut(&mut selected.spec).preset.sampling_params.insert("stop".into(), json!(["preset-stop"]));
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap().track_request_dispatch();
    let mut explicit = request(); explicit.stop = vec!["caller-stop".into()];
    client.complete(&selected, explicit).await.unwrap();
    client.complete(&selected, request()).await.unwrap();
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].body_json::<Value>().unwrap()["stop"], json!(["caller-stop"]));
    assert_eq!(received[1].body_json::<Value>().unwrap()["stop"], json!(["preset-stop"]));
    for (name, value) in [("stop",json!(["bad"])), ("frequency_penalty",json!(0.5)), ("functions",json!([])), ("function_call",json!("auto")), ("web_search_options",json!({})), ("prompt",json!({"id":"hidden"}))] {
        let mut selected = model(&server.uri(), Protocol::OpenAiResponses);
        Arc::make_mut(&mut selected.spec).preset.sampling_params.insert(name.into(), value);
        Arc::make_mut(&mut selected.endpoint).auth = Auth::BearerEnv { var: "OCTET_PARITY_MISSING_AUTH".into() };
        let attempt = client.track_request_dispatch();
        let error = attempt.complete(&selected, request()).await.unwrap_err();
        assert!(matches!(error, AiError::Config(_)), "{name}: {error:?}");
        assert!(!attempt.request_may_have_been_sent());
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}


struct CountingAuth {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    delay: Duration,
}

#[async_trait::async_trait]
impl CredentialResolver for CountingAuth {
    async fn resolve(&self) -> Result<ResolvedCredential, AuthError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if !self.delay.is_zero() { tokio::time::sleep(self.delay).await; }
        Ok(ResolvedCredential { scheme: CredentialScheme::Bearer, value: Secret::from("counted-auth"), extra_headers: Default::default() })
    }
}

fn isolated_azure_env() -> std::collections::BTreeMap<String, String> {
    ["AZURE_OPENAI_BASE_URL", "AZURE_OPENAI_RESOURCE_NAME", "AZURE_OPENAI_API_VERSION", "AZURE_OPENAI_DEPLOYMENT_NAME_MAP"]
        .into_iter().map(|name| (name.to_owned(), String::new())).collect()
}

#[tokio::test]
async fn strict_responses_wire_cap_matches_reservation_contract_including_codex_omission() {
    for profile in [ResponsesRuntimeProfile::Default, ResponsesRuntimeProfile::Codex] {
        for unsupported in [false, true] {
            for cap in [1, 128] {
                let server = MockServer::start().await;
                serve(&server, sse(&[json!({"type":"response.created","response":{"id":"cap"}}), json!({"type":"response.completed","response":{"id":"cap","output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}})])).await;
                let mut selected = model(&server.uri(), Protocol::OpenAiResponses);
                Arc::make_mut(&mut selected.spec).limits.max_output_tokens = 4096;
                Arc::make_mut(&mut selected.spec).preset.supports_max_output_tokens = unsupported.then_some(false);
                Arc::make_mut(&mut selected.endpoint).runtime.responses_profile = profile;
                let expected = if unsupported || profile == ResponsesRuntimeProfile::Codex { None } else { Some(cap) };
                assert_eq!(effective_output_token_cap(&selected, Some(cap)), expected);
                let mut req = tier_request(None); req.max_output_tokens = Some(cap);
                req.compatibility = CompatibilityMode::Strict;
                let overrides = RequestOverrides { sampling_params: std::collections::BTreeMap::from([("top_p".into(), json!(0.8))]), ..Default::default() };
                AiClient::try_with_proxy_environment(Default::default()).unwrap()
                    .complete_with_overrides(&selected, req, overrides).await.unwrap();
                let requests = server.received_requests().await.unwrap();
                assert_eq!(requests.len(), 1);
                let body = requests[0].body_json::<Value>().unwrap();
                assert_eq!(body.get("max_output_tokens").and_then(Value::as_u64), expected);
                assert_eq!(selected.spec.limits.max_output_tokens, 4096);
                assert_eq!(effective_output_token_cap(&selected, Some(cap)), expected);
            }
        }
    }
    for protocol in [Protocol::OpenAiChat, Protocol::OpenAiResponses, Protocol::AnthropicMessages,
        Protocol::GoogleGenerativeAi, Protocol::BedrockConverse, Protocol::MistralConversations] {
        let selected = model("http://127.0.0.1/", protocol);
        assert_eq!(effective_output_token_cap(&selected, Some(128)), Some(128));
        assert_eq!(effective_output_token_cap(&selected, None),
            matches!(protocol, Protocol::AnthropicMessages | Protocol::BedrockConverse).then_some(selected.spec.limits.max_output_tokens));
    }
}

#[tokio::test]
async fn request_overrides_consume_sampling_headers_and_environment_without_mutating_catalog_or_process() {
    let server = MockServer::start().await;
    serve(&server, custom_sse(Protocol::OpenAiChat, "ok")).await;
    let mut selected = model(&server.uri(), Protocol::OpenAiChat);
    let variable = "OCTET_PARITY_REQUEST_AUTH";
    let before = std::env::var_os(variable);
    let endpoint = Arc::make_mut(&mut selected.endpoint);
    endpoint.auth = Auth::bearer_env(variable);
    endpoint.default_headers.insert("x-order", "endpoint".parse().unwrap());
    let preset = &mut Arc::make_mut(&mut selected.spec).preset;
    preset.headers.insert("x-order".into(), "model".into());
    preset.sampling_params = std::collections::BTreeMap::from([
        ("temperature".into(),json!(0.2)), ("top_p".into(),json!(0.3)), ("stop".into(),json!(["model-stop"]))]);
    let original_preset = preset.clone();
    let mut req = request(); req.temperature = Some(0.9); req.stop = vec!["canonical-stop".into()]; req.max_output_tokens = Some(128);
    let overrides = RequestOverrides {
        sampling_params: std::collections::BTreeMap::from([("temperature".into(),json!(0.7)), ("top_p".into(),json!(0.8)), ("stop".into(),json!(["override-stop"]))]),
        headers: std::collections::BTreeMap::from([("X-ORDER".into(),"caller".into()), ("authorization".into(),"Bearer forged".into()), ("content-type".into(),"text/plain".into())]),
        env: std::collections::BTreeMap::from([(variable.into(),"overlay-secret".into())]),
        ..Default::default()
    };
    assert!(!format!("{overrides:?}").contains("overlay-secret"));
    AiClient::try_with_proxy_environment(Default::default()).unwrap().complete_with_overrides(&selected, req, overrides).await.unwrap();
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(),1);
    let body = received[0].body_json::<Value>().unwrap();
    assert!((body["temperature"].as_f64().unwrap()-0.7).abs()<0.0001);
    assert_eq!(body["top_p"],0.8);
    assert_eq!(body["stop"],json!(["canonical-stop"]));
    assert_eq!(body["max_completion_tokens"],128);
    assert_eq!(received[0].headers["x-order"],"caller");
    assert_eq!(received[0].headers["authorization"],"Bearer overlay-secret");
    assert_eq!(received[0].headers["content-type"],"application/json");
    assert_eq!(selected.spec.preset,original_preset);
    assert_eq!(std::env::var_os(variable),before);
}

#[tokio::test]
async fn preset_and_request_sampling_cannot_bypass_tools_or_caps_before_auth_or_dispatch() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut selected = model(&server.uri(),Protocol::OpenAiChat);
    Arc::make_mut(&mut selected.endpoint).auth = Auth::dynamic(Arc::new(CountingAuth { calls: calls.clone(), delay: Duration::ZERO }));
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    for (name,value) in [("functions",json!([])), ("function_call",json!("auto")), ("web_search_options",json!({})),
        ("prompt",json!({"id":"stored"})), ("max_completion_tokens",json!(999999)), ("max_output_tokens",json!(999999)),
        ("supports_max_output_tokens",json!(false)), ("responses_profile",json!("codex")), ("future_control",json!({}))] {
        for as_preset in [false,true] {
            let mut target = selected.clone();
            let mut overrides = RequestOverrides::default();
            if as_preset { Arc::make_mut(&mut target.spec).preset.sampling_params.insert(name.into(),value.clone()); }
            else { overrides.sampling_params.insert(name.into(),value.clone()); }
            let attempt = client.track_request_dispatch();
            assert!(matches!(attempt.complete_with_overrides(&target,request(),overrides).await.unwrap_err(),AiError::Config(_)),"{name}");
            assert!(!attempt.request_may_have_been_sent());
        }
    }
    for overrides in [RequestOverrides { max_retries:Some(1),..Default::default() },
        RequestOverrides { max_retry_delay_ms:Some(1),..Default::default() },
        RequestOverrides { headers:std::collections::BTreeMap::from([("host".into(),"other.invalid".into())]),..Default::default() },
        RequestOverrides { timeout_ms:Some(0),..Default::default() }] {
        let attempt = client.track_request_dispatch();
        assert!(matches!(attempt.complete_with_overrides(&selected,request(),overrides).await.unwrap_err(),AiError::Config(_)));
        assert!(!attempt.request_may_have_been_sent());
    }
    assert_eq!(calls.load(Ordering::SeqCst),0);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn request_local_timeout_cancels_credential_wait_before_dispatch() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut selected = model(&server.uri(),Protocol::OpenAiChat);
    Arc::make_mut(&mut selected.endpoint).auth = Auth::dynamic(Arc::new(CountingAuth {calls:calls.clone(),delay:Duration::from_secs(1)}));
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap().track_request_dispatch();
    let result = tokio::time::timeout(Duration::from_secs(2),client.complete_with_overrides(&selected,request(),RequestOverrides {timeout_ms:Some(20),..Default::default()})).await.unwrap();
    assert!(matches!(result.unwrap_err(),AiError::Transport(ref error) if error.timeout));
    assert_eq!(calls.load(Ordering::SeqCst),1);
    assert!(!client.request_may_have_been_sent());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn request_local_proxy_is_consumed_and_transient_errors_are_redacted_without_retry() {
    let proxy = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(503)
        .insert_header("retry-after","1").set_body_string("override-header-secret overlay-auth-secret"))
        .mount(&proxy).await;
    let variable = "OCTET_PARITY_PROXY_AUTH";
    let mut selected = model("http://target.invalid/v1/",Protocol::OpenAiChat);
    Arc::make_mut(&mut selected.endpoint).auth = Auth::bearer_env(variable);
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    let overrides = RequestOverrides {
        headers:std::collections::BTreeMap::from([("x-private".into(),"override-header-secret".into())]),
        env:std::collections::BTreeMap::from([("HTTP_PROXY".into(),proxy.uri()), (variable.into(),"overlay-auth-secret".into())]),
        max_retries:Some(0), max_retry_delay_ms:Some(0), ..Default::default()
    };
    let error = client.complete_with_overrides(&selected,request(),overrides).await.unwrap_err();
    assert!(matches!(error,AiError::Http(_)));
    assert!(!format!("{error:?}").contains("override-header-secret"));
    assert!(!format!("{error:?}").contains("overlay-auth-secret"));
    let received = proxy.received_requests().await.unwrap(); assert_eq!(received.len(),1);
    assert_eq!(received[0].headers["host"],"target.invalid");
}

#[tokio::test]
async fn azure_overrides_select_deployment_version_and_explicit_destination_on_the_wire() {
    let original = MockServer::start().await;
    let destination = MockServer::start().await;
    serve(&destination,sse(&[json!({"type":"response.created","response":{"id":"azure"}}),tier_terminal(None,true,false)])).await;
    let mut selected = model(&original.uri(),Protocol::OpenAiResponses);
    let endpoint = Arc::make_mut(&mut selected.endpoint);
    endpoint.runtime.responses_profile = ResponsesRuntimeProfile::Azure;
    endpoint.auth = Auth::header_env("api-key".parse().unwrap(),"AZURE_OPENAI_API_KEY");
    let original_url = endpoint.base_url.clone();
    let original_api = selected.spec.api_name.clone();
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    for (explicit,map,expected) in [(Some("deploy/with ?#\""),Some("mapped"),"deploy/with ?#\""),
        (None,Some("mapped"),"mapped"),(None,None,"from-env")] {
        let mut env = isolated_azure_env();
        env.insert("AZURE_OPENAI_API_KEY".into(),"azure-secret".into());
        env.insert("AZURE_OPENAI_API_VERSION".into(),"environment-version".into());
        env.insert("AZURE_OPENAI_DEPLOYMENT_NAME_MAP".into(),"fixture=from-env".into());
        let azure = AzureRequestOptions {
            deployment_name:explicit.map(str::to_owned),
            deployment_map:map.map(|value|std::collections::BTreeMap::from([("fixture".into(),value.into())])).unwrap_or_default(),
            base_url:Some(format!("{}/gateway%20path/",destination.uri())),
            api_version:Some("v1-explicit".into()),..Default::default()
        };
        client.complete_with_overrides(&selected,tier_request(None),RequestOverrides {azure:Some(azure),env,..Default::default()}).await.unwrap();
        let received = destination.received_requests().await.unwrap();
        let request = received.last().unwrap();
        assert_eq!(request.url.path(),"/gateway%20path/responses");
        assert_eq!(request.url.query(),Some("api-version=v1-explicit"));
        assert_eq!(request.headers["api-key"],"azure-secret");
        let body = request.body_json::<Value>().unwrap(); assert_eq!(body["model"],expected);
        assert_eq!(body["store"],false);
    }
    assert!(original.received_requests().await.unwrap().is_empty());
    assert_eq!(selected.endpoint.base_url,original_url);
    assert_eq!(selected.spec.api_name,original_api);
}

#[tokio::test]
async fn azure_environment_cannot_forward_credentials_to_an_unselected_origin() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let origin = MockServer::start().await; let other = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut selected = model(&origin.uri(),Protocol::OpenAiResponses);
    let endpoint = Arc::make_mut(&mut selected.endpoint);
    endpoint.runtime.responses_profile = ResponsesRuntimeProfile::Azure;
    endpoint.auth = Auth::dynamic(Arc::new(CountingAuth {calls:calls.clone(),delay:Duration::ZERO}));
    let client = AiClient::try_with_proxy_environment(Default::default()).unwrap();
    for resource in [None,Some("unselected-explicit-resource".to_owned())] {
        let mut env = isolated_azure_env(); env.insert("AZURE_OPENAI_BASE_URL".into(),other.uri());
        let attempt = client.track_request_dispatch();
        let error = attempt.complete_with_overrides(&selected,tier_request(None),RequestOverrides {
            env, azure:Some(AzureRequestOptions {resource_name:resource,..Default::default()}),..Default::default()
        }).await.unwrap_err();
        assert!(matches!(error,AiError::Config(_))); assert!(!attempt.request_may_have_been_sent());
    }
    for (map,base) in [(Some("fixture=one,fixture=two"),None),
        (None,Some("https://user:private-secret@resource.openai.azure.com/")),
        (None,Some("https://resource.openai.azure.com/?token=private-secret")),
        (None,Some("https://resource.openai.azure.com/?api-version=v1&api-version=v2"))] {
        let mut env = isolated_azure_env();
        if let Some(map) = map { env.insert("AZURE_OPENAI_DEPLOYMENT_NAME_MAP".into(),map.into()); }
        let overrides = RequestOverrides { env, azure:Some(AzureRequestOptions {base_url:base.map(str::to_owned),..Default::default()}),..Default::default() };
        assert!(!format!("{overrides:?}").contains("private-secret"));
        let attempt = client.track_request_dispatch();
        let error = attempt.complete_with_overrides(&selected,tier_request(None),overrides).await.unwrap_err();
        assert!(matches!(error,AiError::Config(_))); assert!(!attempt.request_may_have_been_sent());
        assert!(!format!("{error:?}").contains("private-secret"));
    }
    assert_eq!(calls.load(Ordering::SeqCst),0);
    assert!(origin.received_requests().await.unwrap().is_empty());
    assert!(other.received_requests().await.unwrap().is_empty());
}


#[tokio::test]
async fn request_local_body_deadline_and_stream_drop_close_the_single_provider_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for exhaust_deadline in [false,true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket,_) = listener.accept().await.unwrap();
            let mut request = Vec::new(); let mut chunk = [0u8;4096];
            let header_end = loop {
                let n = socket.read(&mut chunk).await.unwrap(); assert!(n>0);
                request.extend_from_slice(&chunk[..n]); assert!(request.len()<64*1024);
                if let Some(index) = request.windows(4).position(|v|v==b"\r\n\r\n") { break index+4; }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let length:usize = headers.lines().find_map(|line|line.strip_prefix("content-length:")).unwrap().trim().parse().unwrap();
            while request.len()<header_end+length {
                let n = socket.read(&mut chunk).await.unwrap(); assert!(n>0); request.extend_from_slice(&chunk[..n]);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000\r\n\r\n").await.unwrap();
            socket.write_all(b"data: {\"id\":\"hold\",\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n").await.unwrap();
            matches!(tokio::time::timeout(Duration::from_secs(2),socket.read(&mut chunk)).await,Ok(Ok(0))|Ok(Err(_)))
        });
        let selected = model(&format!("http://{address}/"),Protocol::OpenAiChat);
        let client = AiClient::try_with_proxy_environment(Default::default()).unwrap().track_request_dispatch();
        let mut stream = client.stream_with_overrides(&selected,request(),RequestOverrides {timeout_ms:Some(250),..Default::default()}).await.unwrap();
        assert!(matches!(stream.next().await.unwrap().unwrap(),StreamEvent::Started{..}));
        if exhaust_deadline {
            let error = tokio::time::timeout(Duration::from_secs(2),async {
                loop { if let Err(error) = stream.next().await.expect("deadline must report a terminal error") { break error; } }
            }).await.unwrap();
            // The fixture emits one consumer-visible delta, so a failed body
            // deadline is annotated with bounded stream progress. The
            // underlying cause must still be the body timeout.
            let transport = match &error {
                AiError::Transport(transport) => transport,
                AiError::StreamFailure { inner, progress } => {
                    assert!(progress.content_bytes > 0, "{progress:?}");
                    match inner.as_ref() {
                        AiError::Transport(transport) => transport,
                        other => panic!("expected a transport cause: {other:?}"),
                    }
                }
                other => panic!("expected a body deadline: {other:?}"),
            };
            assert!(transport.timeout && transport.phase == TransportPhase::Body, "{transport:?}");
        }
        drop(stream);
        assert!(client.request_may_have_been_sent());
        assert!(tokio::time::timeout(Duration::from_secs(3),server).await.unwrap().unwrap());
    }
}

#[tokio::test]
async fn request_signer_receives_final_override_headers_body_and_unchanged_cap() {
    struct Signer(Arc<std::sync::Mutex<Vec<(http::HeaderMap,Vec<u8>)>>>);
    #[async_trait::async_trait]
    impl RequestSigner for Signer {
        async fn sign(&self, request:&SigningRequest)->Result<SignedRequestHeaders,AuthError> {
            self.0.lock().unwrap().push((request.headers().clone(),request.body().to_vec()));
            let mut headers = http::HeaderMap::new(); headers.insert("authorization","signed-authority".parse().unwrap());
            Ok(SignedRequestHeaders::new(headers,vec![Secret::from("signed-authority")]))
        }
    }
    let server = MockServer::start().await; serve(&server,custom_sse(Protocol::OpenAiChat,"ok")).await;
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut selected = model(&server.uri(),Protocol::OpenAiChat);
    Arc::make_mut(&mut selected.endpoint).auth = Auth::request_signer(Arc::new(Signer(captured.clone())));
    Arc::make_mut(&mut selected.spec).preset.headers.insert("x-order".into(),"model".into());
    let mut req = request(); req.max_output_tokens=Some(128);
    let overrides = RequestOverrides { headers:std::collections::BTreeMap::from([("x-order".into(),"caller".into()),("authorization".into(),"forged".into())]),
        sampling_params:std::collections::BTreeMap::from([("top_p".into(),json!(0.8))]),..Default::default() };
    AiClient::try_with_proxy_environment(Default::default()).unwrap().complete_with_overrides(&selected,req,overrides).await.unwrap();
    let received=server.received_requests().await.unwrap(); assert_eq!(received.len(),1);
    assert_eq!(received[0].headers["authorization"],"signed-authority");
    let captured=captured.lock().unwrap(); assert_eq!(captured.len(),1);
    assert_eq!(captured[0].0["x-order"],"caller"); assert_eq!(captured[0].1,received[0].body);
    let body:Value=serde_json::from_slice(&captured[0].1).unwrap();
    assert_eq!(body["top_p"],0.8); assert_eq!(body["max_completion_tokens"],128);
}
