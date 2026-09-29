//! Unit tests for `crate::protocol::openai_responses`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `openai_responses.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be navigated separately.
//! This is still a child module of `crate::protocol::openai_responses`, so
//! `use super::*` reaches the entries the seam re-exports, and the items that
//! now live in a sibling module are imported from that sibling by name: the
//! request wire tree from `super::wire` and the decode helpers and usage DTO
//! from `super::stream`.

use super::stream::{map_usage, ResponsesUsageDto};
use super::wire::{
    into_wire_input, ResponsesRequest, COMPUTER_TOOL_NAME, MAX_COMPUTER_SCREENSHOT_BYTES,
};
use super::*;
use crate::catalog::Model;
use crate::error::AiError;
use crate::protocol::sse::SseEvent;
use crate::protocol::HttpRequestParts;
use crate::stream::{ResponseBuilder, StreamEvent};
use crate::test_fixtures::base_request;
use crate::types::{
    AssistantPart, CacheRetention, Protocol, ReasoningState, ReasoningStateKind, ToolCallId,
    ToolDef, ToolResultPart,
};
use crate::types::{
    Capabilities, Endpoint, EndpointId, ImageMedia, ImageSource, JsonSchemaFormat, Media, Message,
    ModalitySet, ModelId, ModelLimits, ModelSpec, OutputFormat, ProviderMediaRef, ReasoningConfig,
    Request, ResponsesRuntimeProfile, ToolChoice, UserMessage, UserPart,
};
use crate::CompatibilityMode;
use std::sync::Arc;

fn without_structured_output(model: &Model) -> Model {
    let mut spec = (*model.spec).clone();
    spec.capabilities.structured_output = false;
    Model {
        spec: Arc::new(spec),
        endpoint: model.endpoint.clone(),
    }
}

fn user_req(content: Vec<UserPart>, compatibility: CompatibilityMode) -> Request {
    Request {
        messages: vec![Message::User(UserMessage { content })],
        compatibility,
        ..base_request()
    }
}

fn make_test_model(reasoning: bool) -> Model {
    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("test-o1".to_string()),
        endpoint: EndpointId("responses-ep".to_string()),
        api_name: "o1-2024-12-17".to_string(),
        display_name: None,
        protocol: Protocol::OpenAiResponses,
        capabilities: Capabilities {
            responses_features: Default::default(),
            input_modalities: ModalitySet::none().with(crate::types::Modality::Image),
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: true,
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
            structured_output: true,
            deferred_tool_loading: false,
        },
        limits: ModelLimits {
            context_window: 200000,
            max_output_tokens: 16384,
        },
        pricing: None,
        cache: crate::types::CacheCompatibility::default(),
    };

    let ep = Endpoint {
        id: EndpointId("responses-ep".to_string()),
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
fn test_build_request_responses_basic() {
    let model = make_test_model(true);
    let req = Request {
        system: Some("System instructions".to_string()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Hello".to_string())],
        })],
        max_output_tokens: Some(1000),
        temperature: Some(0.5),
        ..base_request()
    };

    let parts = build_request(&model, &req).unwrap();
    assert_eq!(parts.url.to_string(), "https://api.openai.com/v1/responses");

    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["model"], "o1-2024-12-17");
    assert_eq!(body["max_output_tokens"], 1000);
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert!(body.get("parallel_tool_calls").is_none());
    assert!(body.get("text").is_none());
    assert_eq!(body["include"][0], "reasoning.encrypted_content");
    assert_eq!(body["input"][0]["type"], "message");
    assert_eq!(body["input"][0]["role"], "developer");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(
        body["input"][0]["content"][0]["text"],
        "System instructions"
    );
    assert_eq!(body["input"][1]["type"], "message");
    assert_eq!(body["input"][1]["role"], "user");
    assert_eq!(body["input"][1]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][1]["content"][0]["text"], "Hello");
}

#[test]
fn gpt_6_astra_preserves_default_reasoning_and_emits_top_wire_efforts() {
    let model = crate::catalog::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-6-astra".to_owned()))
        .unwrap();
    let mut req = user_req(
        vec![UserPart::Text("Hello".to_owned())],
        CompatibilityMode::Strict,
    );

    req.temperature = Some(0.7);
    assert!(build_request(&model, &req).is_err()); // Astra cannot honor Off.
    req.reasoning = ReasoningConfig::Effort(crate::types::ReasoningEffort::Low);
    assert!(build_request(&model, &req).is_err()); // Non-none reasoning rejects sampling.
    req.temperature = None;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["model"], "gpt-6-astra");
    assert_eq!(body["reasoning"]["effort"], "low");
    for unsupported in ["temperature", "top_p", "logprobs"] {
        assert!(
            body.get(unsupported).is_none(),
            "Astra request unexpectedly included {unsupported}"
        );
    }

    for (effort, expected) in [
        (crate::types::ReasoningEffort::Low, "low"),
        (crate::types::ReasoningEffort::Xhigh, "xhigh"),
        (crate::types::ReasoningEffort::Max, "max"),
    ] {
        req.reasoning = ReasoningConfig::Effort(effort);
        let body: serde_json::Value =
            serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
        assert_eq!(body["reasoning"]["effort"], expected);
    }
}

#[test]
fn responses_lite_uses_header_and_input_items_for_tools_and_instructions() {
    let mut model = make_test_model(true);
    let mut spec = (*model.spec).clone();
    spec.capabilities.responses_lite = true;
    spec.capabilities.agent_delegation = Some(crate::types::AgentDelegation::V2);
    spec.capabilities.reasoning.as_mut().unwrap().max_effort = crate::types::ReasoningEffort::Ultra;
    model.spec = Arc::new(spec);
    assert!(model.spec.capabilities.parallel_tool_calls);

    let mut req = user_req(
        vec![UserPart::Text("Hello".to_owned())],
        CompatibilityMode::Strict,
    );
    req.system = Some("System instructions".to_owned());
    req.reasoning = ReasoningConfig::Effort(crate::types::ReasoningEffort::Ultra);
    req.tools.push(ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
    });

    let parts = build_request(&model, &req).unwrap();
    assert_eq!(
        parts
            .headers
            .get("x-openai-internal-codex-responses-lite")
            .unwrap(),
        "true"
    );
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert!(body.get("instructions").is_none());
    assert!(body.get("tools").is_none());
    assert_eq!(body["input"][0]["type"], "additional_tools");
    assert_eq!(body["input"][0]["role"], "developer");
    assert_eq!(body["input"][0]["tools"][0]["type"], "namespace");
    assert_eq!(body["input"][0]["tools"][0]["name"], "functions");
    assert_eq!(body["input"][0]["tools"][0]["tools"][0]["name"], "read");
    assert_eq!(body["input"][1]["role"], "developer");
    assert_eq!(
        body["input"][1]["content"][0]["text"],
        "System instructions"
    );
    assert_eq!(body["input"][2]["role"], "user");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["reasoning"]["effort"], "max");
    assert_eq!(body["reasoning"]["context"], "all_turns");
    assert!(body["reasoning"].get("mode").is_none());
}

