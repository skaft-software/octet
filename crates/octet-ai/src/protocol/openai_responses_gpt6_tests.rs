//! Deterministic GPT-6 wire foundations; these tests do not qualify a live route.
use super::*;
use crate::{AssistantMessage, CompatibilityMode, ModelId, UserMessage};
use crate::{
    ResponsesConfigurationUpdate, ResponsesFeatures, ResponsesInput, ResponsesItem,
    ResponsesReplayItem,
};
use std::sync::Arc;

fn model() -> crate::Model {
    let mut model = crate::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-6-astra".into()))
        .unwrap();
    let features = ResponsesFeatures {
        async_tools: true,
        steering: true,
        reasoning_effort_updates: true,
        compact_reasoning_effort_updates: false,
    };
    Arc::make_mut(&mut model.spec)
        .capabilities
        .responses_features = features;
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_features = features;
    model
}

fn request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".into())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Effort(crate::ReasoningEffort::Low),
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: crate::OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: None,
    }
}

#[test]
fn public_gpt6_cache_options_encode_documented_mode_and_ttl() {
    for name in ["gpt-6-astra", "gpt-6-sol", "gpt-6-luna"] {
        let model = crate::ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId(name.into()))
            .unwrap();
        let mut req = request();
        req.reasoning = if name == "gpt-6-astra" {
            ReasoningConfig::Effort(crate::ReasoningEffort::Low)
        } else {
            ReasoningConfig::Off
        };
        req.session_id = Some("cache-key".into());
        for (retention, expected_options) in [
            (CacheRetention::Short, None),
            (CacheRetention::Long, Some(serde_json::json!({"ttl":"30m"}))),
            (CacheRetention::None, Some(serde_json::json!({"mode":"explicit"}))),
        ] {
            req.cache_retention = retention;
            let body: serde_json::Value =
                serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
            assert_eq!(body.get("prompt_cache_options").cloned(), expected_options, "{name}");
            assert!(body.get("prompt_cache_retention").is_none(), "{name}");
            assert_eq!(
                body.get("prompt_cache_key")
                    .and_then(serde_json::Value::as_str),
                (retention != CacheRetention::None).then_some("cache-key"),
                "{name}"
            );
        }
    }
}

#[test]
fn cache_options_are_not_assumed_for_compatible_or_subscription_routes() {
    let mut model = model();
    let req = request();
    Arc::make_mut(&mut model.endpoint).base_url =
        url::Url::parse("https://gateway.example/v1/").unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert!(body.get("prompt_cache_options").is_none());

    Arc::make_mut(&mut model.endpoint).base_url =
        url::Url::parse("https://api.openai.com/v1/").unwrap();
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        crate::ResponsesRuntimeProfile::Codex;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert!(body.get("prompt_cache_options").is_none());

    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        crate::ResponsesRuntimeProfile::Default;
    Arc::make_mut(&mut model.spec).api_name = "gpt-5.4".into();
    Arc::make_mut(&mut model.spec).cache.supports_explicit_prompt_cache_mode = false;
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert!(body.get("prompt_cache_options").is_none());
}

