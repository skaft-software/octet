//! Unit tests for `crate::protocol::anthropic`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `anthropic.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::anthropic`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::catalog::Model;
use crate::types::{
    Capabilities, Endpoint, EndpointId, ImageMedia, ImageSource, Media, Message, ModalitySet,
    ModelId, ModelLimits, ModelSpec, OutputFormat, OutputModalities, ReasoningConfig,
    ReasoningPart, Request, ToolChoice, ToolDef, UserMessage, UserPart,
};
use crate::CompatibilityMode;
use std::sync::Arc;

fn make_test_model(reasoning: bool) -> Model {
    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("test-claude".to_string()),
        endpoint: EndpointId("anthropic-ep".to_string()),
        api_name: "claude-3-5-sonnet".to_string(),
        display_name: None,
        protocol: Protocol::AnthropicMessages,
        capabilities: Capabilities {
            responses_features: Default::default(),
            input_modalities: ModalitySet::none().with(crate::types::Modality::Image),
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: true,
            reasoning: if reasoning {
                Some(crate::types::ReasoningCapability {
                    options: None,
                    control: crate::types::ReasoningControl::TokenBudget,
                    exposes_text: true,
                    preserves_state: true,
                    effort_budgets: Some(crate::types::ReasoningEffortBudgets {
                        minimal: 1024,
                        low: 2048,
                        medium: 4096,
                        high: 8192,
                        xhigh: 16384,
                        max: 32768,
                    }),
                    openai_chat_mode: crate::types::OpenAiChatReasoningMode::Standard,
                    min_effort: crate::types::ReasoningEffort::Minimal,
                    max_effort: crate::types::ReasoningEffort::High,
                })
            } else {
                None
            },
            responses_lite: false,
            agent_delegation: None,
            structured_output: true,
            deferred_tool_loading: false,
        },
        limits: ModelLimits {
            context_window: 200000,
            max_output_tokens: 8192,
        },
        pricing: None,
        cache: crate::types::CacheCompatibility::default(),
    };

    let ep = Endpoint {
        id: EndpointId("anthropic-ep".to_string()),
        base_url: url::Url::parse("https://api.anthropic.com/v1/").unwrap(),
        auth: crate::auth::Auth::none(),
        default_headers: http::HeaderMap::new(),
        transport: crate::types::EndpointTransport::Http,
        runtime: crate::types::RequestRuntime::default(),
        timeout: std::time::Duration::from_secs(30),
    };

    Model {
        spec: Arc::new(spec),
        endpoint: Arc::new(ep),
    }
}

#[test]
fn test_build_request_anthropic_basic() {
    let model = make_test_model(false);
    let req = Request {
        system: Some("System instructions".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Hello".to_string())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(1000),
        temperature: Some(0.5),
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::None,
        session_id: None,
    };

    let parts = build_request(&model, &req).unwrap();
    assert_eq!(
        parts.url.to_string(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        parts
            .headers
            .get("anthropic-version")
            .unwrap()
            .to_str()
            .unwrap(),
        "2023-06-01"
    );

    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["model"], "claude-3-5-sonnet");
    assert_eq!(body["max_tokens"], 1000);
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["system"][0]["text"], "System instructions");
    assert!(body["system"][0].get("cache_control").is_none());
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][0]["text"], "Hello");
}

#[test]
fn caller_anthropic_beta_list_is_authoritative_and_deduplicated() {
    let mut model = make_test_model(false);
    {
        let endpoint = Arc::make_mut(&mut model.endpoint);
        // Repeated headers and comma-joined values both occur in practice;
        // the caller's list replaces inferred defaults and is deduplicated.
        endpoint.default_headers.append(
            http::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_static(
                "fine-grained-tool-streaming-2025-05-14, interleaved-thinking-2025-05-14",
            ),
        );
        endpoint.default_headers.append(
            http::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_static("fine-grained-tool-streaming-2025-05-14"),
        );
    }
    let req = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Hello".to_string())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::None,
        session_id: None,
    };
    let parts = build_request(&model, &req).unwrap();
    assert_eq!(
        parts
            .headers
            .get("anthropic-beta")
            .unwrap()
            .to_str()
            .unwrap(),
        "fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14"
    );
}