#[test]
fn responses_lite_refuses_required_strict_tool_constraints() {
    let mut model = make_test_model(true);
    Arc::make_mut(&mut model.spec).capabilities.responses_lite = true;
    let mut req = user_req(
        vec![UserPart::Text("hello".into())],
        CompatibilityMode::Strict,
    );
    req.tools.push(ToolDef {
        name: "strict".into(),
        async_execution: false,
        description: "required strict schema".into(),
        parameters: serde_json::json!({"type":"object", "properties":{}}),
        constrained_sampling: Some(crate::ConstrainedSampling::JsonSchema {
            strict: crate::ConstrainedSamplingStrict::Require,
        }),
    });
    assert!(matches!(
        build_request(&model, &req),
        Err(AiError::Unsupported(
            crate::UnsupportedError::ConstrainedSampling(_)
        ))
    ));
    req.tools[0].constrained_sampling = Some(crate::ConstrainedSampling::JsonSchema {
        strict: crate::ConstrainedSamplingStrict::Prefer,
    });
    assert!(build_request(&model, &req).is_ok());
}

#[test]
fn responses_lite_honors_disabled_parallel_tool_capability() {
    let mut model = make_test_model(true);
    let mut spec = (*model.spec).clone();
    spec.capabilities.responses_lite = true;
    spec.capabilities.parallel_tool_calls = false;
    model.spec = Arc::new(spec);

    let mut req = user_req(
        vec![UserPart::Text("Hello".to_owned())],
        CompatibilityMode::Strict,
    );
    req.tools.push(ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
    });

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["parallel_tool_calls"], false);
}

#[test]
fn responses_lite_disables_parallel_calls_without_tools_and_strips_image_detail() {
    let mut model = make_test_model(true);
    let mut spec = (*model.spec).clone();
    spec.capabilities.responses_lite = true;
    model.spec = Arc::new(spec);

    let req = user_req(
        vec![UserPart::Media(Media::Image(ImageMedia {
            source: ImageSource::Url(url::Url::parse("https://example.com/image.png").unwrap()),
            media_type: None,
            detail: Some(crate::types::ImageDetail::High),
        }))],
        CompatibilityMode::Strict,
    );

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["input"][0]["type"], "additional_tools");
    assert_eq!(body["input"][0]["tools"], serde_json::json!([]));
    assert_eq!(body["input"][1]["role"], "user");
    assert_eq!(
        body["input"][1]["content"][0]["image_url"],
        "https://example.com/image.png"
    );
    assert!(body["input"][1]["content"][0].get("detail").is_none());
}

#[test]
fn public_replay_encoder_keeps_opaque_items_and_verbatim_call_ids() {
    let model = make_test_model(true);
    let raw = serde_json::json!({
        "type": "function_call",
        "id": "fc_provider_item",
        "call_id": "call_raw|item_exact",
        "name": "exec",
        "arguments": "{\"command\":\"pwd\"}",
        "phase": "commentary",
        "encrypted_content": "opaque",
        "programmatic_tool": {"runtime": "python"},
        "future_field": {"nested": true}
    });
    let output =
        crate::responses::ResponsesOutput::new(vec![crate::responses::ResponsesItem::new(
            raw.clone(),
        )
        .unwrap()]);
    let replay = vec![
        crate::responses::ResponsesReplayItem::User(UserMessage {
            content: vec![UserPart::Text("run it".to_owned())],
        }),
        crate::responses::ResponsesReplayItem::Output(output),
        crate::responses::ResponsesReplayItem::User(UserMessage {
            content: vec![UserPart::ToolResult(crate::types::ToolResult {
                tool_call_id: crate::types::ToolCallId("call_raw|item_exact".to_owned()),
                content: vec![crate::types::ToolResultPart::Text("ok".to_owned())],
                is_error: false,
                added_tool_names: None,
            })],
        }),
    ];

    let input =
        crate::responses::encode_responses_replay(&model, Some("be precise"), &replay).unwrap();
    let value = serde_json::to_value(input).unwrap();
    assert_eq!(value[0]["role"], "developer");
    assert_eq!(value[2], raw);
    assert_eq!(value[3]["type"], "function_call_output");
    assert_eq!(value[3]["call_id"], "call_raw|item_exact");
}

#[test]
fn compacted_replay_base_replaces_prior_system_input_verbatim() {
    let model = make_test_model(true);
    let compacted = serde_json::json!({
        "type": "compaction",
        "id": "cmp_exact",
        "encrypted_content": "opaque-instructions-and-history"
    });
    let replay = vec![
        crate::responses::ResponsesReplayItem::Compacted(crate::responses::ResponsesOutput::new(
            vec![
                crate::responses::ResponsesItem::new(serde_json::json!({
                    "type": "message",
                    "id": "leading-preserved-output"
                }))
                .unwrap(),
                crate::responses::ResponsesItem::new(compacted.clone()).unwrap(),
            ],
        )),
        crate::responses::ResponsesReplayItem::User(UserMessage {
            content: vec![UserPart::Text("after checkpoint".to_owned())],
        }),
    ];

    let input =
        crate::responses::encode_responses_replay(&model, Some("must not be reinserted"), &replay)
            .unwrap();
    let value = serde_json::to_value(input).unwrap();
    assert_eq!(value[0]["id"], "leading-preserved-output");
    assert_eq!(value[1], compacted);
    assert_eq!(value[2]["role"], "user");
    assert!(!value.to_string().contains("must not be reinserted"));

    let mut req = user_req(
        vec![UserPart::Text("canonical fallback is unused".to_owned())],
        CompatibilityMode::Strict,
    );
    req.system = Some("current instructions".to_owned());
    req.responses = Some(crate::responses::ResponsesOptions::full_replay(
        crate::responses::ResponsesInput::new(
            value
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .map(crate::responses::ResponsesItem::new)
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        ),
    ));
    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["input"][0]["id"], "leading-preserved-output");
    assert_eq!(body["input"][1], compacted);
    assert_eq!(body["instructions"], "current instructions");
    assert!(!body["input"].to_string().contains("current instructions"));
}