fn tool() -> ToolDef {
    ToolDef {
        async_execution: true,
        name: "lookup".into(),
        description: "Lookup".into(),
        parameters: serde_json::json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"],"additionalProperties":false}),
        constrained_sampling: None,
    }
}
fn call(model: &crate::Model, asynchronous: bool) -> Message {
    Message::Assistant(AssistantMessage {
        model: model.spec.id.clone(),
        protocol: Protocol::OpenAiResponses,
        content: vec![AssistantPart::ToolCall(crate::ToolCall {
            async_execution: asynchronous,
            id: ToolCallId("call_lookup".into()),
            name: "lookup".into(),
            arguments_json: r#"{"city":"Paris"}"#.into(),
            argument_error: None,
        })],
    })
}
fn update(effort: crate::ReasoningEffort) -> ResponsesConfigurationUpdate {
    ResponsesConfigurationUpdate {
        reasoning: ReasoningConfig::Effort(effort),
    }
}
fn user_item() -> ResponsesItem {
    ResponsesItem::new(serde_json::json!({"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]})).unwrap()
}

#[test]
fn route_authority_requires_both_declarations_not_lite_or_v2() {
    let mut model = model();
    assert!(model.responses_features().async_tools);
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_features = ResponsesFeatures::default();
    Arc::make_mut(&mut model.spec).capabilities.responses_lite = true;
    Arc::make_mut(&mut model.spec).capabilities.agent_delegation = Some(crate::AgentDelegation::V2);
    assert_eq!(model.responses_features(), ResponsesFeatures::default());
    let mut req = request();
    req.tools.push(tool());
    assert!(build_request(&model, &req).is_err());
    let old: crate::ToolCall = serde_json::from_value(
        serde_json::json!({"id":"call","name":"lookup","arguments_json":"{}"}),
    )
    .unwrap();
    assert!(!old.async_execution);
    assert!(serde_json::to_value(old).unwrap().get("async").is_none());
    let old: ToolDef = serde_json::from_value(
        serde_json::json!({"name":"lookup","description":"","parameters":{}}),
    )
    .unwrap();
    assert!(!old.async_execution);
}

#[test]
fn async_function_and_custom_definitions_and_calls_preserve_wire_marker() {
    let mut model = model();
    let mut req = request();
    req.tools.push(tool());
    Arc::make_mut(&mut model.spec)
        .preset
        .supports_openai_grammar_tools = Some(true);
    req.messages.push(call(&model, true));
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["tools"][0]["async"], true);
    assert_eq!(body["input"][1]["async"], true);
    req.tools[0].parameters = serde_json::json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]});
    req.tools[0].constrained_sampling = Some(crate::ConstrainedSampling::Grammar {
        variants: crate::GrammarVariants {
            openai_regex: Some(".*".into()),
            ..Default::default()
        },
    });
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["tools"][0]["type"], "custom");
    assert_eq!(body["tools"][0]["async"], true);
    assert_eq!(body["input"][1]["type"], "custom_tool_call");
    assert_eq!(body["input"][1]["async"], true);
}

#[test]
fn only_advertised_validated_async_calls_can_remain_pending_across_turns() {
    let model = model();
    let mut req = request();
    req.tools.push(tool());
    req.messages.push(call(&model, true));
    req.messages.push(Message::Assistant(AssistantMessage {
        model: model.spec.id.clone(),
        protocol: Protocol::OpenAiResponses,
        content: vec![AssistantPart::Text("Independent answer".into())],
    }));
    assert!(build_request(&model, &req).is_ok());
    req.messages[1] = call(&model, false);
    assert!(build_request(&model, &req).is_err());
    req.messages[1] = call(&model, true);
    req.tools[0].async_execution = false;
    assert!(build_request(&model, &req).is_err());
    req.tools[0].async_execution = true;
    req.messages.push(call(&model, true));
    assert!(build_request(&model, &req).is_err()); // Duplicate call IDs remain strict.
    req.messages.pop();
    if let Message::Assistant(assistant) = &mut req.messages[1] {
        if let AssistantPart::ToolCall(call) = &mut assistant.content[0] {
            call.arguments_json = "{}".into();
        }
    }
    assert!(build_request(&model, &req).is_err()); // Schema-invalid call cannot stay pending.
}

#[test]
fn ordered_updates_preserve_baseline_and_effective_reasoning() {
    let model = model();
    let mut req = request();
    let input = encode_replay_input(
        &model,
        None,
        &[
            ResponsesReplayItem::User(UserMessage {
                content: vec![UserPart::Text("first".into())],
            }),
            ResponsesReplayItem::ConfigurationUpdate(update(crate::ReasoningEffort::High)),
            ResponsesReplayItem::User(UserMessage {
                content: vec![UserPart::Text("next".into())],
            }),
        ],
    );
    let input = input.unwrap();
    assert_eq!(
        input.effective_reasoning(&req.reasoning).unwrap(),
        ReasoningConfig::Effort(crate::ReasoningEffort::High)
    );
    req.responses = Some(crate::ResponsesOptions::full_replay(input));
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(
        body["input"][1],
        serde_json::json!({"type":"configuration_update","reasoning":{"effort":"high"}})
    );
}