/// A minimal Anthropic Messages request for beta-header tests.
fn beta_test_request(reasoning: ReasoningConfig) -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Hello".to_string())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::None,
        session_id: None,
    }
}

fn beta_header(parts: &crate::protocol::HttpRequestParts) -> Option<String> {
    parts
        .headers
        .get("anthropic-beta")
        .map(|value| value.to_str().unwrap().to_owned())
}

#[test]
fn oauth_routes_infer_the_claude_code_betas() {
    // Upstream `getBetaFeatures`: an OAuth/subscription token implies the
    // Claude Code betas. The rule keys on the declared credential variable,
    // so it is data, not a provider-name branch.
    for var in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_OAUTH_TOKEN"] {
        let mut model = make_test_model(false);
        Arc::make_mut(&mut model.endpoint).auth = crate::auth::Auth::BearerEnv {
            var: var.to_string(),
        };
        let parts = build_request(&model, &beta_test_request(ReasoningConfig::Off)).unwrap();
        assert_eq!(
            beta_header(&parts).as_deref(),
            Some("claude-code-20250219,oauth-2025-04-20"),
            "{var} must select the OAuth betas"
        );
    }

    // An ordinary API-key route infers nothing: the beta header is absent
    // rather than carrying an empty value.
    let mut model = make_test_model(false);
    Arc::make_mut(&mut model.endpoint).auth = crate::auth::Auth::header_env(
        http::HeaderName::from_static("x-api-key"),
        "ANTHROPIC_API_KEY",
    );
    let parts = build_request(&model, &beta_test_request(ReasoningConfig::Off)).unwrap();
    assert_eq!(beta_header(&parts), None);
}

#[test]
fn extended_thinking_infers_the_interleaved_thinking_beta_only_when_enabled() {
    use crate::types::ReasoningControl;

    // Budget-controlled model: an effort selection becomes extended thinking.
    // (`Medium` keeps the derived 4096-token budget under the fixture's
    // 8192-token output limit; `High` would equal it and fail validation.)
    let extended = make_test_model(true);
    let parts = build_request(
        &extended,
        &beta_test_request(ReasoningConfig::Effort(
            crate::types::ReasoningEffort::Medium,
        )),
    )
    .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(
        beta_header(&parts).as_deref(),
        Some("interleaved-thinking-2025-05-14"),
        "extended thinking must pair with the interleaved-thinking beta"
    );

    // Adaptive (effort) thinking is interleaved by construction: upstream
    // suppresses the beta when the route forces adaptive thinking.
    let mut adaptive = make_test_model(true);
    Arc::make_mut(&mut adaptive.spec)
        .capabilities
        .reasoning
        .as_mut()
        .expect("the fixture model declares reasoning")
        .control = ReasoningControl::Effort;
    let parts = build_request(
        &adaptive,
        &beta_test_request(ReasoningConfig::Effort(
            crate::types::ReasoningEffort::Medium,
        )),
    )
    .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(beta_header(&parts), None);

    // Thinking disabled: no beta.
    let parts = build_request(&extended, &beta_test_request(ReasoningConfig::Off)).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(beta_header(&parts), None);
}

#[test]
fn an_explicit_caller_beta_list_still_replaces_inferred_features() {
    let mut model = make_test_model(false);
    {
        let endpoint = Arc::make_mut(&mut model.endpoint);
        endpoint.auth = crate::auth::Auth::BearerEnv {
            var: "ANTHROPIC_OAUTH_TOKEN".to_string(),
        };
        endpoint.default_headers.insert(
            http::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_static("caller-feature-2026-01-01"),
        );
    }
    let parts = build_request(&model, &beta_test_request(ReasoningConfig::Off)).unwrap();
    assert_eq!(
        beta_header(&parts).as_deref(),
        Some("caller-feature-2026-01-01"),
        "an explicit caller list stays authoritative and is never merged with inferred betas"
    );
}