#[test]
fn owned_responses_input_moves_through_both_tree_boundaries() {
    use crate::responses::{ResponsesInput, ResponsesItem};
    fn allocations(value: &serde_json::Value) -> (*const u8, *const serde_json::Value) {
        (
            value["encrypted_content"].as_str().unwrap().as_ptr(),
            value["future_field"].as_array().unwrap().as_ptr(),
        )
    }
    for count in [1, 128] {
        let input = ResponsesInput::new(
            (0..count)
                .map(|index| {
                    ResponsesItem::new(serde_json::json!({
                        "type": "reasoning", "id": format!("opaque-{index}"),
                        "encrypted_content": "opaque-🙂".repeat(1024),
                        "future_field": [{"nested": [null, true, 42, "exact"]}]
                    }))
                    .unwrap()
                })
                .collect(),
        );
        let original_allocations: Vec<_> = input
            .items()
            .iter()
            .map(|item| allocations(item.as_json()))
            .collect();
        let input = into_wire_input(input);
        let array_allocation = input.as_array().unwrap().as_ptr();
        assert_eq!(
            input
                .as_array()
                .unwrap()
                .iter()
                .map(allocations)
                .collect::<Vec<_>>(),
            original_allocations
        );
        let dto = ResponsesRequest {
            model: "model".to_owned(),
            input,
            instructions: None,
            previous_response_id: None,
            context_management: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            max_output_tokens: None,
            temperature: None,
            service_tier: None,
            reasoning: None,
            text: None,
            prompt_cache_key: None,
            prompt_cache_retention: None,
            prompt_cache_options: None,
            include: vec![],
            store: false,
            stream: true,
        };
        // The metadata serialization must never traverse the owned input,
        // even if the original input would subsequently overwrite a clone.
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({
                "model": "model", "store": false, "stream": true
            })
        );
        let body = dto.into_json().unwrap();
        assert_eq!(body.as_object().unwrap().len(), 4);
        let items = body["input"].as_array().unwrap();
        assert_eq!(items.as_ptr(), array_allocation);
        assert_eq!(
            items.iter().map(allocations).collect::<Vec<_>>(),
            original_allocations
        );
    }
}

#[test]
fn raw_replay_and_lite_rebuild_without_mutating_opaque_input() {
    use crate::responses::{ResponsesInput, ResponsesItem, ResponsesOptions};
    let raw = vec![
        serde_json::json!({"type":"compaction", "encrypted_content":"opaque-🙂".repeat(4096), "future":{"null":null, "array":[true, 3]}}),
        serde_json::json!({"type":"message", "role":"user", "content":[
            {"type":"input_image", "image_url":"https://example.com/image.png", "detail":"high", "future":[1,2]},
            {"type":"input_text", "text":"original", "detail":"keep"}
        ], "detail":"keep-root"}),
        serde_json::json!({"type":"function_call_output", "call_id":"call|verbatim", "output":[
            {"type":"input_image", "image_url":"https://example.com/result.png", "detail":"low", "future":{"keep":true}}
        ], "future":"retain"}),
        serde_json::json!({"type":"custom_tool_call_output", "call_id":"custom|verbatim", "output":[
            {"type":"input_image", "image_url":"https://example.com/custom.png", "detail":"auto"}
        ]}),
    ];
    for lite in [false, true] {
        let mut model = make_test_model(true);
        Arc::make_mut(&mut model.spec).capabilities.responses_lite = lite;
        let mut req = user_req(
            vec![UserPart::Text("unused canonical input".to_owned())],
            CompatibilityMode::Strict,
        );
        req.system = Some("fresh instructions".to_owned());
        req.tools.push(ToolDef {
            async_execution: false,
            name: "read".to_owned(),
            description: "read".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
            constrained_sampling: None,
        });
        req.responses = Some(ResponsesOptions::full_replay(ResponsesInput::new(
            raw.iter()
                .cloned()
                .map(|item| ResponsesItem::new(item).unwrap())
                .collect(),
        )));
        let original_request = serde_json::to_value(&req).unwrap();
        let first = build_request(&model, &req).unwrap();
        let second = build_request(&model, &req).unwrap();
        assert_eq!(first.body, second.body);
        assert_eq!(serde_json::to_value(&req).unwrap(), original_request);
        let body: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
        let mut expected = raw.clone();
        if lite {
            expected[1]["content"][0]
                .as_object_mut()
                .unwrap()
                .remove("detail");
            expected[2]["output"][0]
                .as_object_mut()
                .unwrap()
                .remove("detail");
            expected[3]["output"][0]
                .as_object_mut()
                .unwrap()
                .remove("detail");
            assert_eq!(&body["input"].as_array().unwrap()[2..], expected.as_slice());
            assert_eq!(body["input"][0]["type"], "additional_tools");
            assert_eq!(body["input"][1]["content"][0]["text"], "fresh instructions");
            assert_eq!(body["reasoning"]["context"], "all_turns");
            assert_eq!(body["parallel_tool_calls"], false);
            assert!(body.get("instructions").is_none());
            assert!(body.get("tools").is_none());
        } else {
            assert_eq!(body["input"], serde_json::Value::Array(expected));
            assert_eq!(body["instructions"], "fresh instructions");
            assert_eq!(body["tools"][0]["name"], "read");
            assert_eq!(body["parallel_tool_calls"], true);
        }
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        for omitted in [
            "previous_response_id",
            "max_output_tokens",
            "temperature",
            "text",
            "service_tier",
        ] {
            assert!(body.get(omitted).is_none(), "{omitted}");
        }
    }
}

