//! Unit tests for `crate::protocol::openai_chat`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `openai_chat.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::openai_chat`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::catalog::Model;
use crate::types::{
    AssistantMessage, AssistantPart, AudioFormat, AudioOutputOptions, Capabilities, Endpoint,
    EndpointId, ImageDetail, ImageMedia, ImageSource, Media, Message, ModalitySet, ModelId,
    ModelLimits, ModelSpec, OpenAiChatReasoningMode, OutputFormat, OutputModalities,
    ReasoningConfig, ReasoningEffort, ReasoningPart, Request, ToolCall, ToolCallId, ToolChoice,
    UserMessage, UserPart,
};
use crate::CompatibilityMode;
use std::sync::Arc;

fn make_test_model(
    image: bool,
    audio_in: bool,
    audio_out: bool,
    tools: bool,
    reasoning: bool,
    structured: bool,
) -> Model {
    let mut input = ModalitySet::none();
    if image {
        input = input.with(crate::types::Modality::Image);
    }
    if audio_in {
        input = input.with(crate::types::Modality::Audio);
    }

    let mut output = ModalitySet::none();
    if audio_out {
        output = output.with(crate::types::Modality::Audio);
    }

    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("test-model".to_string()),
        endpoint: EndpointId("test-ep".to_string()),
        api_name: "gpt-4-test".to_string(),
        display_name: None,
        protocol: Protocol::OpenAiChat,
        capabilities: Capabilities {
            responses_features: Default::default(),
            input_modalities: input,
            output_modalities: output,
            tools,
            parallel_tool_calls: tools,
            reasoning: if reasoning {
                Some(crate::types::ReasoningCapability {
                    options: None,
                    control: crate::types::ReasoningControl::Effort,
                    exposes_text: true,
                    preserves_state: true,
                    effort_budgets: None,
                    openai_chat_mode: crate::types::OpenAiChatReasoningMode::Standard,
                    min_effort: crate::types::ReasoningEffort::Minimal,
                    max_effort: crate::types::ReasoningEffort::High,
                })
            } else {
                None
            },
            responses_lite: false,
            agent_delegation: None,
            structured_output: structured,
            deferred_tool_loading: false,
        },
        limits: ModelLimits {
            context_window: 10000,
            max_output_tokens: 2000,
        },
        pricing: None,
        cache: crate::types::CacheCompatibility::default(),
    };

    let ep = Endpoint {
        id: EndpointId("test-ep".to_string()),
        base_url: url::Url::parse("https://api.openai.com/v1/").unwrap(),
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
fn openrouter_summary_reasoning_never_invents_a_disable() {
    use crate::types::{ReasoningControl, ReasoningOptions};
    let mut model = make_test_model(false, false, false, false, true, false);
    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.openai_chat_mode = OpenAiChatReasoningMode::OpenRouter;
    capability.options = Some(ReasoningOptions {
        values: vec!["max".into(), "high".into(), "low".into()],
        default: Some("max".into()),
    });
    capability.max_effort = ReasoningEffort::Max;
    let mut req = Request {
        system: Some("Summarize the conversation".into()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("history".into())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(1024),
        temperature: None,
        stop: vec![],
        reasoning: crate::select_auxiliary_reasoning(&model).unwrap(),
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };
    let body = |model: &Model, req: &Request| -> serde_json::Value {
        serde_json::from_slice(&build_request(model, req).unwrap().body).unwrap()
    };
    assert_eq!(
        body(&model, &req)["reasoning"],
        serde_json::json!({"effort":"max"})
    );
    req.reasoning = ReasoningConfig::Off;
    assert!(matches!(
        build_request(&model, &req),
        Err(AiError::Unsupported(crate::UnsupportedError::Reasoning))
    ));
    // Optional OpenRouter Off is omission, not an unadvertised `none`.
    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability
        .options
        .as_mut()
        .unwrap()
        .values
        .insert(0, "none".into());
    assert!(body(&model, &req).get("reasoning").is_none());
    Arc::make_mut(&mut model.spec).preset.thinking_format =
        Some(crate::declarations::ThinkingFormat::OpenRouter);
    assert!(body(&model, &req).get("reasoning").is_none());
    Arc::make_mut(&mut model.spec).preset.thinking_format = None;
    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.control = ReasoningControl::Toggle;
    capability.options = Some(ReasoningOptions {
        values: vec!["false".into(), "true".into()],
        default: Some("false".into()),
    });
    assert!(body(&model, &req).get("reasoning").is_none());
    req.reasoning = ReasoningConfig::On;
    assert_eq!(
        body(&model, &req)["reasoning"],
        serde_json::json!({"enabled":true})
    );
    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.control = ReasoningControl::AlwaysOn;
    capability.options = Some(ReasoningOptions {
        values: vec!["default".into()],
        default: Some("default".into()),
    });
    req.reasoning = crate::select_auxiliary_reasoning(&model).unwrap();
    assert!(body(&model, &req).get("reasoning").is_none());
    // Other profiles retain explicit Off semantics.
    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.control = ReasoningControl::Effort;
    capability.openai_chat_mode = OpenAiChatReasoningMode::Standard;
    capability.options = Some(ReasoningOptions {
        values: vec!["none".into(), "high".into()],
        default: Some("high".into()),
    });
    req.reasoning = ReasoningConfig::Off;
    assert_eq!(body(&model, &req)["reasoning_effort"], "none");
}

#[test]
fn test_build_request_text_only() {
    let model = make_test_model(false, false, false, false, false, false);
    let mut req = Request {
        system: Some("System instructions".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![
                UserPart::Text("Hel".to_string()),
                UserPart::Text("lo".to_string()),
            ],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: Some(0.8),
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
    assert_eq!(
        parts.url.to_string(),
        "https://api.openai.com/v1/chat/completions"
    );
    assert!(parts.streaming);

    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["model"], "gpt-4-test");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert!(
        body.get("max_completion_tokens").is_none() && body.get("max_tokens").is_none(),
        "local model limits must not become provider request parameters"
    );
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "System instructions");
    assert_eq!(body["messages"][1]["role"], "user");
    // A plain text user message uses the string form accepted by both
    // OpenAI and text-only OpenAI-compatible providers such as DeepSeek.
    assert_eq!(body["messages"][1]["content"], "Hello");

    req.max_output_tokens = Some(1000);
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["max_completion_tokens"], 1000);
    assert!(body.get("max_tokens").is_none());
}

#[test]
fn omits_tool_choice_when_no_tools_enabled_for_request() {
    let model = make_test_model(false, false, false, true, false, false);
    let req = Request {
        system: Some("System prompt".to_string()),
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
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert!(
        body.get("tools").is_none(),
        "tools must be omitted when no tool defs are present"
    );
    assert!(
        body.get("tool_choice").is_none(),
        "tool_choice must be omitted when tools are omitted"
    );
}

#[test]
fn system_message_reasoning_mode_keeps_qwen_compatible_role() {
    let mut model = make_test_model(false, false, false, true, true, false);
    Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap()
        .openai_chat_mode = OpenAiChatReasoningMode::SystemMessage;
    let req = Request {
        system: Some("system prompt".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".to_string())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Effort(ReasoningEffort::High),
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn always_on_reasoning_uses_provider_default_without_a_control_parameter() {
    let mut model = make_test_model(false, false, false, true, true, false);
    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.control = crate::types::ReasoningControl::AlwaysOn;
    capability.openai_chat_mode = OpenAiChatReasoningMode::SystemMessage;
    let mut request = Request {
        system: Some("system prompt".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".to_string())],
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

    assert!(matches!(
        build_request(&model, &request),
        Err(AiError::Unsupported(
            crate::error::UnsupportedError::Reasoning
        ))
    ));
    request.reasoning = ReasoningConfig::On;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("reasoning").is_none());
    assert!(body.get("thinking").is_none());
}

#[test]
fn provider_reasoning_values_preserve_literals_but_omit_semantic_default() {
    let mut model = make_test_model(false, false, false, true, true, false);
    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.control = crate::types::ReasoningControl::Toggle;
    capability.openai_chat_mode = OpenAiChatReasoningMode::ProviderValues {
        values: vec!["none".into(), "default".into()],
        default: Some("default".into()),
        system_message: true,
    };
    let mut req = Request {
        system: Some("system prompt".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".to_string())],
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

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["reasoning_effort"], "none");

    req.reasoning = ReasoningConfig::On;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert!(body.get("reasoning_effort").is_none());

    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.openai_chat_mode = OpenAiChatReasoningMode::ProviderValues {
        values: vec!["off".into(), "on".into()],
        default: Some("on".into()),
        system_message: true,
    };
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["reasoning_effort"], "on");

    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.control = crate::types::ReasoningControl::Effort;
    capability.openai_chat_mode = OpenAiChatReasoningMode::ProviderValues {
        values: vec!["none".into(), "low".into(), "high".into()],
        default: Some("low".into()),
        system_message: true,
    };
    capability.min_effort = ReasoningEffort::Low;
    capability.max_effort = ReasoningEffort::High;
    req.reasoning = ReasoningConfig::Effort(ReasoningEffort::High);
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn unsupported_deferred_tool_loading_rejects_instead_of_hiding_schemas() {
    let mut model = make_test_model(false, false, false, true, false, false);
    Arc::make_mut(&mut model.spec)
        .capabilities
        .deferred_tool_loading = true;

    let make_tool = |name: &str| crate::types::ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: name.to_string(),
        description: "test tool".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    };
    let request = Request {
        system: None,
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: ToolCallId("call-1".into()),
                    name: "read".into(),
                    arguments_json: "{}".to_string(),
                    argument_error: None,
                })],
                model: ModelId("test-model".into()),
                protocol: Protocol::OpenAiChat,
            }),
            Message::User(UserMessage {
                content: vec![UserPart::ToolResult(crate::types::ToolResult {
                    tool_call_id: ToolCallId("call-1".into()),
                    content: vec![ToolResultPart::Text("connected".into())],
                    is_error: false,
                    added_tool_names: Some(vec!["browser_click".into()]),
                })],
            }),
        ],
        tools: vec![
            make_tool("read"),
            make_tool("bash"),
            make_tool("browser_click"),
        ],
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

    assert!(matches!(
        build_request(&model, &request),
        Err(AiError::Config(crate::error::ConfigError::InvalidModel(_)))
    ));
}

#[test]
fn completed_grammar_custom_call_uses_declared_property_and_schema_validation() {
    // Pi admits grammar `custom` tools only on a route that declares
    // `supportsOpenAIGrammarTools`; the decode side uses the same
    // declaration to recover the tool's declared input property.
    let mut model = make_test_model(false, false, false, true, false, false);
    std::sync::Arc::make_mut(&mut model.spec)
        .preset
        .supports_openai_grammar_tools = Some(true);
    let tool = ToolDef {
        async_execution: false,
        name: "language".to_owned(),
        description: "grammar".to_owned(),
        parameters: serde_json::json!({"type":"object", "properties":{"source":{"type":"string"}},
            "required":["source"], "additionalProperties":false}),
        constrained_sampling: Some(crate::types::ConstrainedSampling::Grammar {
            variants: crate::types::GrammarVariants {
                openai_regex: Some(".+".to_owned()),
                ..Default::default()
            },
        }),
    };
    let body = serde_json::json!({"id":"call", "choices":[{
        "message":{"role":"assistant","content":null,"tool_calls":[{
            "id":"c1","type":"custom","custom":{"name":"language","input":"quote \"\n雪"}}]},
        "finish_reason":"tool_calls"}], "usage":{"prompt_tokens":2,"completion_tokens":3}});
    let response =
        decode_response_with_tools(&model, &serde_json::to_vec(&body).unwrap(), None, &[tool])
            .unwrap();
    let AssistantPart::ToolCall(call) = &response.message.content[0] else {
        panic!("missing custom call")
    };
    assert!(call.argument_error.is_none());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&call.arguments_json).unwrap(),
        serde_json::json!({"source":"quote \"\n雪"})
    );
}