#[test]
fn cache_retention_controls_anthropic_wire_markers() {
    let mut model = make_test_model(false);
    let mut req = Request {
        system: Some("stable system".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("stable user".to_string())],
        })],
        tools: vec![crate::types::ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "lookup".to_string(),
            description: "lookup".to_string(),
            parameters: serde_json::json!({"type":"object"}),
        }],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: Some("session-123".to_string()),
    };

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");

    req.cache_retention = crate::types::CacheRetention::Long;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");

    req.cache_retention = crate::types::CacheRetention::None;
    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["system"][0]["text"], "stable system");
    assert!(body["system"][0].get("cache_control").is_none());
    assert!(body["messages"][0]["content"][0]
        .get("cache_control")
        .is_none());
    assert!(body["tools"][0].get("cache_control").is_none());
    assert!(parts.headers.get("x-session-affinity").is_none());

    Arc::make_mut(&mut model.spec)
        .cache
        .send_session_affinity_headers = true;
    Arc::make_mut(&mut model.endpoint).id = EndpointId("opencode-anthropic".into());
    req.session_id = Some("zen-session".into());
    let parts = build_request(&model, &req).unwrap();
    assert_eq!(parts.headers["x-opencode-session"], "zen-session");
    assert!(parts.headers.get("x-session-affinity").is_none());
    req.session_id = None;
    let parts = build_request(&model, &req).unwrap();
    assert!(parts.headers.get("x-opencode-session").is_none());
}

#[test]
fn warm_breakpoint_covers_reusable_canonical_prefix_not_synthetic_suffix() {
    let model = make_test_model(false);
    let mut req = Request {
        system: Some("stable system".into()),
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("prior user".into())],
            }),
            Message::Assistant(crate::types::AssistantMessage {
                content: vec![AssistantPart::Text("prior answer".into())],
                model: model.spec.id.clone(),
                protocol: Protocol::AnthropicMessages,
            }),
        ],
        tools: vec![ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "lookup".into(),
            description: "lookup".into(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(1),
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::WarmShort,
        session_id: Some("same-session".into()),
    };
    req.messages.push(Message::User(UserMessage {
        content: vec![UserPart::Text("Reply with a single period.".into())],
    }));
    let warm = build_request(&model, &req).unwrap();
    let warm_body: serde_json::Value = serde_json::from_slice(&warm.body).unwrap();
    req.cache_retention = CacheRetention::Short;
    req.max_output_tokens = Some(100);
    req.messages.pop();
    req.messages.push(Message::User(UserMessage {
        content: vec![UserPart::Text("real follow-up".into())],
    }));
    let follow_up = build_request(&model, &req).unwrap();
    let follow_up_body: serde_json::Value = serde_json::from_slice(&follow_up.body).unwrap();

    assert_eq!(warm_body["system"], follow_up_body["system"]);
    assert_eq!(warm_body["tools"], follow_up_body["tools"]);
    let mut warm_prefix = warm_body["messages"].as_array().unwrap()[..2].to_vec();
    let follow_up_prefix = &follow_up_body["messages"].as_array().unwrap()[..2];
    assert_eq!(
        warm_prefix[1]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    warm_prefix[1]["content"][0]
        .as_object_mut()
        .unwrap()
        .remove("cache_control");
    assert_eq!(&warm_prefix, follow_up_prefix);
    assert!(warm_body["messages"][2]["content"][0]
        .get("cache_control")
        .is_none());
    assert_eq!(
        follow_up_body["messages"][2]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn test_build_request_anthropic_url_image_and_structured_output() {
    let model = make_test_model(false);
    let req = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Media(Media::image_url(
                url::Url::parse("https://example.test/image.png").unwrap(),
                None,
            ))],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::JsonSchema(crate::types::JsonSchemaFormat {
            name: "answer".to_string(),
            description: None,
            schema: serde_json::json!({"type":"object"}),
            strict: true,
        }),
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["messages"][0]["content"][0]["source"]["type"], "url");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(body["output_config"]["format"]["schema"]["type"], "object");
}

#[test]
fn test_build_request_anthropic_thinking_replay() {
    let model = make_test_model(true);
    let state = ReasoningState {
        model: ModelId("test-claude".to_string()),
        protocol: Protocol::AnthropicMessages,
        kind: ReasoningStateKind::AnthropicSignature {
            signature: "test-sig".to_string(),
        },
    };

    let req = Request {
        system: None,
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("Hello".to_string())],
            }),
            Message::Assistant(crate::types::AssistantMessage {
                content: vec![
                    AssistantPart::Reasoning(ReasoningPart {
                        text: Some("Thinking content".to_string()),
                        state: Some(state),
                    }),
                    AssistantPart::Text("Final answer".to_string()),
                ],
                model: ModelId("test-claude".to_string()),
                protocol: Protocol::AnthropicMessages,
            }),
        ],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Effort(crate::types::ReasoningEffort::Medium),
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["thinking"]["budget_tokens"], 4096);

    let assistant_msg = &body["messages"][1];
    assert_eq!(assistant_msg["role"], "assistant");
    assert_eq!(assistant_msg["content"][0]["type"], "thinking");
    assert_eq!(assistant_msg["content"][0]["thinking"], "Thinking content");
    assert_eq!(assistant_msg["content"][0]["signature"], "test-sig");
    assert_eq!(assistant_msg["content"][1]["type"], "text");
    assert_eq!(assistant_msg["content"][1]["text"], "Final answer");
}