#[test]
fn moved_input_keeps_canonical_and_opaque_grammar_replay_distinct() {
    use crate::responses::{ResponsesInput, ResponsesItem, ResponsesOptions};
    let mut model = make_test_model(false);
    Arc::make_mut(&mut model.spec)
        .preset
        .supports_openai_grammar_tools = Some(true);
    let mut req = user_req(
        vec![UserPart::Text("go".to_owned())],
        CompatibilityMode::Strict,
    );
    req.tools.push(ToolDef {
        async_execution: false,
        name: "language".to_owned(), description: "grammar".to_owned(),
        parameters: serde_json::json!({"type":"object", "properties":{"source":{"type":"string"}}, "required":["source"], "additionalProperties":false}),
        constrained_sampling: Some(crate::types::ConstrainedSampling::Grammar {
            variants: crate::types::GrammarVariants { openai_lark: Some("start: /.+/".to_owned()), openai_regex: None },
        }),
    });
    let source = "println(\"🙂\")\n";
    req.messages
        .push(Message::Assistant(crate::types::AssistantMessage {
            content: vec![AssistantPart::ToolCall(crate::types::ToolCall {
                async_execution: false,
                id: ToolCallId("call_canonical".to_owned()),
                name: "language".to_owned(),
                arguments_json: serde_json::json!({"source":source}).to_string(),
                argument_error: None,
            })],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiResponses,
        }));
    req.messages.push(Message::User(UserMessage {
        content: vec![UserPart::ToolResult(crate::types::ToolResult {
            tool_call_id: ToolCallId("call_canonical".to_owned()),
            content: vec![ToolResultPart::Text("ok".to_owned())],
            is_error: false,
            added_tool_names: None,
        })],
    }));
    let original = serde_json::to_value(&req).unwrap();
    let first = build_request(&model, &req).unwrap();
    assert_eq!(first.body, build_request(&model, &req).unwrap().body);
    assert_eq!(serde_json::to_value(&req).unwrap(), original);
    let body: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
    assert_eq!(body["input"][1]["type"], "custom_tool_call");
    assert_eq!(body["input"][1]["input"], source);
    assert!(body["input"][1].get("arguments").is_none());
    assert_eq!(body["input"][2]["type"], "custom_tool_call_output");

    let mut raw = vec![
        serde_json::json!({"type":"function_call", "name":"language", "call_id":"function|exact", "arguments":"{\"source\":\"unchanged\"}", "future":true}),
        serde_json::json!({"type":"custom_tool_call", "name":"language", "call_id":"custom|exact", "input":source, "future":[null, true]}),
        serde_json::json!({"type":"function_call_output", "call_id":"custom|exact", "output":"ok", "future":{"keep":true}}),
        serde_json::json!({"type":"function_call_output", "call_id":"function|exact", "output":"unconverted"}),
    ];
    req.responses = Some(ResponsesOptions::full_replay(ResponsesInput::new(
        raw.iter()
            .cloned()
            .map(|item| ResponsesItem::new(item).unwrap())
            .collect(),
    )));
    let original = serde_json::to_value(&req).unwrap();
    let first = build_request(&model, &req).unwrap();
    assert_eq!(first.body, build_request(&model, &req).unwrap().body);
    assert_eq!(serde_json::to_value(&req).unwrap(), original);
    let body: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
    raw[2]["type"] = "custom_tool_call_output".into();
    assert_eq!(body["input"], serde_json::Value::Array(raw));
}

#[test]
fn full_replay_request_does_not_mix_previous_response_id_or_storage() {
    let model = make_test_model(true);
    let mut req = user_req(
        vec![UserPart::Text("next".to_owned())],
        CompatibilityMode::Strict,
    );
    req.responses = Some(crate::responses::ResponsesOptions::full_replay(
        crate::responses::ResponsesInput::new(vec![crate::responses::ResponsesItem::new(
            serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "next"}]
            }),
        )
        .unwrap()]),
    ));

    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert!(body.get("previous_response_id").is_none());
    assert_eq!(body["store"], false);

    req.responses.as_mut().unwrap().previous_response_id = Some("resp_server".to_owned());
    let error = match build_request(&model, &req) {
        Err(error) => error,
        Ok(_) => panic!("full input plus previous_response_id must be rejected"),
    };
    assert!(error.to_string().contains("cannot be used together"));
}

#[test]
fn legacy_pro_reasoning_mode_is_never_serialized() {
    let model = make_test_model(true);
    let mut req = user_req(
        vec![UserPart::Text("review this migration".to_string())],
        CompatibilityMode::Lossy,
    );
    req.reasoning = ReasoningConfig::Effort(crate::types::ReasoningEffort::Medium);
    req.reasoning_mode = crate::types::ReasoningMode::Pro;

    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert!(body["reasoning"].get("mode").is_none());
    assert_eq!(body["reasoning"]["effort"], "medium");
    assert!(parts
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "ignored_reasoning_mode"));

    req.compatibility = CompatibilityMode::Strict;
    assert!(matches!(
        build_request(&model, &req),
        Err(AiError::Unsupported(
            crate::error::UnsupportedError::ReasoningMode
        ))
    ));
}

#[test]
fn reasoning_request_streams_summaries_and_replay_keeps_empty_summary() {
    let model = make_test_model(true);
    let mut req = user_req(
        vec![UserPart::Text("follow up".to_string())],
        CompatibilityMode::Strict,
    );
    req.reasoning = ReasoningConfig::Effort(crate::types::ReasoningEffort::High);
    req.messages = vec![
        Message::User(UserMessage {
            content: vec![UserPart::Text("initial".to_string())],
        }),
        Message::Assistant(crate::types::AssistantMessage {
            content: vec![AssistantPart::Reasoning(crate::types::ReasoningPart {
                text: None,
                state: Some(ReasoningState {
                    protocol: Protocol::OpenAiResponses,
                    model: model.spec.id.clone(),
                    kind: ReasoningStateKind::OpenAiReasoning {
                        item_id: Some("rs_terra".to_string()),
                        encrypted_content: Some("encrypted".to_string()),
                    },
                }),
            })],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiResponses,
        }),
        Message::User(UserMessage {
            content: vec![UserPart::Text("follow up".to_string())],
        }),
    ];

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["reasoning"]["summary"], "auto");
    assert_eq!(body["input"][1]["type"], "reasoning");
    assert_eq!(body["input"][1]["id"], "rs_terra");
    assert_eq!(body["input"][1]["summary"], serde_json::json!([]));
    assert_eq!(body["input"][1]["encrypted_content"], "encrypted");
}

#[test]
fn reasoning_effort_emits_xhigh_and_max_and_maps_ultra_to_max() {
    for (effort, expected) in [
        (crate::types::ReasoningEffort::Xhigh, "xhigh"),
        (crate::types::ReasoningEffort::Max, "max"),
        (crate::types::ReasoningEffort::Ultra, "max"),
    ] {
        let mut model = make_test_model(true);
        let mut spec = (*model.spec).clone();
        spec.capabilities.reasoning.as_mut().unwrap().max_effort =
            crate::types::ReasoningEffort::Ultra;
        spec.capabilities.agent_delegation = Some(crate::types::AgentDelegation::V2);
        model.spec = std::sync::Arc::new(spec);
        let mut req = user_req(
            vec![UserPart::Text("hi".to_string())],
            CompatibilityMode::Strict,
        );
        req.reasoning = ReasoningConfig::Effort(effort);
        let body: serde_json::Value =
            serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
        assert_eq!(body["reasoning"]["effort"], expected);
    }
}

