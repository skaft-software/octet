use std::sync::Arc;
use url::Url;

use crate::{
    AssistantMessage, AssistantPart, Auth, Capabilities, CompatibilityMode::Lossy,
    CompatibilityMode::Strict, Endpoint, EndpointId, ImageMedia, ImageSource, Media, Message,
    Modality, ModalitySet, Model, ModelId, ModelLimits, ModelSpec, OutputFormat, OutputModalities,
    Protocol, ReasoningCapability, ReasoningConfig, ReasoningControl, ReasoningEffortBudgets,
    ReasoningPart, ReasoningState, ReasoningStateKind, Request, ToolCall, ToolCallId, ToolChoice,
    ToolResult, ToolResultPart, UserMessage, UserPart,
};

fn make_model(
    protocol: Protocol,
    image: bool,
    audio_in: bool,
    audio_out: bool,
    reasoning: bool,
) -> Model {
    let mut input = ModalitySet::none();
    if image {
        input = input.with(Modality::Image);
    }
    if audio_in {
        input = input.with(Modality::Audio);
    }

    let mut output = ModalitySet::none();
    if audio_out {
        output = output.with(Modality::Audio);
    }

    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId(format!("model-{:?}", protocol)),
        endpoint: EndpointId("ep-1".to_string()),
        api_name: "test-model".to_string(),
        display_name: None,
        protocol,
        capabilities: Capabilities {
            responses_features: Default::default(),
            input_modalities: input,
            output_modalities: output,
            tools: true,
            parallel_tool_calls: true,
            reasoning: if reasoning {
                Some(ReasoningCapability {
                    options: None,
                    control: if protocol == Protocol::AnthropicMessages {
                        ReasoningControl::TokenBudget
                    } else {
                        ReasoningControl::Effort
                    },
                    exposes_text: true,
                    preserves_state: true,
                    effort_budgets: if protocol == Protocol::AnthropicMessages {
                        Some(ReasoningEffortBudgets {
                            minimal: 1024,
                            low: 2048,
                            medium: 4096,
                            high: 8192,
                            xhigh: 16384,
                            max: 32768,
                        })
                    } else {
                        None
                    },
                    openai_chat_mode: crate::OpenAiChatReasoningMode::Standard,
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
            context_window: 10000,
            max_output_tokens: 8192,
        },
        pricing: None,
        cache: crate::types::CacheCompatibility::default(),
    };

    let ep = Endpoint {
        id: EndpointId("ep-1".to_string()),
        base_url: Url::parse("https://api.provider.com/v1/").unwrap(),
        auth: Auth::none(),
        default_headers: http::HeaderMap::new(),
        transport: crate::types::EndpointTransport::Http,
        runtime: crate::types::RequestRuntime::default(),
        timeout: std::time::Duration::from_secs(10),
    };

    Model {
        spec: Arc::new(spec),
        endpoint: Arc::new(ep),
    }
}

#[test]
fn test_cross_protocol_canonical_immutability() {
    let model = make_model(Protocol::OpenAiChat, true, true, false, true);

    let image = Media::Image(ImageMedia {
        source: ImageSource::Inline(bytes::Bytes::from(vec![1, 2, 3])),
        media_type: Some(mime::IMAGE_PNG),
        detail: None,
    });

    let tool_call = ToolCall {
        async_execution: false,
        id: ToolCallId("call_1".to_string()),
        name: "test_tool".to_string(),
        arguments_json: "{}".to_string(),
        argument_error: None,
    };

    let tool_result = ToolResult {
        tool_call_id: ToolCallId("call_1".to_string()),
        content: vec![ToolResultPart::Text("done".to_string())],
        is_error: false,
        added_tool_names: None,
    };

    let assistant_reasoning = AssistantPart::Reasoning(ReasoningPart {
        text: Some("Thinking".to_string()),
        state: None,
    });

    let req = Request {
        system: Some("System prompt".to_string()),
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("Hello".to_string()), UserPart::Media(image)],
            }),
            Message::Assistant(AssistantMessage {
                content: vec![assistant_reasoning, AssistantPart::ToolCall(tool_call)],
                model: ModelId("model-OpenAiChat".to_string()),
                protocol: Protocol::OpenAiChat,
            }),
            Message::User(UserMessage {
                content: vec![UserPart::ToolResult(tool_result)],
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
        compatibility: Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    // Serialize to Chat Completions
    let parts_chat = crate::protocol::openai_chat::build_request(&model, &req).unwrap();
    assert!(!parts_chat.body.is_empty());

    // Verify req is unmodified
    assert_eq!(req.system, Some("System prompt".to_string()));
    assert_eq!(req.messages.len(), 3);
}

#[test]
fn test_cross_protocol_anthropic_message_merging() {
    let model = make_model(Protocol::AnthropicMessages, true, false, false, false);

    let req = Request {
        system: None,
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("First user message".to_string())],
            }),
            Message::User(UserMessage {
                content: vec![UserPart::Text(
                    "Second consecutive user message".to_string(),
                )],
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
        compatibility: Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    let parts = crate::protocol::anthropic::build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();

    // Consecutive user messages must be merged into one User message
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 2);
}

#[test]
fn test_lossy_inserts_missing_tool_result_before_next_assistant() {
    for protocol in [
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
        Protocol::GoogleGenerativeAi,
    ] {
        let model = make_model(protocol, false, false, false, false);
        let req = Request {
            system: None,
            messages: vec![
                Message::Assistant(AssistantMessage {
                    content: vec![AssistantPart::ToolCall(ToolCall {
                        async_execution: false,
                        id: ToolCallId("call_missing".to_string()),
                        name: "lookup".to_string(),
                        arguments_json: "{}".to_string(),
                        argument_error: None,
                    })],
                    model: model.spec.id.clone(),
                    protocol,
                }),
                Message::Assistant(AssistantMessage {
                    content: vec![AssistantPart::Text("continued".to_string())],
                    model: model.spec.id.clone(),
                    protocol,
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
            compatibility: Lossy,
            cache_retention: crate::types::CacheRetention::Short,
            session_id: None,
        };
        let body: serde_json::Value = match protocol {
            Protocol::OpenAiResponses => serde_json::from_slice(
                &crate::protocol::openai_responses::build_request(&model, &req)
                    .unwrap()
                    .body,
            )
            .unwrap(),
            Protocol::AnthropicMessages => serde_json::from_slice(
                &crate::protocol::anthropic::build_request(&model, &req)
                    .unwrap()
                    .body,
            )
            .unwrap(),
            Protocol::GoogleGenerativeAi => serde_json::from_slice(
                &crate::protocol::google::build_request(&model, &req)
                    .unwrap()
                    .body,
            )
            .unwrap(),
            Protocol::OpenAiChat | Protocol::BedrockConverse | Protocol::MistralConversations => {
                unreachable!()
            }
            // The pi-messages codec has its own wire-body fixtures; this
            // cross-codec fixture only compares the JSON-document codecs above.
            Protocol::PiMessages => unreachable!(),
        };
        let serialized = body.to_string();
        if protocol == Protocol::GoogleGenerativeAi {
            // This fixture's generic Google model does not advertise call IDs.
            // Check native name-based pairing and the inserted error's position
            // instead of requiring an unsupported wire field.
            assert_eq!(
                body["contents"],
                serde_json::json!([
                    {"role": "model", "parts": [
                        {"functionCall": {"name": "lookup", "args": {}}}
                    ]},
                    {"role": "user", "parts": [
                        {"functionResponse": {"name": "lookup", "response": {
                            "error": "Tool execution result was not supplied by the caller."
                        }}}
                    ]},
                    {"role": "model", "parts": [{"text": "continued"}]}
                ])
            );
        } else {
            assert!(serialized.contains("call_missing"));
        }
        assert!(serialized.contains("Tool execution result was not supplied"));
    }
}

#[test]
fn test_cross_protocol_reasoning_state_rejection() {
    // OpenAI model
    let model_openai = make_model(Protocol::OpenAiChat, false, false, false, true);

    // Reasoning state from Anthropic (cross-protocol/model mismatch)
    let state = ReasoningState {
        model: ModelId("claude-model".to_string()),
        protocol: Protocol::AnthropicMessages,
        kind: ReasoningStateKind::AnthropicSignature {
            signature: "sig_abc".to_string(),
        },
    };

    let req = Request {
        system: None,
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("Hello".to_string())],
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantPart::Reasoning(ReasoningPart {
                        text: Some("Thinking".to_string()),
                        state: Some(state),
                    }),
                    AssistantPart::Text("Hi".to_string()),
                ],
                model: ModelId("claude-model".to_string()),
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
        compatibility: Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    // In Strict mode, it should be rejected
    let res_strict = crate::protocol::openai_chat::build_request(&model_openai, &req);
    assert!(
        res_strict.is_err(),
        "Expected Strict mode rejection of cross-model reasoning state"
    );

    // In Lossy mode, it should compile successfully and drop the mismatched state
    let mut req_lossy = req.clone();
    req_lossy.compatibility = Lossy;
    let res_lossy = crate::protocol::openai_chat::build_request(&model_openai, &req_lossy);
    assert!(
        res_lossy.is_ok(),
        "Expected Lossy mode to drop state and pass"
    );
}

/// Strict JSON-schema constrained sampling is applied across every codec family,
/// and grammar-constrained tools become OpenAI `custom` tools where the wire
/// format defines them.
#[test]
fn constrained_sampling_wire_shape_across_codecs() {
    use crate::types::{ConstrainedSampling, ConstrainedSamplingStrict, GrammarVariants, ToolDef};

    let strict_tool = |name: &str| ToolDef {
        async_execution: false,
        name: name.to_string(),
        description: "strict".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
        constrained_sampling: Some(ConstrainedSampling::JsonSchema {
            strict: ConstrainedSamplingStrict::Prefer,
        }),
    };
    let grammar_tool = ToolDef {
        async_execution: false,
        name: "grammar_tool".to_string(),
        description: "grammar".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"input": {"type": "string"}},
            "required": ["input"]
        }),
        constrained_sampling: Some(ConstrainedSampling::Grammar {
            variants: GrammarVariants {
                openai_lark: Some("start: WORD".to_string()),
                openai_regex: None,
            },
        }),
    };

    let req_for = || Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".to_string())],
        })],
        tools: vec![strict_tool("strict_tool"), grammar_tool.clone()],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };

    // Chat Completions: this fixture opts into strict tools; an unknown
    // compatible endpoint must not inherit the public OpenAI default.
    let mut chat = make_model(Protocol::OpenAiChat, false, false, false, false);
    Arc::make_mut(&mut chat.spec).preset.supports_strict_mode = Some(true);
    Arc::make_mut(&mut chat.spec)
        .preset
        .supports_openai_grammar_tools = Some(true);
    let body: serde_json::Value = serde_json::from_slice(
        &crate::protocol::openai_chat::build_request(&chat, &req_for())
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["tools"][0]["function"]["strict"], true);
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["additionalProperties"],
        false
    );
    assert_eq!(body["tools"][1]["type"], "custom");
    assert_eq!(
        body["tools"][1]["custom"]["format"]["grammar"]["syntax"],
        "lark"
    );

    // Responses: strict function tool + grammar `custom` tool, both declared on
    // this model (the plain Responses profile defaults strict off).
    let mut resp = make_model(Protocol::OpenAiResponses, false, false, false, false);
    Arc::make_mut(&mut resp.spec).preset.supports_strict_mode = Some(true);
    Arc::make_mut(&mut resp.spec)
        .preset
        .supports_openai_grammar_tools = Some(true);
    let body: serde_json::Value = serde_json::from_slice(
        &crate::protocol::openai_responses::build_request(&resp, &req_for())
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["tools"][0]["strict"], true);
    assert_eq!(
        body["tools"][0]["parameters"]["additionalProperties"],
        false
    );
    assert_eq!(body["tools"][1]["type"], "custom");
    assert_eq!(body["tools"][1]["format"]["type"], "grammar");

    // Anthropic: strict schema and explicit provider-enforcement flag,
    // enabled by the declared Anthropic compat record.
    let mut anthropic = make_model(Protocol::AnthropicMessages, false, false, false, false);
    Arc::make_mut(&mut anthropic.spec).preset.anthropic_compat =
        Some(crate::declarations::AnthropicCompatPreset {
            supports_strict_tools: Some(true),
            ..Default::default()
        });
    let body: serde_json::Value = serde_json::from_slice(
        &crate::protocol::anthropic::build_request(&anthropic, &req_for())
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(
        body["tools"][0]["input_schema"]["additionalProperties"],
        false
    );
    assert_eq!(body["tools"][0]["strict"], true);
    assert!(body["tools"][1].get("strict").is_none());
    // Grammar is unsupported on Anthropic messages, so it stays a function tool
    // with its canonical schema.
    assert_eq!(body["tools"][1]["name"], "grammar_tool");

    // Bedrock: strict flag inside toolSpec, enabled by the model declaration
    // (Bedrock defaults strict off).
    let mut bedrock = make_model(Protocol::BedrockConverse, false, false, false, false);
    Arc::make_mut(&mut bedrock.spec).preset.supports_strict_mode = Some(true);
    let body: serde_json::Value = serde_json::from_slice(
        &crate::protocol::bedrock::build_request(&bedrock, &req_for())
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["toolConfig"]["tools"][0]["toolSpec"]["strict"], true);
    assert_eq!(
        body["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["additionalProperties"],
        false
    );

    // Google: strict rewrite plus VALIDATED function-calling mode.
    let google = make_model(Protocol::GoogleGenerativeAi, false, false, false, false);
    let body: serde_json::Value = serde_json::from_slice(
        &crate::protocol::google::build_request(&google, &req_for())
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["additionalProperties"],
        false
    );
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["mode"],
        "VALIDATED"
    );
}