#[test]
fn constrained_sampling_emits_strict_and_grammar_custom_tools() {
    use crate::types::{ConstrainedSampling, ConstrainedSamplingStrict, GrammarVariants};

    // The grammar `custom` shape is declaration-gated per route.
    let mut model = make_test_model(false, false, false, true, false, false);
    std::sync::Arc::make_mut(&mut model.spec)
        .preset
        .supports_openai_grammar_tools = Some(true);
    let request = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".into())],
        })],
        tools: vec![
            crate::types::ToolDef {
                async_execution: false,
                constrained_sampling: Some(ConstrainedSampling::JsonSchema {
                    strict: ConstrainedSamplingStrict::Prefer,
                }),
                name: "strict_tool".to_string(),
                description: "strict".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }),
            },
            crate::types::ToolDef {
                async_execution: false,
                constrained_sampling: Some(ConstrainedSampling::Grammar {
                    variants: GrammarVariants {
                        openai_lark: Some("start: WORD".to_string()),
                        openai_regex: None,
                    },
                }),
                name: "grammar_tool".to_string(),
                description: "grammar".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"input": {"type": "string"}},
                    "required": ["input"]
                }),
            },
        ],
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

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["strict"], true);
    // Strict rewrite closes the object and re-lists every property.
    assert_eq!(
        tools[0]["function"]["parameters"]["additionalProperties"],
        false
    );
    assert_eq!(
        tools[0]["function"]["parameters"]["required"],
        serde_json::json!(["city"])
    );
    assert_eq!(tools[1]["type"], "custom");
    assert_eq!(tools[1]["custom"]["name"], "grammar_tool");
    assert_eq!(tools[1]["custom"]["format"]["type"], "grammar");
    assert_eq!(tools[1]["custom"]["format"]["grammar"]["syntax"], "lark");
    assert_eq!(
        tools[1]["custom"]["format"]["grammar"]["definition"],
        "start: WORD"
    );
}