#[test]
fn completed_assistant_history_uses_output_text_for_the_next_turn() {
    let model = make_test_model(false);
    let mut req = user_req(
        vec![UserPart::Text("second prompt".to_string())],
        CompatibilityMode::Strict,
    );
    req.messages = vec![
        Message::User(UserMessage {
            content: vec![UserPart::Text("first prompt".to_string())],
        }),
        Message::Assistant(crate::types::AssistantMessage {
            content: vec![AssistantPart::Text("first response".to_string())],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiResponses,
        }),
        Message::User(UserMessage {
            content: vec![UserPart::Text("second prompt".to_string())],
        }),
    ];

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["input"][1]["role"], "assistant");
    assert_eq!(body["input"][1]["content"][0]["type"], "output_text");
    assert_eq!(body["input"][1]["content"][0]["text"], "first response");
    assert_eq!(
        body["input"][1]["content"][0]["annotations"],
        serde_json::json!([])
    );
    assert_eq!(body["input"][2]["role"], "user");
    assert_eq!(body["input"][2]["content"][0]["type"], "input_text");
}

#[test]
fn responses_usage_tolerates_gateway_cache_totals_with_a_broader_denominator() {
    let usage: ResponsesUsageDto = serde_json::from_value(serde_json::json!({
        "input_tokens": 100,
        "output_tokens": 3,
        "total_tokens": 103,
        "input_tokens_details": {
            "cached_tokens": 120,
            "cache_write_tokens": 20
        },
        "output_tokens_details": { "reasoning_tokens": 5 }
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
fn cache_retention_controls_responses_key_and_headers() {
    let model = make_test_model(false);
    let mut req = user_req(
        vec![UserPart::Text("hello".to_string())],
        CompatibilityMode::Strict,
    );
    req.session_id = Some("a".repeat(70));

    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(
        body["prompt_cache_key"].as_str().unwrap().chars().count(),
        64
    );
    assert!(body.get("prompt_cache_retention").is_none());
    assert_eq!(
        parts.headers["session_id"],
        req.session_id.as_deref().unwrap()
    );
    assert_eq!(
        parts.headers["x-client-request-id"],
        req.session_id.as_deref().unwrap()
    );

    req.cache_retention = crate::types::CacheRetention::Long;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["prompt_cache_retention"], "24h");

    req.cache_retention = crate::types::CacheRetention::None;
    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert!(body.get("prompt_cache_key").is_none());
    assert!(body.get("prompt_cache_retention").is_none());
    assert!(parts.headers.get("session_id").is_none());
    assert!(parts.headers.get("x-client-request-id").is_none());
}

#[test]
fn responses_explicit_affinity_formats_emit_provider_specific_headers() {
    let mut model = make_test_model(false);
    let mut request = user_req(
        vec![UserPart::Text("hello".to_string())],
        CompatibilityMode::Strict,
    );
    request.session_id = Some("stable-session".to_string());

    Arc::make_mut(&mut model.spec).cache.session_affinity_format =
        Some(crate::types::SessionAffinityFormat::OpenAi);
    let parts = build_request(&model, &request).unwrap();
    assert_eq!(parts.headers["session_id"], "stable-session");
    assert_eq!(parts.headers["x-client-request-id"], "stable-session");
    assert_eq!(parts.headers["x-session-affinity"], "stable-session");

    let cache = &mut Arc::make_mut(&mut model.spec).cache;
    cache.send_session_id_header = false;
    cache.session_affinity_format = Some(crate::types::SessionAffinityFormat::OpenAiNoSession);
    let parts = build_request(&model, &request).unwrap();
    assert!(parts.headers.get("session_id").is_none());
    assert_eq!(parts.headers["x-client-request-id"], "stable-session");
    assert_eq!(parts.headers["x-session-affinity"], "stable-session");

    Arc::make_mut(&mut model.spec).cache.session_affinity_format =
        Some(crate::types::SessionAffinityFormat::OpenRouter);
    let parts = build_request(&model, &request).unwrap();
    assert_eq!(parts.headers["x-session-id"], "stable-session");
    assert!(parts.headers.get("x-client-request-id").is_none());
}

#[test]
fn opencode_responses_session_header_is_independent_of_cache_retention() {
    let mut model = make_test_model(false);
    Arc::make_mut(&mut model.endpoint).id = crate::EndpointId("opencode".into());
    Arc::make_mut(&mut model.spec)
        .cache
        .send_session_affinity_headers = true;
    let mut req = user_req(
        vec![UserPart::Text("hello".into())],
        CompatibilityMode::Strict,
    );
    req.session_id = Some("stable-session".into());
    req.cache_retention = CacheRetention::None;
    let parts = build_request(&model, &req).unwrap();
    assert_eq!(parts.headers["x-opencode-session"], "stable-session");
    assert!(parts.headers.get("x-client-request-id").is_none());

    Arc::make_mut(&mut model.spec)
        .preset
        .headers
        .insert("X-OpenCode-Session".into(), "caller-value".into());
    let parts = build_request(&model, &req).unwrap();
    assert!(parts.headers.get("x-opencode-session").is_none());
    req.session_id = None;
    Arc::make_mut(&mut model.spec).preset.headers.clear();
    let parts = build_request(&model, &req).unwrap();
    assert!(parts.headers.get("x-opencode-session").is_none());
}

#[test]
fn responses_codex_affinity_uses_the_request_session_id() {
    let mut model = make_test_model(false);
    let cache = &mut Arc::make_mut(&mut model.spec).cache;
    cache.send_session_id_header = false;
    cache.session_affinity_format = Some(crate::types::SessionAffinityFormat::Codex);
    let mut request = user_req(
        vec![UserPart::Text("hello".to_string())],
        CompatibilityMode::Strict,
    );
    request.session_id = Some("durable-session".to_string());

    let parts = build_request(&model, &request).unwrap();
    assert_eq!(parts.headers["session-id"], "durable-session");
    assert_eq!(parts.headers["x-client-request-id"], "durable-session");
    assert!(parts.headers.get("session_id").is_none());
}

#[test]
fn responses_compat_can_disable_standard_session_and_long_retention() {
    let mut model = make_test_model(false);
    let cache = &mut Arc::make_mut(&mut model.spec).cache;
    cache.send_session_id_header = false;
    cache.supports_long_retention = false;

    let mut req = user_req(
        vec![UserPart::Text("hello".to_string())],
        CompatibilityMode::Strict,
    );
    req.cache_retention = crate::types::CacheRetention::Long;
    req.session_id = Some("codex-session".to_string());

    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["prompt_cache_key"], "codex-session");
    assert!(body.get("prompt_cache_retention").is_none());
    assert!(parts.headers.get("session_id").is_none());
    assert_eq!(parts.headers["x-client-request-id"], "codex-session");
}

#[test]
fn test_build_request_responses_tool_shape() {
    let model = make_test_model(false);
    let mut req = Request {
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".to_string())],
        })],
        tools: vec![crate::types::ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "lookup".to_string(),
            description: "lookup data".to_string(),
            parameters: serde_json::json!({"type":"object"}),
        }],
        tool_choice: ToolChoice::Named("lookup".to_string()),
        ..base_request()
    };
    let mut capable = (*model.spec).clone();
    capable.capabilities.tools = true;
    let model = crate::catalog::Model {
        spec: std::sync::Arc::new(capable),
        endpoint: model.endpoint.clone(),
    };
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "lookup");
    assert!(body["tools"][0].get("function").is_none());
    assert_eq!(body["tool_choice"]["name"], "lookup");
    assert_eq!(body["parallel_tool_calls"], true);
    assert!(body.get("stop").is_none());
    assert!(body.get("max_completion_tokens").is_none());

    req.stop.push("END".to_string());
    assert!(matches!(
        build_request(&model, &req),
        Err(AiError::Unsupported(crate::UnsupportedError::StopSequences))
    ));
}