#[test]
fn updates_reject_unsupported_efforts_adjacency_and_unqualified_compact() {
    let mut model = model();
    let baseline = request().reasoning;
    let valid = ResponsesInput::new(vec![
        update(crate::ReasoningEffort::High).to_item(),
        user_item(),
    ]);
    assert!(crate::validate_responses_input(&model, &valid, &baseline, false).is_ok());
    assert!(crate::validate_responses_input(&model, &valid, &baseline, true).is_err());
    let adjacent = ResponsesInput::new(vec![
        update(crate::ReasoningEffort::High).to_item(),
        update(crate::ReasoningEffort::Low).to_item(),
    ]);
    assert!(crate::validate_responses_input(&model, &adjacent, &baseline, false).is_err());
    for reasoning in [
        ReasoningConfig::Off,
        ReasoningConfig::On,
        ReasoningConfig::Budget(100),
        ReasoningConfig::Effort(crate::ReasoningEffort::Ultra),
    ] {
        let invalid = ResponsesInput::new(vec![
            ResponsesConfigurationUpdate { reasoning }.to_item(),
            user_item(),
        ]);
        assert!(crate::validate_responses_input(&model, &invalid, &baseline, false).is_err());
    }
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_features
        .reasoning_effort_updates = false;
    assert!(crate::validate_responses_input(&model, &valid, &baseline, false).is_err());
}

#[test]
fn codex_update_authority_is_independent_of_lite_v2_and_async() {
    let mut model = model();
    let mut req = request();
    Arc::make_mut(&mut model.spec).capabilities.responses_lite = true;
    Arc::make_mut(&mut model.spec).capabilities.agent_delegation = Some(crate::AgentDelegation::V2);
    Arc::make_mut(&mut model.spec)
        .capabilities
        .responses_features
        .async_tools = false;
    req.responses = Some(crate::ResponsesOptions::full_replay(ResponsesInput::new(
        vec![update(crate::ReasoningEffort::High).to_item(), user_item()],
    )));
    let body: serde_json::Value =
        serde_json::from_slice(&build_request(&model, &req).unwrap().body).unwrap();
    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(body["reasoning"]["context"], "all_turns");
    assert_eq!(body["input"][1]["type"], "configuration_update");
    req.responses.as_mut().unwrap().context_management =
        Some(serde_json::json!([{"type":"compaction","compact_threshold":1000}]));
    assert!(build_request(&model, &req).is_err());
}

#[test]
fn sol_luna_sampling_depends_on_effective_not_baseline_effort_and_checks_presets() {
    for name in ["gpt-6-sol", "gpt-6-luna"] {
        let mut model = model();
        let spec = Arc::make_mut(&mut model.spec);
        spec.api_name = name.into();
        spec.id = ModelId(name.into());
        spec.capabilities
            .reasoning
            .as_mut()
            .unwrap()
            .options
            .as_mut()
            .unwrap()
            .values
            .insert(0, "none".into());
        let mut req = request();
        req.temperature = Some(0.7);
        assert!(build_request(&model, &req).is_err());
        req.reasoning = ReasoningConfig::Off;
        assert!(build_request(&model, &req).is_ok());
        req.responses = Some(crate::ResponsesOptions::full_replay(ResponsesInput::new(
            vec![update(crate::ReasoningEffort::High).to_item(), user_item()],
        )));
        assert!(build_request(&model, &req).is_err());
        req.temperature = None;
        req.responses = None;
        req.reasoning = ReasoningConfig::Effort(crate::ReasoningEffort::Low);
        Arc::make_mut(&mut model.spec)
            .preset
            .sampling_params
            .insert("top_p".into(), serde_json::json!(0.9));
        assert!(build_request(&model, &req).is_err());
        Arc::make_mut(&mut model.endpoint).base_url =
            url::Url::parse("https://third-party.invalid/v1/").unwrap();
        assert!(build_request(&model, &req).is_ok());
    }
}

#[tokio::test]
async fn async_stream_marker_survives_assembly_and_steered_is_one_terminal() {
    let model = model();
    let data = br#"data: {"type":"response.created","response":{"id":"resp_1"}}

data: {"type":"response.output_item.added","output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup","async":true,"arguments":"{\"city\":\"Paris\"}"}}

data: {"type":"response.output_item.done","output_index":0,"item":{"id":"fc_1","type":"function_call","async":true}}