#[test]
fn required_constrained_sampling_that_cannot_be_honored_is_rejected() {
    use crate::types::{ConstrainedSampling, ConstrainedSamplingStrict};

    let model = make_test_model(false, false, false, true, false, false);
    let request = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".into())],
        })],
        tools: vec![crate::types::ToolDef {
            async_execution: false,
            constrained_sampling: Some(ConstrainedSampling::JsonSchema {
                strict: ConstrainedSamplingStrict::Require,
            }),
            name: "unstrictable".to_string(),
            description: String::new(),
            // `oneOf` is outside the strict subset, so a `require` request
            // must fail rather than silently downgrade.
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"x": {"type": "string"}},
                "oneOf": [{"type": "object"}]
            }),
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
        session_id: None,
    };

    assert!(matches!(
        build_request(&model, &request),
        Err(AiError::Unsupported(
            crate::error::UnsupportedError::ConstrainedSampling(_)
        ))
    ));
}

#[test]
fn deferred_tool_loading_disabled_keeps_all_schemas() {
    let mut model = make_test_model(false, false, false, true, false, false);
    Arc::make_mut(&mut model.spec)
        .capabilities
        .deferred_tool_loading = false;

    let make_tool = |name: &str| crate::types::ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: name.to_string(),
        description: "test tool".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    };
    let request = Request {
        system: None,
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: ToolCallId("call-1".into()),
                    name: "read".into(),
                    arguments_json: "{}".to_string(),
                    argument_error: None,
                })],
                model: ModelId("test-model".into()),
                protocol: Protocol::OpenAiChat,
            }),
            Message::User(UserMessage {
                content: vec![UserPart::ToolResult(crate::types::ToolResult {
                    tool_call_id: ToolCallId("call-1".into()),
                    content: vec![ToolResultPart::Text("connected".into())],
                    is_error: false,
                    added_tool_names: Some(vec!["browser_click".into()]),
                })],
            }),
        ],
        tools: vec![
            make_tool("read"),
            make_tool("bash"),
            make_tool("browser_click"),
        ],
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

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 3);
}