#[test]
fn declared_responses_runtime_defaults_to_low_verbosity_and_gates_parallel_tools() {
    let mut model = make_test_model(false);
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile = ResponsesRuntimeProfile::Codex;
    Arc::make_mut(&mut model.spec)
        .capabilities
        .parallel_tool_calls = false;

    let mut req = user_req(
        vec![UserPart::Text("hello".to_string())],
        CompatibilityMode::Strict,
    );
    req.tools = vec![crate::types::ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "lookup".to_string(),
        description: "lookup data".to_string(),
        parameters: serde_json::json!({"type":"object"}),
    }];

    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["text"]["verbosity"], "low");
    assert!(body["text"].get("format").is_none());
    assert_eq!(body["parallel_tool_calls"], false);

    Arc::make_mut(&mut model.spec)
        .capabilities
        .parallel_tool_calls = true;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["parallel_tool_calls"], true);
}

#[test]
fn test_decode_stream_event_responses() {
    let model = make_test_model(false);
    let mut builder =
        ResponseBuilder::new(ModelId("m".to_string()), Protocol::OpenAiResponses, None);

    let sse_created = SseEvent {
        event: None,
        data: r#"{"type": "response.created", "response": {"id": "resp-123"}}"#.to_string(),
    };

    let evs = decode_stream_event(&model, &sse_created, &mut builder).unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEvent::Started { .. }));
}

#[test]
fn test_build_request_image_input() {
    let model = make_test_model(false);

    let inline_image = Media::Image(ImageMedia {
        source: ImageSource::Inline(bytes::Bytes::from(vec![0x47, 0x49, 0x46])),
        media_type: Some(mime::IMAGE_GIF),
        detail: Some(crate::types::ImageDetail::Low),
    });

    let url_image = Media::Image(ImageMedia {
        source: ImageSource::Url(url::Url::parse("https://example.com/test.png").unwrap()),
        media_type: None,
        detail: None,
    });

    let req = Request {
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Media(inline_image), UserPart::Media(url_image)],
        })],
        ..base_request()
    };

    let parts = build_request(&model, &req).unwrap();
    let body_val: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();

    let input_items = body_val["input"].as_array().unwrap();
    assert_eq!(input_items.len(), 1);
    let parts_array = input_items[0]["content"].as_array().unwrap();
    assert_eq!(parts_array.len(), 2);

    assert_eq!(parts_array[0]["type"], "input_image");
    assert_eq!(
        parts_array[0]["image_url"].as_str(),
        Some("data:image/gif;base64,R0lG")
    );
    assert_eq!(parts_array[0]["detail"].as_str(), Some("low"));

    assert_eq!(parts_array[1]["type"], "input_image");
    assert_eq!(
        parts_array[1]["image_url"].as_str(),
        Some("https://example.com/test.png")
    );
    assert!(parts_array[1]["detail"].is_null());
}

// f3: a Lossy structured-output downgrade must drop `text.format`, not just
// emit a diagnostic.
#[test]
fn lossy_structured_output_downgrade_omits_text_format() {
    let model = without_structured_output(&make_test_model(false));
    let mut req = user_req(
        vec![UserPart::Text("hi".to_string())],
        CompatibilityMode::Lossy,
    );
    req.output_format = OutputFormat::JsonSchema(JsonSchemaFormat {
        name: "Out".to_string(),
        description: None,
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    });

    let parts = build_request(&model, &req).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
    assert!(
        body.get("text").is_none(),
        "downgraded request must not serialize `text.format`: {body}"
    );
    assert!(parts
        .diagnostics
        .iter()
        .any(|d| d.code == "downgraded_output_format"));

    // Strict still rejects outright.
    req.compatibility = CompatibilityMode::Strict;
    assert!(matches!(
        build_request(&model, &req),
        Err(AiError::Unsupported(
            crate::UnsupportedError::StructuredOutput
        ))
    ));
}

// f4: an expired provider ref is dropped from the wire (Lossy) with a
// diagnostic; Strict rejects it.
#[test]
fn lossy_expired_provider_ref_is_dropped() {
    let model = make_test_model(false);
    let expired = UserPart::Media(Media::Image(ImageMedia {
        source: ImageSource::ProviderRef(ProviderMediaRef {
            protocol: Protocol::OpenAiResponses,
            id: "file_expired".to_string(),
            expires_at: Some(std::time::UNIX_EPOCH),
        }),
        media_type: None,
        detail: None,
    }));
    let req = user_req(vec![expired], CompatibilityMode::Lossy);

    let parts = build_request(&model, &req).unwrap();
    let body = String::from_utf8(parts.body.to_vec()).unwrap();
    assert!(
        !body.contains("file_expired"),
        "expired provider ref must not be serialized: {body}"
    );
    assert!(parts
        .diagnostics
        .iter()
        .any(|d| d.code == "dropped_expired_media_ref"));
}