data: {"type":"response.incomplete","response":{"id":"resp_1","incomplete_details":{"reason":"steered"}}}

"#;
    for chunk in [0, 1, 17] {
        let events = crate::protocol::harness::drive_with_tools(
            &model,
            decode_stream_event,
            data,
            chunk,
            &[tool()],
        )
        .await
        .unwrap();
        let response = crate::protocol::harness::finished(&events);
        assert_eq!(response.stop_reason, StopReason::Steered);
        assert!(
            matches!(&response.message.content[0], AssistantPart::ToolCall(call) if call.async_execution)
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::Finished(_)))
                .count(),
            1
        );
    }
    let mut unqualified = model;
    Arc::make_mut(&mut unqualified.endpoint)
        .runtime
        .responses_features
        .async_tools = false;
    assert!(crate::protocol::harness::drive_with_tools(
        &unqualified,
        decode_stream_event,
        data,
        0,
        &[tool()]
    )
    .await
    .is_err());
}

#[test]
fn async_history_transform_never_synthesizes_or_discards_delayed_results() {
    let model = model();
    let mut req = request();
    req.tools.push(tool());
    req.messages.push(call(&model, true));
    req.messages.push(Message::Assistant(AssistantMessage {
        model: model.spec.id.clone(),
        protocol: Protocol::OpenAiResponses,
        content: vec![AssistantPart::Text("Independent answer".into())],
    }));
    let pending = crate::transform_messages(&req.messages, &model);
    assert_eq!(pending.len(), 3);
    req.messages.push(Message::User(UserMessage {
        content: vec![UserPart::ToolResult(crate::ToolResult {
            tool_call_id: ToolCallId("call_lookup".into()),
            content: vec![crate::ToolResultPart::Text("sunny".into())],
            is_error: false,
            added_tool_names: None,
        })],
    }));
    req.messages = crate::transform_messages(&req.messages, &model);
    assert_eq!(req.messages.len(), 4);
    assert!(build_request(&model, &req).is_ok());
    req.messages.push(req.messages[3].clone());
    assert!(build_request(&model, &req).is_err()); // Duplicate result is not accepted.
    req.messages.remove(1);
    req.messages.pop();
    assert!(build_request(&model, &req).is_err()); // Orphan is not accepted.
}

#[test]
fn async_call_marker_survives_assistant_frame_hydration() {
    let model = model();
    let mut encoder =
        crate::AssistantMessageFrameEncoder::new(model.spec.id.clone(), model.spec.protocol);
    let events = vec![
        StreamEvent::Started {
            response_id: Some("response".into()),
        },
        StreamEvent::ToolCallStart {
            index: 0,
            id: ToolCallId("call".into()),
            name: "lookup".into(),
            async_execution: true,
        },
        StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: "{}".into(),
        },
        StreamEvent::ToolCallEnd {
            index: 0,
            argument_error: None,
        },
    ];
    let frames = encoder.encode_all(&events).unwrap();
    let frames: Vec<crate::AssistantMessageFrame> =
        serde_json::from_str(&serde_json::to_string(&frames).unwrap()).unwrap();
    let message = crate::reduce_assistant_message_frames(&frames)
        .unwrap()
        .unwrap();
    assert!(matches!(&message.content[0], AssistantPart::ToolCall(call) if call.async_execution));
}

#[test]
fn raw_async_markers_require_authority_schema_and_pairing() {
    let model = model();
    let mut req = request();
    req.tools.push(tool());
    let mut item = serde_json::json!({"type":"function_call","call_id":"call","name":"lookup","async":true,"arguments":"{}"});
    let set_input = |req: &mut Request, items: Vec<serde_json::Value>| {
        req.responses = Some(crate::ResponsesOptions::full_replay(ResponsesInput::new(
            items
                .into_iter()
                .map(|value| ResponsesItem::new(value).unwrap())
                .collect(),
        )));
    };
    set_input(&mut req, vec![item.clone()]);
    assert!(build_request(&model, &req).is_err()); // Invalid pending arguments.
    let result =
        serde_json::json!({"type":"function_call_output","call_id":"call","output":"schema error"});
    set_input(&mut req, vec![item.clone(), result.clone()]);
    assert!(build_request(&model, &req).is_ok()); // Paired non-executing error.
    set_input(&mut req, vec![item.clone(), result.clone(), result]);
    assert!(build_request(&model, &req).is_err());
    item["arguments"] = r#"{"city":"Paris"}"#.into();
    set_input(&mut req, vec![item.clone()]);
    assert!(build_request(&model, &req).is_ok());
    item["name"] = "unadvertised".into();
    set_input(&mut req, vec![item]);
    assert!(build_request(&model, &req).is_err());
}