#[test]
fn binary_reasoning_default_keeps_qwen_tool_schema_on_the_wire() {
    let mut model = make_test_model(false, false, false, true, true, false);
    let capability = Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap();
    capability.control = crate::types::ReasoningControl::Toggle;
    capability.openai_chat_mode = OpenAiChatReasoningMode::ProviderValues {
        values: vec!["none".into(), "default".into()],
        default: Some("default".into()),
        system_message: true,
    };
    let request = Request {
        system: Some("Use tools when needed.".into()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Read sentinel.txt".into())],
        })],
        tools: vec![crate::types::ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "read".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        }],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::On,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn deepseek_thinking_toggle_effort_and_tool_reasoning_replay() {
    let mut model = make_test_model(false, false, false, true, true, false);
    let spec = Arc::make_mut(&mut model.spec);
    spec.api_name = "deepseek-v4-pro".to_string();
    spec.capabilities
        .reasoning
        .as_mut()
        .unwrap()
        .openai_chat_mode = OpenAiChatReasoningMode::DeepSeekThinking;

    let mut req = Request {
        system: Some("system prompt".to_string()),
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("look this up".to_string())],
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantPart::Reasoning(ReasoningPart {
                        text: Some("I need the tool result first.".to_string()),
                        state: None,
                    }),
                    AssistantPart::ToolCall(ToolCall {
                        async_execution: false,
                        id: ToolCallId("call_1".to_string()),
                        name: "lookup".to_string(),
                        arguments_json: "{}".to_string(),
                        argument_error: None,
                    }),
                ],
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiChat,
            }),
        ],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Effort(ReasoningEffort::Low),
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
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["thinking"]["type"], "enabled");
    assert!(
        body.get("max_completion_tokens").is_none() && body.get("max_tokens").is_none(),
        "DeepSeek must not receive the local capacity reserve as a generated cap"
    );
    // This fixture explicitly selects the supported low wire value.
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(body["messages"][1]["content"], "look this up");
    assert_eq!(
        body["messages"][2]["reasoning_content"],
        "I need the tool result first."
    );
    assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call_1");

    req.max_output_tokens = Some(1000);
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["max_tokens"], 1000);
    assert!(body.get("max_completion_tokens").is_none());

    req.max_output_tokens = None;
    req.reasoning = ReasoningConfig::Off;
    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["thinking"]["type"], "disabled");
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn mistral_profile_uses_its_bounded_request_contract() {
    let mut model = make_test_model(false, false, false, true, false, false);
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .openai_chat_profile = crate::types::OpenAiChatRuntimeProfile::Mistral;
    let cache = &mut Arc::make_mut(&mut model.spec).cache;
    cache.send_session_affinity_headers = true;
    cache.session_affinity_format = Some(crate::types::SessionAffinityFormat::Mistral);

    let canonical_tool_id = "call_with_a_long_non_mistral_id";
    let request = Request {
        system: None,
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantPart::Reasoning(ReasoningPart {
                        text: Some("Inspect the repository first.".into()),
                        state: None,
                    }),
                    AssistantPart::ToolCall(ToolCall {
                        async_execution: false,
                        id: ToolCallId(canonical_tool_id.into()),
                        name: "read".into(),
                        arguments_json: r#"{"path":"README.md"}"#.into(),
                        argument_error: None,
                    }),
                ],
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiChat,
            }),
            Message::User(UserMessage {
                content: vec![UserPart::ToolResult(crate::types::ToolResult {
                    tool_call_id: ToolCallId(canonical_tool_id.into()),
                    content: vec![ToolResultPart::Text("contents".into())],
                    is_error: false,
                    added_tool_names: None,
                })],
            }),
        ],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(123),
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: Some("mistral-affinity".into()),
    };

    let parts = build_request(&model, &request).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["max_tokens"], 123);
    assert!(body.get("max_completion_tokens").is_none());
    assert!(body.get("stream_options").is_none());
    assert_eq!(
        body["messages"][0]["content"][0]["thinking"][0]["text"],
        "Inspect the repository first."
    );
    let normalized_id = body["messages"][0]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(normalized_id, mistral_tool_call_id(canonical_tool_id));
    assert_eq!(normalized_id.len(), 9);
    assert!(normalized_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric()));
    assert_eq!(body["messages"][1]["tool_call_id"], normalized_id);
    assert_eq!(parts.headers["x-affinity"], "mistral-affinity");
    assert!(parts.headers.get("session_id").is_none());
    assert!(parts.headers.get("x-client-request-id").is_none());
    assert!(parts.headers.get("x-session-affinity").is_none());
}