// f10: an inline image with no media type is dropped rather than defaulted to
// a guessed `image/jpeg` (design §75).
#[test]
fn lossy_inline_image_without_media_type_is_dropped() {
    let model = make_test_model(false);
    let img = UserPart::Media(Media::Image(ImageMedia {
        source: ImageSource::Inline(bytes::Bytes::from(vec![1, 2, 3])),
        media_type: None,
        detail: None,
    }));
    let req = user_req(vec![img], CompatibilityMode::Lossy);

    let parts = build_request(&model, &req).unwrap();
    let body = String::from_utf8(parts.body.to_vec()).unwrap();
    assert!(
        !body.contains("image/jpeg") && !body.contains("input_image"),
        "inline image without media type must be dropped, not guessed: {body}"
    );
    assert!(parts
        .diagnostics
        .iter()
        .any(|d| d.code == "dropped_image_media_type"));
}

// --- Codex `service_tier` (declared endpoint capability) ---

fn with_responses_profile(model: &Model, profile: ResponsesRuntimeProfile) -> Model {
    let mut endpoint = (*model.endpoint).clone();
    endpoint.runtime.responses_profile = profile;
    Model {
        spec: model.spec.clone(),
        endpoint: Arc::new(endpoint),
    }
}

fn body_of(parts: &HttpRequestParts) -> serde_json::Value {
    serde_json::from_slice(&parts.body).unwrap()
}

#[test]
fn service_tier_is_absent_unless_the_caller_requests_it() {
    let model = with_responses_profile(&make_test_model(true), ResponsesRuntimeProfile::Codex);
    let parts = build_request(&model, &user_req(vec![], CompatibilityMode::Lossy)).unwrap();
    assert!(body_of(&parts).get("service_tier").is_none());
}

#[test]
fn codex_service_tier_wire_values_match_the_declared_tiers() {
    let model = with_responses_profile(&make_test_model(true), ResponsesRuntimeProfile::Codex);
    for tier in [
        crate::types::ServiceTier::Auto,
        crate::types::ServiceTier::Default,
        crate::types::ServiceTier::Flex,
        crate::types::ServiceTier::Priority,
    ] {
        let mut req = user_req(vec![], CompatibilityMode::Lossy);
        req.responses = Some(crate::responses::ResponsesOptions::default().with_service_tier(tier));
        let parts = build_request(&model, &req).unwrap();
        assert_eq!(body_of(&parts)["service_tier"], tier.wire_value());
    }
}

#[test]
fn service_tier_fails_closed_on_a_profile_that_does_not_declare_it() {
    // The default (public OpenAI Responses) profile does not declare the
    // field, so a caller request is rejected instead of silently dropped.
    let model = make_test_model(true);
    assert!(!model
        .endpoint
        .runtime
        .responses_profile
        .accepts_service_tier());
    let mut req = user_req(vec![], CompatibilityMode::Lossy);
    req.responses = Some(
        crate::responses::ResponsesOptions::default()
            .with_service_tier(crate::types::ServiceTier::Priority),
    );
    let err = match build_request(&model, &req) {
        Err(err) => err,
        Ok(_) => panic!("expected a fail-closed service tier error"),
    };
    assert!(
        matches!(
            err,
            AiError::Unsupported(crate::error::UnsupportedError::ServiceTier)
        ),
        "expected a fail-closed service tier error, got {err:?}"
    );
}

// --- Responses computer use (roadmap #388): declaration + dispatch ---

fn declared_computer_use() -> crate::responses::ComputerUseTool {
    crate::responses::ComputerUseTool {
        display_width: 1024,
        display_height: 768,
        environment: crate::responses::ComputerUseEnvironment::Browser,
    }
}

fn computer_use_req() -> Request {
    let mut req = user_req(vec![], CompatibilityMode::Lossy);
    req.responses = Some(
        crate::responses::ResponsesOptions::default().with_computer_use(declared_computer_use()),
    );
    req
}

#[test]
fn computer_use_declaration_matches_the_documented_wire_tool() {
    let model = make_test_model(true);
    let parts = build_request(&model, &computer_use_req()).unwrap();
    assert_eq!(
        body_of(&parts)["tools"],
        serde_json::json!([{
            "type": "computer_use_preview",
            "display_width": 1024,
            "display_height": 768,
            "environment": "browser"
        }])
    );

    // The declaration composes with ordinary function tools.
    let mut req = computer_use_req();
    req.tools = vec![ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "grep".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let body = body_of(&build_request(&model, &req).unwrap());
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2, "function tools plus the computer tool");
    assert_eq!(tools[1]["type"], "computer_use_preview");
}

#[test]
fn computer_use_fails_closed_on_a_profile_that_does_not_declare_it() {
    let model = with_responses_profile(&make_test_model(true), ResponsesRuntimeProfile::Codex);
    assert!(!model
        .endpoint
        .runtime
        .responses_profile
        .accepts_computer_use());
    let err = match build_request(&model, &computer_use_req()) {
        Err(err) => err,
        Ok(_) => panic!("expected a fail-closed computer-use error"),
    };
    assert!(
        matches!(
            err,
            AiError::Unsupported(crate::error::UnsupportedError::ComputerUse)
        ),
        "expected a fail-closed computer-use error, got {err:?}"
    );
}

#[test]
fn computer_use_is_absent_unless_the_caller_declares_it() {
    let model = make_test_model(true);
    let parts = build_request(&model, &user_req(vec![], CompatibilityMode::Lossy)).unwrap();
    let body = body_of(&parts);
    let wire = body.to_string();
    assert!(!wire.contains("computer_use_preview"), "{wire}");
    assert!(!wire.contains("computer_call"), "{wire}");
}

fn computer_tool_result(tool_call_id: &str, image: Option<Media>) -> Message {
    Message::User(UserMessage {
        content: vec![UserPart::ToolResult(crate::types::ToolResult {
            tool_call_id: ToolCallId(tool_call_id.to_owned()),
            content: image
                .into_iter()
                .map(ToolResultPart::Media)
                .chain(std::iter::once(ToolResultPart::Text(
                    "no screenshot authority".to_owned(),
                )))
                .collect(),
            is_error: false,
            added_tool_names: None,
        })],
    })
}