#[tokio::test]
async fn synchronous_fallback_call_ids_can_repeat_after_each_paired_round() {
    use crate::protocol::harness;
    let google_data = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"read\",\"args\":{\"path\":\"src/main.rs\"}}}]},\"finishReason\":\"STOP\"}]}\n\n";
    let qwen_data = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/openai_chat/qwen_xml_tool_call.sse"
    ));
    for (protocol, decoder, data, fallback) in [
        (
            Protocol::GoogleGenerativeAi,
            crate::protocol::google::decode_stream_event as harness::DecodeFn,
            google_data.as_slice(),
            "google_call_0",
        ),
        (
            Protocol::OpenAiChat,
            crate::protocol::openai_chat::decode_stream_event as harness::DecodeFn,
            qwen_data.as_slice(),
            "qwen_xml_call_1",
        ),
    ] {
        let mut model = harness::model(protocol, None);
        Arc::make_mut(&mut model.spec).capabilities.reasoning = None;
        let mut req = request();
        req.reasoning = ReasoningConfig::Off;
        req.tools = vec![ToolDef {
            name: "read".into(),
            description: "Read".into(),
            async_execution: false,
            parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
            constrained_sampling: None,
        }];
        for _ in 0..2 {
            let events = harness::drive_with_tools(&model, decoder, data, 1, &req.tools)
                .await
                .unwrap();
            let response = harness::finished(&events);
            let call = response
                .message
                .content
                .iter()
                .find_map(|part| match part {
                    AssistantPart::ToolCall(call) => Some(call),
                    _ => None,
                })
                .unwrap();
            assert_eq!(call.id.0, fallback);
            req.messages
                .push(Message::Assistant(response.message.clone()));
            req.messages.push(Message::User(UserMessage {
                content: vec![UserPart::ToolResult(crate::ToolResult {
                    tool_call_id: call.id.clone(),
                    content: vec![crate::ToolResultPart::Text("read ok".into())],
                    is_error: false,
                    added_tool_names: None,
                })],
            }));
            crate::validate::validate_request(
                &req,
                &model.spec.capabilities,
                &model.spec.limits,
                protocol,
                &model.spec.id,
                CompatibilityMode::Strict,
            )
            .unwrap();
            match protocol {
                Protocol::GoogleGenerativeAi => {
                    crate::protocol::google::build_request(&model, &req).unwrap();
                }
                Protocol::OpenAiChat => {
                    crate::protocol::openai_chat::build_request(&model, &req).unwrap();
                }
                _ => unreachable!(),
            }
        }
        req.messages.push(req.messages.last().unwrap().clone());
        assert!(crate::validate::validate_request(
            &req,
            &model.spec.capabilities,
            &model.spec.limits,
            protocol,
            &model.spec.id,
            CompatibilityMode::Strict
        )
        .is_err());
    }
}

#[test]
fn async_qualified_histories_keep_global_ids_even_after_completed_sync_calls() {
    let model = model();
    for first_async in [false, true] {
        let mut req = request();
        req.tools.push(tool());
        req.messages.push(call(&model, first_async));
        req.messages.push(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(crate::ToolResult {
                tool_call_id: ToolCallId("call_lookup".into()),
                content: vec![crate::ToolResultPart::Text("done".into())],
                is_error: false,
                added_tool_names: None,
            })],
        }));
        req.messages.push(call(&model, !first_async));
        assert!(build_request(&model, &req).is_err());
    }
}