#[test]
fn reasoning_only_local_turn_replays_as_assistant_content() {
    let model = make_test_model(false, false, false, false, false, false);
    let request = Request {
        system: None,
        messages: vec![Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Reasoning(ReasoningPart {
                text: Some("I need to inspect the picker first.".into()),
                state: None,
            })],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiChat,
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

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    assert_eq!(body["messages"][0]["role"], "assistant");
    assert_eq!(
        body["messages"][0]["content"],
        "I need to inspect the picker first."
    );
    assert!(body["messages"][0].get("reasoning_content").is_none());
}

#[test]
fn test_build_request_audio_out() {
    let model = make_test_model(false, false, true, false, false, false);
    let req = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Say hello".to_string())],
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
        output_modalities: OutputModalities::TextAndAudio(AudioOutputOptions {
            format: AudioFormat::Wav,
            voice: AudioVoice::Named("alloy".to_string()),
        }),
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    let parts = build_request(&model, &req).unwrap();
    assert!(!parts.streaming);

    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["stream"], false);
    assert_eq!(body["modalities"][0], "text");
    assert_eq!(body["modalities"][1], "audio");
    assert_eq!(body["audio"]["voice"], "alloy");
    assert_eq!(body["audio"]["format"], "wav");
}

#[test]
fn test_decode_response_basic() {
    let model = make_test_model(false, false, false, false, false, false);
    let raw_json = r#"{
        "id": "chatcmpl-123",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Hello back!"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    }"#;

    let resp = decode_response(&model, raw_json.as_bytes(), None).unwrap();
    assert_eq!(resp.response_id, Some("chatcmpl-123".to_string()));
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
    assert!(resp.diagnostics.is_empty());
    if let AssistantPart::Text(ref t) = resp.message.content[0] {
        assert_eq!(t, "Hello back!");
    } else {
        panic!("Expected Text part");
    }
}