#[test]
fn test_build_request_anthropic_effort_adaptive_max() {
    // An effort-controlled Anthropic model emits adaptive thinking plus
    // `output_config.effort`, never `budget_tokens`.
    let base = make_test_model(true);
    let mut spec = (*base.spec).clone();
    let cap = spec.capabilities.reasoning.as_mut().unwrap();
    cap.control = crate::types::ReasoningControl::Effort;
    cap.effort_budgets = None;
    cap.max_effort = crate::types::ReasoningEffort::Max;
    let model = Model {
        spec: Arc::new(spec),
        endpoint: base.endpoint.clone(),
    };

    let req = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Hi".to_string())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Effort(crate::types::ReasoningEffort::Max),
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert!(body["thinking"].get("budget_tokens").is_none());
    assert_eq!(body["output_config"]["effort"], "max");
    // No structured-output format requested, so `format` is omitted.
    assert!(body["output_config"].get("format").is_none());
}

#[test]
fn test_decode_stream_event_anthropic() {
    let model = make_test_model(false);
    let mut builder =
        ResponseBuilder::new(ModelId("m".to_string()), Protocol::AnthropicMessages, None);

    let sse_start = SseEvent {
        event: Some("message_start".to_string()),
        data: r#"{"type": "message_start", "message": {"id": "msg-123", "usage": {"input_tokens": 10, "output_tokens": 0}}}"#.to_string(),
    };

    let evs = decode_stream_event(&model, &sse_start, &mut builder).unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEvent::Started { .. }));
}

#[test]
fn oversized_signature_is_rejected_before_buffer_growth() {
    let model = make_test_model(true);
    let mut builder =
        ResponseBuilder::new(ModelId("m".to_string()), Protocol::AnthropicMessages, None);
    builder
        .reserve_buffered_content(crate::stream::MAX_RESPONSE_CONTENT_BYTES)
        .unwrap();
    let signature = SseEvent {
        event: Some("content_block_delta".to_string()),
        data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"x"}}"#
            .to_string(),
    };

    let error = decode_stream_event(&model, &signature, &mut builder).unwrap_err();
    assert!(matches!(
        error,
        AiError::Decode(DecodeError::ResponseTooLarge)
    ));
    assert!(!builder.temp_buffers.contains_key("sig_0"));
}

#[test]
fn test_build_request_image_input() {
    let model = make_test_model(false);

    let inline_image = Media::Image(ImageMedia {
        source: ImageSource::Inline(bytes::Bytes::from(vec![0x47, 0x49, 0x46])),
        media_type: Some(mime::IMAGE_GIF),
        detail: None,
    });

    let url_image = Media::Image(ImageMedia {
        source: ImageSource::Url(url::Url::parse("https://example.com/test.png").unwrap()),
        media_type: None,
        detail: None,
    });

    let req = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Media(inline_image), UserPart::Media(url_image)],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    let parts = build_request(&model, &req).unwrap();
    let body_val: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();

    let messages = body_val["messages"].as_array().unwrap();
    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);

    assert_eq!(content[0]["type"], "image");
    let source0 = &content[0]["source"];
    assert_eq!(source0["type"], "base64");
    assert_eq!(source0["media_type"], "image/gif");
    assert_eq!(source0["data"], "R0lG");

    assert_eq!(content[1]["type"], "image");
    let source1 = &content[1]["source"];
    assert_eq!(source1["type"], "url");
    assert_eq!(source1["url"], "https://example.com/test.png");
}