#[tokio::test]
async fn provider_configuration_updates_are_rejected_at_every_output_boundary() {
    let item = serde_json::json!({"type":"configuration_update","id":"forged", "reasoning":{"effort":"high"}});
    let terminal_item =
        serde_json::json!({"type":"configuration_update","reasoning":{"effort":"high"}});
    for granted in [false, true] {
        let mut model = model();
        Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_features
            .reasoning_effort_updates = granted;
        for event in [
            serde_json::json!({"type":"response.output_item.added","output_index":0,"item":item}),
            serde_json::json!({"type":"response.output_item.done","output_index":0,"item":item}),
            serde_json::json!({"type":"response.completed","response":{"id":"resp_1","output":[terminal_item]}}),
            serde_json::json!({"type":"response.incomplete","response":{"id":"resp_1","output":[terminal_item],"incomplete_details":{"reason":"steered"}}}),
        ] {
            let data = format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_1\"}}}}\n\ndata: {event}\n\n");
            let error =
                crate::protocol::harness::drive(&model, decode_stream_event, data.as_bytes(), 1)
                    .await
                    .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("provider output cannot author configuration_update"),
                "{error}"
            );
        }
    }
}

#[test]
fn forged_output_replay_cannot_promote_provider_items_to_host_updates() {
    let model = model();
    let item = update(crate::ReasoningEffort::High).to_item();
    let output = crate::ResponsesOutput::new(vec![item.clone()]);
    assert!(
        serde_json::from_value::<crate::ResponsesOutput>(serde_json::json!([item.as_json()]))
            .is_err()
    );
    assert!(output.clone().into_input().is_err());
    for forged in [
        ResponsesReplayItem::Output(output.clone()),
        ResponsesReplayItem::Compacted(output),
    ] {
        assert!(crate::responses::encode_responses_replay(&model, None, &[forged]).is_err());
    }
    let input = ResponsesInput::new(vec![item]);
    crate::validate_responses_input(&model, &input, &request().reasoning, false).unwrap();
    assert_eq!(
        input.effective_reasoning(&request().reasoning).unwrap(),
        ReasoningConfig::Effort(crate::ReasoningEffort::High)
    );
    crate::responses::encode_responses_replay(
        &model,
        None,
        &[ResponsesReplayItem::ConfigurationUpdate(update(
            crate::ReasoningEffort::High,
        ))],
    )
    .unwrap();
}

#[test]
fn raw_replay_tracks_sync_async_ids_and_output_order_but_allows_delta_results() {
    let model = model();
    let mut req = request();
    req.tools.push(tool());
    let asynchronous = serde_json::json!({"type":"function_call","call_id":"call","name":"lookup","async":true,"arguments":"{\"city\":\"Paris\"}"});
    let mut synchronous = asynchronous.clone();
    synchronous.as_object_mut().unwrap().remove("async");
    let result =
        serde_json::json!({"type":"function_call_output","call_id":"call","output":"done"});
    let set_input = |req: &mut Request, items: Vec<serde_json::Value>| {
        req.responses = Some(crate::ResponsesOptions::full_replay(ResponsesInput::new(
            items
                .into_iter()
                .map(|value| ResponsesItem::new(value).unwrap())
                .collect(),
        )));
    };
    for invalid in [
        vec![asynchronous.clone(), synchronous.clone()],
        vec![synchronous.clone(), asynchronous.clone()],
        vec![result.clone(), asynchronous.clone(), result.clone()],
        vec![result.clone(), asynchronous.clone()],
        vec![result.clone(), result.clone()],
    ] {
        set_input(&mut req, invalid);
        assert!(build_request(&model, &req).is_err());
    }
    for valid in [
        vec![result.clone()], // Server-delta output for a retained prior call.
        vec![asynchronous, result.clone()],
        vec![synchronous, result],
    ] {
        set_input(&mut req, valid);
        build_request(&model, &req).unwrap();
    }
}