#[test]
fn nonstream_defaulted_stop_diagnostic_preserves_required_usage() {
    let model = make_test_model(false, false, false, false, true, false);
    let mut body = serde_json::json!({
        "id": "private-response-id",
        "choices": [{"message": {"reasoning": "private reasoning"}}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 12},
    });
    for null_finish in [false, true] {
        if null_finish {
            body["choices"][0]["finish_reason"] = serde_json::Value::Null;
        }
        let response = decode_response(&model, body.to_string().as_bytes(), None).unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(response.usage.output_tokens, 12);
        assert_eq!(response.diagnostics.len(), 1);
        assert_eq!(response.diagnostics[0].code, "chat_defaulted_stop_reason");
        assert_eq!(
            response.diagnostics[0].message,
            "Chat completion stop reason was defaulted"
        );
    }
    // Nonstream usage was never defaulted: keep the existing decode error.
    body.as_object_mut().unwrap().remove("usage");
    assert!(matches!(
        decode_response(&model, body.to_string().as_bytes(), None),
        Err(AiError::Decode(DecodeError::Json(_)))
    ));
}

#[test]
fn test_decode_response_recovers_qwen_xml_tool_call() {
    let model = make_test_model(false, false, false, true, false, false);
    let raw_json = r#"{
        "id": "chatcmpl-qwen-xml",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "<tool_call><function=read><parameter=path>README.md</parameter></function></tool_call>"
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
    }"#;

    let response = decode_response(&model, raw_json.as_bytes(), None).unwrap();
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    let call = response
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("Qwen XML call present");
    assert_eq!(call.name, "read");
    assert_eq!(
        call.arguments_value().unwrap(),
        serde_json::json!({"path": "README.md"})
    );
}

#[test]
fn compatibility_parser_accepts_common_json_and_xml_tool_dialects() {
    for (raw, expected_name, expected_args) in [
        (
            r#"{"name":"read","arguments":{"path":"README.md",}}"#,
            "read",
            serde_json::json!({"path": "README.md"}),
        ),
        (
            r#"{"type":"function","function":{"name":"exec","arguments":"{'command':'pwd',}"}}"#,
            "exec",
            serde_json::json!({"command": "pwd"}),
        ),
        (
            r#"{name:'read', arguments:{path:'src/lib.rs',},}"#,
            "read",
            serde_json::json!({"path": "src/lib.rs"}),
        ),
        (
            r#"<function name="read"><parameter name="path">src/lib.rs</parameter></function>"#,
            "read",
            serde_json::json!({"path": "src/lib.rs"}),
        ),
        (
            r#"<function read><parameter=path>src/main.rs</parameter></function>"#,
            "read",
            serde_json::json!({"path": "src/main.rs"}),
        ),
    ] {
        let calls = parse_compat_tool_calls(raw).unwrap();
        assert_eq!(calls.len(), 1, "{raw}");
        assert_eq!(calls[0].0, expected_name, "{raw}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&calls[0].1).unwrap(),
            expected_args,
            "{raw}"
        );
    }

    let calls = parse_compat_tool_calls(
        r#"[{"tool":"read","args":{"path":"a"}},{"name":"read","input":{"path":"b"}}]"#,
    )
    .unwrap();
    assert_eq!(calls.len(), 2);

    for truncated in [
        r#"{"name":"exec","arguments":{"command":"rm -rf /"#,
        r#"{name:'exec',arguments:{command:'rm -rf /"#,
        "<function=exec><parameter=command>rm -rf /",
    ] {
        assert!(
            parse_compat_tool_calls(truncated).is_err(),
            "accepted truncated call {truncated:?}"
        );
    }
}

#[test]
fn completed_native_tool_arguments_are_repaired_conservatively() {
    let model = make_test_model(false, false, false, true, false, false);
    let raw_json = r#"{
        "id":"repair",
        "choices":[{
            "message":{"tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{'path':'C:\\Users\\example',}"}
            }]},
            "finish_reason":"tool_calls"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
    }"#;
    let response = decode_response(&model, raw_json.as_bytes(), None).unwrap();
    let call = response
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(call.arguments_value().unwrap()["path"], r"C:\Users\example");
}