/// Pi admits strict JSON-schema tools and OpenAI grammar `custom` tools per
/// route, with API-specific defaults and a per-model compat override. Both
/// knobs are declaration data here; no codec branches on a provider identity.
#[test]
fn strict_and_grammar_tool_support_are_per_route_declared_defaults() {
    use crate::types::{ConstrainedSampling, ConstrainedSamplingStrict, GrammarVariants, ToolDef};

    let strict_tool = ToolDef {
        async_execution: false,
        name: "strict_tool".to_owned(),
        description: "strict".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
        constrained_sampling: Some(ConstrainedSampling::JsonSchema {
            strict: ConstrainedSamplingStrict::Prefer,
        }),
    };
    let grammar_tool = ToolDef {
        async_execution: false,
        name: "grammar_tool".to_owned(),
        description: "grammar".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"input": {"type": "string"}},
            "required": ["input"]
        }),
        constrained_sampling: Some(ConstrainedSampling::Grammar {
            variants: GrammarVariants {
                openai_lark: Some("start: WORD".to_owned()),
                openai_regex: None,
            },
        }),
    };
    let request = |tools: Vec<ToolDef>| Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".to_owned())],
        })],
        tools,
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: Strict,
        cache_retention: crate::types::CacheRetention::Short,
        session_id: None,
    };
    let body = |protocol: Protocol, tools: Vec<ToolDef>, tune: fn(&mut Model)| {
        let mut model = make_model(protocol, false, false, false, false);
        tune(&mut model);
        serde_json::from_slice::<serde_json::Value>(
            &match protocol {
                Protocol::OpenAiChat => {
                    crate::protocol::openai_chat::build_request(&model, &request(tools))
                }
                Protocol::OpenAiResponses => {
                    crate::protocol::openai_responses::build_request(&model, &request(tools))
                }
                Protocol::AnthropicMessages => {
                    crate::protocol::anthropic::build_request(&model, &request(tools))
                }
                Protocol::BedrockConverse => {
                    crate::protocol::bedrock::build_request(&model, &request(tools))
                }
                Protocol::GoogleGenerativeAi => {
                    crate::protocol::google::build_request(&model, &request(tools))
                }
                Protocol::MistralConversations => {
                    crate::protocol::mistral_conversations::build_request(&model, &request(tools))
                }
                // A future wire protocol must not break this test; the match is
                // deliberately open even while every current variant is covered.
                #[allow(unreachable_patterns)]
                _ => panic!("no codec registered for this protocol"),
            }
            .unwrap()
            .body,
        )
        .unwrap()
    };
    let none = |_model: &mut Model| {};

    // Unknown OpenAI-compatible Chat endpoints default to non-strict tools.
    let chat = body(Protocol::OpenAiChat, vec![strict_tool.clone()], none);
    assert_eq!(chat["tools"][0]["function"]["strict"], false);
    assert!(chat["tools"][0]["function"]["parameters"]
        .get("additionalProperties")
        .is_none());
    let public_openai = body(Protocol::OpenAiChat, vec![strict_tool.clone()], |model| {
        Arc::make_mut(&mut model.endpoint).base_url =
            Url::parse("https://api.openai.com/v1/").unwrap();
    });
    assert_eq!(public_openai["tools"][0]["function"]["strict"], true);
    let declared = body(Protocol::OpenAiChat, vec![strict_tool.clone()], |model| {
        Arc::make_mut(&mut model.spec).preset.supports_strict_mode = Some(true);
    });
    assert_eq!(declared["tools"][0]["function"]["strict"], true);
    let refused = body(Protocol::OpenAiChat, vec![strict_tool.clone()], |model| {
        Arc::make_mut(&mut model.endpoint).base_url =
            Url::parse("https://api.openai.com/v1/").unwrap();
        Arc::make_mut(&mut model.spec).preset.supports_strict_mode = Some(false);
    });
    assert_eq!(refused["tools"][0]["function"]["strict"], false);
    let mut required_tool = strict_tool.clone();
    required_tool.constrained_sampling = Some(ConstrainedSampling::JsonSchema {
        strict: ConstrainedSamplingStrict::Require,
    });
    let unknown_chat = make_model(Protocol::OpenAiChat, false, false, false, false);
    assert!(matches!(
        crate::protocol::openai_chat::build_request(&unknown_chat, &request(vec![required_tool])),
        Err(crate::AiError::Unsupported(
            crate::UnsupportedError::ConstrainedSampling(_)
        ))
    ));
    let chat_grammar = body(Protocol::OpenAiChat, vec![grammar_tool.clone()], none);
    assert_eq!(chat_grammar["tools"][0]["type"], "function");
    let chat_grammar = body(Protocol::OpenAiChat, vec![grammar_tool.clone()], |model| {
        Arc::make_mut(&mut model.spec)
            .preset
            .supports_openai_grammar_tools = Some(true);
    });
    assert_eq!(chat_grammar["tools"][0]["type"], "custom");

    // Responses: the plain profile defaults strict off; Codex and Azure default
    // it on; the model declaration can override either way.
    let plain = body(Protocol::OpenAiResponses, vec![strict_tool.clone()], none);
    assert!(plain["tools"][0].get("strict").is_none());
    let codex = body(
        Protocol::OpenAiResponses,
        vec![strict_tool.clone()],
        |model| {
            Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
                crate::types::ResponsesRuntimeProfile::Codex;
        },
    );
    assert_eq!(codex["tools"][0]["strict"], true);
    let azure = body(
        Protocol::OpenAiResponses,
        vec![strict_tool.clone()],
        |model| {
            Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
                crate::types::ResponsesRuntimeProfile::Azure;
        },
    );
    assert_eq!(azure["tools"][0]["strict"], true);
    let refused = body(
        Protocol::OpenAiResponses,
        vec![strict_tool.clone()],
        |model| {
            Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
                crate::types::ResponsesRuntimeProfile::Codex;
            Arc::make_mut(&mut model.spec).preset.supports_strict_mode = Some(false);
        },
    );
    assert!(refused["tools"][0].get("strict").is_none());

    // Anthropic: strict is off by default and enabled by the compat record.
    let anthropic = body(Protocol::AnthropicMessages, vec![strict_tool.clone()], none);
    assert!(anthropic["tools"][0]["input_schema"]
        .get("additionalProperties")
        .is_none());
    let anthropic_strict = body(
        Protocol::AnthropicMessages,
        vec![strict_tool.clone()],
        |model| {
            Arc::make_mut(&mut model.spec).preset.anthropic_compat =
                Some(crate::declarations::AnthropicCompatPreset {
                    supports_strict_tools: Some(true),
                    ..Default::default()
                });
        },
    );
    assert_eq!(
        anthropic_strict["tools"][0]["input_schema"]["additionalProperties"],
        false
    );

    assert!(anthropic["tools"][0].get("strict").is_none());
    assert_eq!(anthropic_strict["tools"][0]["strict"], true);

    // Bedrock: strict is off by default and opt-in per model.
    let bedrock = body(Protocol::BedrockConverse, vec![strict_tool.clone()], none);
    assert!(bedrock["toolConfig"]["tools"][0]["toolSpec"]
        .get("strict")
        .is_none());
    let bedrock_strict = body(
        Protocol::BedrockConverse,
        vec![strict_tool.clone()],
        |model| {
            Arc::make_mut(&mut model.spec).preset.supports_strict_mode = Some(true);
        },
    );
    assert_eq!(
        bedrock_strict["toolConfig"]["tools"][0]["toolSpec"]["strict"],
        true
    );

    // Google: VALIDATED by default, plainly AUTO when the model refuses strict.
    let google = body(
        Protocol::GoogleGenerativeAi,
        vec![strict_tool.clone()],
        none,
    );
    assert_eq!(
        google["toolConfig"]["functionCallingConfig"]["mode"],
        "VALIDATED"
    );
    let google_plain = body(
        Protocol::GoogleGenerativeAi,
        vec![strict_tool.clone()],
        |model| {
            Arc::make_mut(&mut model.spec).preset.supports_strict_mode = Some(false);
        },
    );
    assert_eq!(
        google_plain["toolConfig"]["functionCallingConfig"]["mode"],
        "AUTO"
    );

    // Native Mistral Conversations keeps Pi's compliant strict default.
    let mistral = body(Protocol::MistralConversations, vec![strict_tool], none);
    assert_eq!(
        mistral["tools"][0]["function"]["parameters"]["additionalProperties"],
        false
    );
}