#[test]
fn completed_canonical_async_history_survives_tool_removal_or_advertisement_changes() {
    let model = model();
    let mut first = request();
    first.tools.push(tool());
    first.messages.push(call(&model, true));
    first.messages.push(Message::Assistant(AssistantMessage {
        model: model.spec.id.clone(),
        protocol: model.spec.protocol,
        content: vec![AssistantPart::Text("Independent work".into())],
    }));
    build_request(&model, &first).unwrap();
    let result = Message::User(UserMessage {
        content: vec![
            UserPart::ToolResult(crate::ToolResult {
                tool_call_id: ToolCallId("call_lookup".into()),
                content: vec![crate::ToolResultPart::Text("sunny".into())],
                is_error: false,
                added_tool_names: None,
            }),
            UserPart::Text("Continue without the old tool".into()),
        ],
    });
    for change in ["remove", "disable_async", "change_schema"] {
        let mut later = first.clone();
        match change {
            "remove" => later.tools.clear(),
            "disable_async" => later.tools[0].async_execution = false,
            "change_schema" => {
                later.tools[0].parameters["properties"]["city"]["type"] = "integer".into()
            }
            _ => unreachable!(),
        }
        assert!(
            build_request(&model, &later).is_err(),
            "pending call must retain authority: {change}"
        );
        later.messages.push(result.clone());
        let body: serde_json::Value =
            serde_json::from_slice(&build_request(&model, &later).unwrap().body).unwrap();
        assert_eq!(body["input"][1]["async"], true);
        assert_eq!(body["input"][1]["call_id"], "call_lookup");
        assert_eq!(body["input"][3]["call_id"], "call_lookup");
        let mut unqualified = model.clone();
        Arc::make_mut(&mut unqualified.endpoint)
            .runtime
            .responses_features
            .async_tools = false;
        assert!(build_request(&unqualified, &later).is_err());
        later.messages.push(result.clone());
        assert!(
            build_request(&model, &later).is_err(),
            "duplicate historical result: {change}"
        );
    }
    let mut foreign = first;
    foreign.messages.push(result);
    if let Message::Assistant(assistant) = &mut foreign.messages[1] {
        assistant.model = ModelId("other-route/model".into());
    }
    assert!(build_request(&model, &foreign).is_err());
}

#[test]
fn completed_opaque_async_history_survives_tool_removal_or_advertisement_changes() {
    let mut model = model();
    Arc::make_mut(&mut model.spec)
        .preset
        .supports_openai_grammar_tools = Some(true);
    for custom in [false, true] {
        let mut first = request();
        first.tools.push(tool());
        if custom {
            first.tools[0].constrained_sampling = Some(crate::ConstrainedSampling::Grammar {
                variants: crate::GrammarVariants {
                    openai_regex: Some(".*".into()),
                    ..Default::default()
                },
            });
        }
        let call = if custom {
            serde_json::json!({"type":"custom_tool_call","call_id":"call_old","name":"lookup","async":true,"input":"Paris"})
        } else {
            serde_json::json!({"type":"function_call","call_id":"call_old","name":"lookup","async":true,"arguments":"{\"city\":\"Paris\"}"})
        };
        let result = serde_json::json!({"type":if custom {"custom_tool_call_output"} else {"function_call_output"},"call_id":"call_old","output":"sunny"});
        let set_input = |request: &mut Request, values: Vec<serde_json::Value>| {
            request.responses = Some(crate::ResponsesOptions::full_replay(ResponsesInput::new(
                values
                    .into_iter()
                    .map(|value| ResponsesItem::new(value).unwrap())
                    .collect(),
            )));
        };
        set_input(&mut first, vec![call.clone()]);
        build_request(&model, &first).unwrap();
        for remove in [false, true] {
            let mut later = first.clone();
            if remove {
                later.tools.clear();
            } else {
                later.tools[0].async_execution = false;
            }
            assert!(build_request(&model, &later).is_err());
            set_input(
                &mut later,
                vec![call.clone(), result.clone(), user_item().into_json()],
            );
            let body: serde_json::Value =
                serde_json::from_slice(&build_request(&model, &later).unwrap().body).unwrap();
            assert_eq!(body["input"][0], call);
            assert_eq!(body["input"][1], result);
            let mut unqualified = model.clone();
            Arc::make_mut(&mut unqualified.endpoint)
                .runtime
                .responses_features
                .async_tools = false;
            assert!(build_request(&unqualified, &later).is_err());
            set_input(
                &mut later,
                vec![result.clone(), call.clone(), result.clone()],
            );
            assert!(build_request(&model, &later).is_err());
            set_input(
                &mut later,
                vec![call.clone(), result.clone(), result.clone()],
            );
            assert!(build_request(&model, &later).is_err());
        }
    }
}