#[test]
fn completed_bare_json_tool_call_is_recovered_from_content() {
    let model = make_test_model(false, false, false, true, false, false);
    let raw_json = r#"{
        "id":"bare-json",
        "choices":[{
            "message":{"content":"```json\n{\"tool\":\"read\",\"arguments\":{\"path\":\"README.md\",}}\n```"},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
    }"#;
    let response = decode_response(&model, raw_json.as_bytes(), None).unwrap();
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert!(matches!(
        &response.message.content[..],
        [AssistantPart::ToolCall(call)] if call.name == "read"
    ));
}

#[test]
fn tool_output_locked_is_suppressed_and_requests_recovery() {
    let model = make_test_model(false, false, false, true, false, false);
    let raw_json = r#"{
        "id":"locked",
        "choices":[{
            "message":{"content":"I will inspect it now.\n[tool_output_locked]"},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
    }"#;
    let response = decode_response(&model, raw_json.as_bytes(), None).unwrap();
    assert_eq!(
        response.stop_reason,
        StopReason::Other("tool_output_locked".to_string())
    );
    let text = response
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(!text.contains("tool_output_locked"));
    assert!(text.contains("inspect it"));
}

#[test]
fn test_decode_stream_event_text() {
    let model = make_test_model(false, false, false, false, false, false);
    let mut builder = ResponseBuilder::new(ModelId("m".to_string()), Protocol::OpenAiChat, None);

    let sse = SseEvent {
        event: None,
        data: r#"{"id": "chunk-1", "choices": [{"delta": {"content": "Hello"}}]}"#.to_string(),
    };

    let evs = decode_stream_event(&model, &sse, &mut builder).unwrap();
    assert_eq!(evs.len(), 3); // Started, TextStart, TextDelta
    assert!(matches!(evs[0], StreamEvent::Started { .. }));
    assert!(matches!(evs[1], StreamEvent::TextStart { .. }));
    if let StreamEvent::TextDelta { ref delta, .. } = evs[2] {
        assert_eq!(delta, "Hello");
    } else {
        panic!("Expected TextDelta");
    }
}

#[test]
fn anthropic_style_chat_cache_markers_cover_system_conversation_and_tools() {
    let mut model = make_test_model(false, false, false, true, false, false);
    Arc::make_mut(&mut model.spec).cache.cache_control_format =
        Some(crate::types::CacheControlFormat::Anthropic);
    let request = Request {
        system: Some("system".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("latest user turn".to_string())],
        })],
        tools: vec![crate::types::ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
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
        session_id: Some("stable-session".to_string()),
    };

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        body["messages"][1]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn chat_usage_accepts_gateway_cache_aliases_and_keeps_buckets_disjoint() {
    let usage: ChatUsage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 1_000,
        "completion_tokens": 20,
        "total_tokens": 1_020,
        "prompt_cache_hit_tokens": 700,
        "prompt_tokens_details": {
            "cache_write_tokens": 100
        },
        "completion_tokens_details": { "reasoning_tokens": 5 }
    }))
    .unwrap();
    let mapped = map_usage(&usage).unwrap();
    assert_eq!(mapped.input_tokens, 200);
    assert_eq!(mapped.cache_read_tokens, 700);
    assert_eq!(mapped.cache_write_tokens, 100);
    assert_eq!(mapped.reasoning_tokens, 5);

    // When both read spellings occur, they describe one counter. The
    // documented nested field wins rather than double-counting a hit.
    let usage: ChatUsage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 100,
        "completion_tokens": 1,
        "total_tokens": 101,
        "prompt_cache_hit_tokens": 80,
        "prompt_tokens_details": { "cached_tokens": 60 }
    }))
    .unwrap();
    assert_eq!(map_usage(&usage).unwrap().cache_read_tokens, 60);

    // A completed gateway response must not be discarded merely because
    // its cache detail counters use a broader denominator than
    // `prompt_tokens`.
    let usage: ChatUsage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 100,
        "completion_tokens": 3,
        "total_tokens": 103,
        "prompt_tokens_details": {
            "cached_tokens": 120,
            "cache_write_tokens": 20
        },
        "completion_tokens_details": { "reasoning_tokens": 5 }
    }))
    .unwrap();
    let mapped = map_usage(&usage).unwrap();
    assert_eq!(mapped.input_tokens, 0);
    assert_eq!(mapped.cache_read_tokens, 120);
    assert_eq!(mapped.cache_write_tokens, 20);
    assert_eq!(mapped.output_tokens, 8);
    assert_eq!(mapped.reasoning_tokens, 5);
    assert_eq!(mapped.total_tokens, 148);
}

#[test]
fn cache_retention_controls_openai_chat_key() {
    let mut model = make_test_model(false, false, false, false, false, false);
    let mut req = Request {
        system: Some("system".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".to_string())],
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
        session_id: Some("b".repeat(70)),
    };

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(
        body["prompt_cache_key"].as_str().unwrap().chars().count(),
        64
    );
    assert!(body.get("prompt_cache_retention").is_none());

    let cache = &mut Arc::make_mut(&mut model.spec).cache;
    cache.send_session_affinity_headers = true;
    cache.session_affinity_format = Some(crate::types::SessionAffinityFormat::OpenRouter);
    let parts = build_request(&model, &req).unwrap();
    assert_eq!(
        parts.headers["x-session-id"],
        req.session_id.as_deref().unwrap()
    );
    assert!(parts.headers.get("x-client-request-id").is_none());

    Arc::make_mut(&mut model.spec).cache.session_affinity_format =
        Some(crate::types::SessionAffinityFormat::OpenAi);
    let parts = build_request(&model, &req).unwrap();
    assert_eq!(
        parts.headers["session_id"],
        req.session_id.as_deref().unwrap()
    );
    assert_eq!(
        parts.headers["x-client-request-id"],
        req.session_id.as_deref().unwrap()
    );
    assert_eq!(
        parts.headers["x-session-affinity"],
        req.session_id.as_deref().unwrap()
    );

    req.cache_retention = crate::types::CacheRetention::Long;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["prompt_cache_retention"], "24h");

    req.cache_retention = crate::types::CacheRetention::None;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert!(body.get("prompt_cache_key").is_none());
    assert!(body.get("prompt_cache_retention").is_none());
}