fn computer_call_message(arguments_json: &str) -> Message {
    let model = make_test_model(true);
    Message::Assistant(crate::types::AssistantMessage {
        content: vec![AssistantPart::ToolCall(crate::types::ToolCall {
            async_execution: false,
            id: ToolCallId("call_comp_1".to_owned()),
            name: COMPUTER_TOOL_NAME.to_owned(),
            arguments_json: arguments_json.to_owned(),
            argument_error: None,
        })],
        model: model.spec.id.clone(),
        protocol: Protocol::OpenAiResponses,
    })
}

#[test]
fn computer_call_history_replays_as_computer_call_and_output() {
    let model = make_test_model(true);
    let mut req = user_req(vec![], CompatibilityMode::Strict);
    req.messages = vec![
        computer_call_message(r#"{"action":{"type":"screenshot"}}"#),
        computer_tool_result(
            "call_comp_1",
            Some(Media::Image(ImageMedia {
                source: ImageSource::Inline(bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\n")),
                media_type: Some(mime::IMAGE_PNG),
                detail: None,
            })),
        ),
    ];
    let body = body_of(&build_request(&model, &req).unwrap());
    let input = body["input"].as_array().unwrap();
    let rendered = serde_json::to_string(input).unwrap();

    // The assistant turn replays as a computer_call item, not a function
    // call the route never declared.
    let call_index = input
        .iter()
        .position(|item| item["type"] == "computer_call")
        .unwrap_or_else(|| panic!("no computer_call item in {rendered}"));
    assert_eq!(input[call_index]["call_id"], "call_comp_1");
    assert_eq!(input[call_index]["action"]["type"], "screenshot");

    // The caller's result replays as computer_call_output with the single
    // documented computer_screenshot object.
    let output_index = input
        .iter()
        .position(|item| item["type"] == "computer_call_output")
        .unwrap_or_else(|| panic!("no computer_call_output item in {rendered}"));
    assert!(call_index < output_index, "call must precede its output");
    assert_eq!(input[output_index]["call_id"], "call_comp_1");
    assert_eq!(input[output_index]["output"]["type"], "computer_screenshot");
    assert_eq!(
        input[output_index]["output"]["image_url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
    assert!(
        !rendered.contains("function_call_output"),
        "computer results must never use the function shape: {rendered}"
    );
}

#[test]
fn computer_call_output_stays_bounded_when_no_screenshot_is_available() {
    let model = make_test_model(true);
    let mut req = user_req(vec![], CompatibilityMode::Strict);
    req.messages = vec![
        computer_call_message(r#"{"action":{"type":"wait"}}"#),
        computer_tool_result("call_comp_1", None),
    ];
    let body = body_of(&build_request(&model, &req).unwrap());
    let input = body["input"].as_array().unwrap();
    let output = input
        .iter()
        .find(|item| item["type"] == "computer_call_output")
        .expect("computer_call_output item");
    // Canonical text has no wire slot in a screenshot-only output, so the
    // item stays a well-formed, empty screenshot rather than unbounded prose
    // or a fabricated image.
    assert_eq!(
        output["output"],
        serde_json::json!({"type": "computer_screenshot"})
    );
}

#[test]
fn oversized_inline_screenshot_is_not_forwarded() {
    let model = make_test_model(true);
    let mut req = user_req(vec![], CompatibilityMode::Strict);
    req.messages = vec![
        computer_call_message(r#"{"action":{"type":"screenshot"}}"#),
        computer_tool_result(
            "call_comp_1",
            Some(Media::Image(ImageMedia {
                source: ImageSource::Inline(bytes::Bytes::from(vec![
                    0_u8;
                    MAX_COMPUTER_SCREENSHOT_BYTES
                        + 1
                ])),
                media_type: Some(mime::IMAGE_PNG),
                detail: None,
            })),
        ),
    ];
    let parts = build_request(&model, &req).unwrap();
    let body = body_of(&parts);
    let input = body["input"].as_array().unwrap();
    let output = input
        .iter()
        .find(|item| item["type"] == "computer_call_output")
        .expect("computer_call_output item");
    assert_eq!(
        output["output"],
        serde_json::json!({"type": "computer_screenshot"})
    );
}

#[test]
fn undocumented_canonical_computer_action_is_not_replayed() {
    let model = make_test_model(true);
    let mut req = user_req(vec![], CompatibilityMode::Strict);
    req.messages = vec![
        computer_call_message(r#"{"action":{"type":"shell_exec","command":"rm -rf /"}}"#),
        computer_tool_result("call_comp_1", None),
    ];
    let parts = build_request(&model, &req).unwrap();
    let rendered = body_of(&parts).to_string();
    assert!(
        !rendered.contains("shell_exec"),
        "an undocumented action must not be re-echoed as provider input: {rendered}"
    );
}

#[test]
fn opaque_replay_dispatches_computer_results_by_authoritative_output() {
    use crate::responses::{ResponsesItem, ResponsesOutput, ResponsesReplayItem};
    let model = make_test_model(true);
    let output = ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
        "id": "cc_1",
        "type": "computer_call",
        "call_id": "call_comp_1",
        "status": "completed",
        "action": {"type": "screenshot"}
    }))
    .unwrap()]);
    let input = crate::responses::encode_responses_replay(
        &model,
        None,
        &[
            ResponsesReplayItem::Output(output),
            ResponsesReplayItem::User(UserMessage {
                content: vec![UserPart::ToolResult(crate::types::ToolResult {
                    tool_call_id: ToolCallId("call_comp_1".to_owned()),
                    content: vec![ToolResultPart::Text("screenshot unavailable".to_owned())],
                    is_error: true,
                    added_tool_names: None,
                })],
            }),
        ],
    );
    let input = input.unwrap();
    let rendered = serde_json::to_string(input.items()).unwrap();
    assert!(
        rendered.contains("\"type\":\"computer_call\""),
        "{rendered}"
    );
    let output = input
        .items()
        .iter()
        .find(|item| item.as_json()["type"] == "computer_call_output")
        .unwrap_or_else(|| panic!("no computer_call_output item in {rendered}"));
    assert_eq!(output.as_json()["call_id"], "call_comp_1");
    assert_eq!(
        output.as_json()["output"],
        serde_json::json!({"type": "computer_screenshot"})
    );
    assert!(!rendered.contains("function_call_output"), "{rendered}");
}