fn compat_request(tool_choice: ToolChoice) -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".to_string())],
        })],
        tools: vec![ToolDef {
            async_execution: false,
            name: "lookup".to_string(),
            description: "Look up a city.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
            constrained_sampling: None,
        }],
        tool_choice,
        max_output_tokens: Some(8192),
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    }
}

fn compat_build(model: &Model, req: &Request) -> (serde_json::Value, http::HeaderMap) {
    let parts = build_request(model, req).unwrap();
    (serde_json::from_slice(&parts.body).unwrap(), parts.headers)
}

fn compat_beta_header(headers: &http::HeaderMap) -> String {
    headers
        .get("anthropic-beta")
        .map(|value| value.to_str().unwrap().to_string())
        .unwrap_or_default()
}

#[test]
fn eager_tool_streaming_defaults_on_and_a_declared_false_uses_the_legacy_beta() {
    let model = make_test_model(false);
    let (body, headers) = compat_build(&model, &compat_request(ToolChoice::Auto));
    assert_eq!(body["tools"][0]["eager_input_streaming"], true);
    assert!(!compat_beta_header(&headers).contains("fine-grained-tool-streaming"));

    let mut legacy = make_test_model(false);
    Arc::make_mut(&mut legacy.spec).preset.anthropic_compat =
        Some(crate::declarations::AnthropicCompatPreset {
            supports_eager_tool_input_streaming: Some(false),
            ..Default::default()
        });
    let (body, headers) = compat_build(&legacy, &compat_request(ToolChoice::Auto));
    assert!(body["tools"][0].get("eager_input_streaming").is_none());
    assert!(compat_beta_header(&headers).contains("fine-grained-tool-streaming-2025-05-14"));
    // A request without tools never needs the legacy per-tool beta.
    let mut no_tools = compat_request(ToolChoice::Auto);
    no_tools.tools.clear();
    let (_body, headers) = compat_build(&legacy, &no_tools);
    assert!(!compat_beta_header(&headers).contains("fine-grained-tool-streaming"));
}

#[test]
fn declared_fallback_models_emit_the_wire_list_and_its_beta() {
    let model = make_test_model(false);
    let (_body, headers) = compat_build(&model, &compat_request(ToolChoice::Auto));
    assert!(!compat_beta_header(&headers).contains("server-side-fallback"));

    let mut fallback = make_test_model(false);
    Arc::make_mut(&mut fallback.spec).preset.anthropic_compat =
        Some(crate::declarations::AnthropicCompatPreset {
            allowed_fallback_models: vec![crate::declarations::AnthropicFallbackModel {
                provider: "anthropic".to_string(),
                model: "claude-haiku-4-5".to_string(),
                cost: Some(crate::declarations::AnthropicFallbackCost {
                    input: 1.0,
                    output: 5.0,
                    cache_read: 0.1,
                    cache_write: 1.25,
                }),
            }],
            ..Default::default()
        });
    let (body, headers) = compat_build(&fallback, &compat_request(ToolChoice::Auto));
    assert_eq!(
        body["fallbacks"],
        serde_json::json!([{"model": "claude-haiku-4-5"}])
    );
    assert!(compat_beta_header(&headers).contains("server-side-fallback-2026-07-01"));
    // Never an empty array: Anthropic rejects the field with no target.
    let (body, _headers) = compat_build(&make_test_model(false), &compat_request(ToolChoice::Auto));
    assert!(body.get("fallbacks").is_none());
}

#[test]
fn force_adaptive_thinking_suppresses_the_interleaved_beta() {
    let mut req = compat_request(ToolChoice::Auto);
    req.reasoning = ReasoningConfig::Budget(4096);
    let model = make_test_model(true);
    let (_body, headers) = compat_build(&model, &req);
    assert!(compat_beta_header(&headers).contains("interleaved-thinking"));

    let mut forced = make_test_model(true);
    Arc::make_mut(&mut forced.spec).preset.anthropic_compat =
        Some(crate::declarations::AnthropicCompatPreset {
            force_adaptive_thinking: Some(true),
            ..Default::default()
        });
    let (_body, headers) = compat_build(&forced, &req);
    assert!(!compat_beta_header(&headers).contains("interleaved-thinking"));
}