#[test]
fn opencode_chat_session_header_is_independent_of_cache_retention() {
    let mut model = make_test_model(false, false, false, false, false, false);
    Arc::make_mut(&mut model.spec)
        .cache
        .send_session_affinity_headers = true;
    Arc::make_mut(&mut model.endpoint).id = EndpointId("opencode-go".into());
    let mut req = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".into())],
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
        session_id: Some("zen-session".into()),
    };
    for retention in [
        crate::types::CacheRetention::None,
        crate::types::CacheRetention::Short,
    ] {
        req.cache_retention = retention;
        let parts = build_request(&model, &req).unwrap();
        assert_eq!(parts.headers["x-opencode-session"], "zen-session");
        assert!(parts.headers.get("session_id").is_none());
        assert!(parts.headers.get("x-session-affinity").is_none());
    }
    req.session_id = None;
    let parts = build_request(&model, &req).unwrap();
    assert!(parts.headers.get("x-opencode-session").is_none());

    req.session_id = Some("zen-session".into());
    Arc::make_mut(&mut model.endpoint).id = EndpointId("opencode".into());
    let parts = build_request(&model, &req).unwrap();
    assert_eq!(parts.headers["x-opencode-session"], "zen-session");
    Arc::make_mut(&mut model.endpoint)
        .default_headers
        .insert("x-opencode-session", "caller-session".parse().unwrap());
    let parts = build_request(&model, &req).unwrap();
    assert!(parts.headers.get("x-opencode-session").is_none());
    Arc::make_mut(&mut model.endpoint)
        .default_headers
        .remove("x-opencode-session");
    Arc::make_mut(&mut model.spec)
        .preset
        .headers
        .insert("X-OpenCode-Session".into(), "preset-session".into());
    let parts = build_request(&model, &req).unwrap();
    assert!(parts.headers.get("x-opencode-session").is_none());

    Arc::make_mut(&mut model.spec)
        .preset
        .headers
        .remove("X-OpenCode-Session");
    Arc::make_mut(&mut model.endpoint).id = EndpointId("baseten".into());
    req.cache_retention = crate::types::CacheRetention::Short;
    let parts = build_request(&model, &req).unwrap();
    assert_eq!(parts.headers["session_id"], "zen-session");
    assert_eq!(parts.headers["x-session-affinity"], "zen-session");
    assert!(parts.headers.get("x-opencode-session").is_none());
}

#[test]
fn test_build_request_audio_input() {
    let model = make_test_model(false, true, false, false, false, false);
    let audio_payload = bytes::Bytes::from(vec![0x00, 0x01, 0x02, 0x03]);
    let audio_media = crate::types::AudioMedia {
        payload: crate::types::AudioPayload::Inline(audio_payload),
        format: AudioFormat::Wav,
        transcript: None,
    };
    let req = Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Media(crate::types::Media::Audio(audio_media))],
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
    assert_eq!(messages.len(), 1);
    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "input_audio");
    let input_audio = &content[0]["input_audio"];
    assert_eq!(input_audio["format"], "wav");
    assert_eq!(input_audio["data"], "AAECAw==");
}

#[test]
fn test_build_request_image_input() {
    let model = make_test_model(true, false, false, false, false, false);

    let inline_image = Media::Image(ImageMedia {
        source: ImageSource::Inline(bytes::Bytes::from(vec![0x47, 0x49, 0x46])),
        media_type: Some(mime::IMAGE_GIF),
        detail: Some(ImageDetail::High),
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

    assert_eq!(content[0]["type"], "image_url");
    assert_eq!(content[0]["image_url"]["url"], "data:image/gif;base64,R0lG");
    assert_eq!(content[0]["image_url"]["detail"], "high");

    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(
        content[1]["image_url"]["url"],
        "https://example.com/test.png"
    );
    assert!(content[1]["image_url"]["detail"].is_null());
}