#[test]
fn declared_mid_conversation_effort_appends_the_effort_system_message() {
    let mut req = compat_request(ToolChoice::Auto);
    req.reasoning = ReasoningConfig::Budget(4096);
    let model = make_test_model(true);
    let (body, headers) = compat_build(&model, &req);
    assert!(body.get("fallbacks").is_none());
    assert!(!compat_beta_header(&headers).contains("mid-conversation-output-config"));
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);

    let mut mid = make_test_model(true);
    Arc::make_mut(&mut mid.spec).preset.anthropic_compat =
        Some(crate::declarations::AnthropicCompatPreset {
            supports_mid_convo_effort: Some(true),
            ..Default::default()
        });
    let (body, headers) = compat_build(&mid, &req);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    // Budget thinking has no adaptive effort, so Pi's active-effort default
    // ("high") is used rather than inventing a tier.
    assert_eq!(
        messages[1],
        serde_json::json!({"role": "system", "content": [], "output_config": {"effort": "high"}})
    );
    assert!(compat_beta_header(&headers).contains("mid-conversation-output-config-2026-07-01"));
    assert!(compat_beta_header(&headers).contains("thinking-binding-controls-2026-08-01"));

    // Adaptive effort models carry their mapped level.
    let mut adaptive = mid.clone();
    let adaptive_reasoning = Arc::make_mut(&mut adaptive.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    adaptive_reasoning.control = crate::types::ReasoningControl::Effort;
    adaptive_reasoning.effort_budgets = None;
    adaptive_reasoning.max_effort = crate::types::ReasoningEffort::Xhigh;
    let mut effort_req = compat_request(ToolChoice::Auto);
    effort_req.reasoning = ReasoningConfig::Effort(crate::types::ReasoningEffort::Xhigh);
    let (body, _headers) = compat_build(&adaptive, &effort_req);
    assert_eq!(body["output_config"]["effort"], "xhigh");
    assert_eq!(
        body["messages"].as_array().unwrap().last().unwrap()["output_config"]["effort"],
        "xhigh"
    );
}

#[test]
fn empty_thinking_signatures_replay_as_text_unless_the_route_declares_them() {
    let model = make_test_model(false);
    let empty_state = ReasoningState {
        model: ModelId("test-claude".to_string()),
        protocol: Protocol::AnthropicMessages,
        kind: ReasoningStateKind::AnthropicSignature {
            signature: String::new(),
        },
    };
    let request = || Request {
        system: None,
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("Hello".to_string())],
            }),
            Message::Assistant(crate::types::AssistantMessage {
                content: vec![AssistantPart::Reasoning(ReasoningPart {
                    text: Some("Interrupted thinking".to_string()),
                    state: Some(empty_state.clone()),
                })],
                model: ModelId("test-claude".to_string()),
                protocol: Protocol::AnthropicMessages,
            }),
        ],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };
    let (body, _headers) = compat_build(&model, &request());
    // Pi's default: an empty signature becomes plain text for Anthropic.
    assert_eq!(body["messages"][1]["content"][0]["type"], "text");
    assert_eq!(
        body["messages"][1]["content"][0]["text"],
        "Interrupted thinking"
    );

    let mut declaring = make_test_model(false);
    Arc::make_mut(&mut declaring.spec).preset.anthropic_compat =
        Some(crate::declarations::AnthropicCompatPreset {
            allow_empty_signature: Some(true),
            ..Default::default()
        });
    let (body, _headers) = compat_build(&declaring, &request());
    assert_eq!(body["messages"][1]["content"][0]["type"], "thinking");
    assert_eq!(body["messages"][1]["content"][0]["signature"], "");

    // A fully empty thinking block is never sent at all.
    let mut silent = request();
    silent.messages[1] = Message::Assistant(crate::types::AssistantMessage {
        content: vec![AssistantPart::Reasoning(ReasoningPart {
            text: Some("   ".to_string()),
            state: Some(empty_state),
        })],
        model: ModelId("test-claude".to_string()),
        protocol: Protocol::AnthropicMessages,
    });
    let (body, _headers) = compat_build(&model, &silent);
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
}
