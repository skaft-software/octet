//! Unit tests for `crate::protocol::bedrock`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `bedrock.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::bedrock`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::protocol::harness;
use crate::types::Request;
use crate::CompatibilityMode;

fn frame(headers: &[(&str, &str)], payload: &Value) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(name.len() as u8);
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7);
        header_bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        header_bytes.extend_from_slice(value.as_bytes());
    }
    let payload = serde_json::to_vec(payload).unwrap();
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

#[test]
fn frame_decoder_handles_fragmented_converse_stream() {
    let bytes = [
        frame(
            &[(":message-type", "event"), (":event-type", "messageStart")],
            &json!({"role": "assistant"}),
        ),
        frame(
            &[
                (":message-type", "event"),
                (":event-type", "contentBlockDelta"),
            ],
            &json!({"contentBlockIndex": 0, "delta": {"text": "hello"}}),
        ),
        frame(
            &[
                (":message-type", "event"),
                (":event-type", "contentBlockStop"),
            ],
            &json!({"contentBlockIndex": 0}),
        ),
        frame(
            &[(":message-type", "event"), (":event-type", "messageStop")],
            &json!({"stopReason": "end_turn"}),
        ),
        frame(
            &[(":message-type", "event"), (":event-type", "metadata")],
            &json!({"usage": {"inputTokens": 3, "outputTokens": 2, "totalTokens": 5}}),
        ),
    ]
    .concat();
    let mut decoder = BedrockEventStreamDecoder::new();
    let mut messages = Vec::new();
    for chunk in bytes.chunks(7) {
        messages.extend(decoder.push(chunk).unwrap());
    }
    decoder.finish().unwrap();
    let model = harness::model(Protocol::BedrockConverse, None);
    let mut builder = ResponseBuilder::new(model.spec.id.clone(), Protocol::BedrockConverse, None);
    let mut state = BedrockStreamState::default();
    let mut events = Vec::new();
    for message in messages {
        events.extend(decode_stream_event(&model, &message, &mut builder, &mut state).unwrap());
    }
    finish_stream(&mut builder, &mut state, &mut events).unwrap();
    assert!(matches!(events.first(), Some(StreamEvent::Started { .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::TextDelta { delta, .. } if delta == "hello")));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Usage(Usage {
            input_tokens: 3,
            output_tokens: 2,
            total_tokens: 5,
            ..
        })
    )));
    assert!(matches!(events.last(), Some(StreamEvent::Finished(_))));
}

#[test]
fn frame_decoder_compacts_a_burst_and_preserves_its_partial_tail() {
    let frame = frame(&[(":message-type", "event")], &json!({"text": "burst"}));
    let mut burst = frame.repeat(256);
    burst.extend_from_slice(&frame[..9]);
    let mut decoder = BedrockEventStreamDecoder::new();
    let messages = decoder.push(&burst).unwrap();
    assert_eq!(messages.len(), 256);
    assert!(messages
        .iter()
        .all(|message| message.payload == messages[0].payload));
    assert_eq!(decoder.buffer, frame[..9]);
    assert_eq!(decoder.push(&frame[9..]).unwrap().len(), 1);
    decoder.finish().unwrap();
}

#[test]
fn frame_decoder_rejects_bad_crc() {
    let mut bytes = frame(
        &[(":message-type", "event"), (":event-type", "messageStart")],
        &json!({"role": "assistant"}),
    );
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    assert!(BedrockEventStreamDecoder::new().push(&bytes).is_err());
}

#[test]
fn frame_decoder_rejects_bad_prelude_crc_and_partial_eof() {
    let mut prelude_corrupted = frame(
        &[(":message-type", "event"), (":event-type", "messageStart")],
        &json!({"role": "assistant"}),
    );
    prelude_corrupted[8] ^= 0xff;
    assert!(BedrockEventStreamDecoder::new()
        .push(&prelude_corrupted)
        .is_err());

    let complete = frame(
        &[(":message-type", "event"), (":event-type", "messageStart")],
        &json!({"role": "assistant"}),
    );
    let mut decoder = BedrockEventStreamDecoder::new();
    assert!(decoder
        .push(&complete[..complete.len() - 1])
        .unwrap()
        .is_empty());
    assert!(decoder.finish().is_err());
}

#[test]
fn finish_stream_rejects_complete_body_without_message_stop() {
    let model = harness::model(Protocol::BedrockConverse, None);
    let mut builder = ResponseBuilder::new(model.spec.id.clone(), Protocol::BedrockConverse, None);
    let mut state = BedrockStreamState::default();
    let mut events = Vec::new();
    assert!(matches!(
        finish_stream(&mut builder, &mut state, &mut events),
        Err(AiError::Decode(DecodeError::InvalidProviderField(field)))
            if field == "Bedrock stream ended without messageStop"
    ));
    assert!(events.is_empty());
}

#[test]
fn exception_frames_preserve_type_and_message() {
    let bytes = frame(
        &[
            (":message-type", "exception"),
            (":exception-type", "ThrottlingException"),
        ],
        &json!({"message": "fixture throttled"}),
    );
    let message = BedrockEventStreamDecoder::new()
        .push(&bytes)
        .unwrap()
        .pop()
        .unwrap();
    let model = harness::model(Protocol::BedrockConverse, None);
    let mut builder = ResponseBuilder::new(model.spec.id.clone(), Protocol::BedrockConverse, None);
    let error = decode_stream_event(
        &model,
        &message,
        &mut builder,
        &mut BedrockStreamState::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        AiError::Provider(ProviderError {
            code: Some(code),
            kind: Some(kind),
            message,
            ..
        }) if code == "ThrottlingException"
            && kind == "bedrock_event_stream"
            && message == "fixture throttled"
    ));
}

#[test]
fn usage_preserves_bedrock_cache_counters_and_rejects_underflow() {
    let usage = map_usage(&json!({
        "inputTokens": 10,
        "cacheReadInputTokens": 4,
        "cacheWriteInputTokens": 2,
        "outputTokens": 5,
        "totalTokens": 17,
    }))
    .unwrap();
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.cache_read_tokens, 4);
    assert_eq!(usage.cache_write_tokens, 2);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.total_tokens, 17);
    assert!(map_usage(&json!({
        "inputTokens": 10,
        "outputTokens": 5,
        "totalTokens": 14,
    }))
    .is_err());
}
#[test]
fn request_builder_uses_converse_shape() {
    let model = harness::model(Protocol::BedrockConverse, None);
    let request = Request {
        system: Some("be concise".to_owned()),
        messages: vec![Message::User(crate::types::UserMessage {
            content: vec![UserPart::Text("hello".to_owned())],
        })],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(64),
        temperature: None,
        stop: Vec::new(),
        reasoning: crate::types::ReasoningConfig::Off,
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: crate::types::OutputFormat::Text,
        output_modalities: crate::types::OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::None,
        session_id: None,
    };
    let parts = build_request(&model, &request).unwrap();
    let body: Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["inferenceConfig"]["maxTokens"], 64);
    assert!(parts
        .url
        .path()
        .ends_with("/model/fixture-api-name/converse-stream"));
}

fn thinking_model() -> crate::catalog::Model {
    let mut model = harness::model(Protocol::BedrockConverse, None);
    let spec = std::sync::Arc::make_mut(&mut model.spec);
    spec.capabilities.structured_output = false;
    spec.capabilities.reasoning = Some(crate::types::ReasoningCapability {
        options: Some(crate::types::ReasoningOptions {
            values: vec!["none".into(), "low".into(), "high".into()],
            default: Some("low".into()),
        }),
        control: crate::types::ReasoningControl::TokenBudget,
        exposes_text: true,
        preserves_state: true,
        effort_budgets: Some(crate::types::ReasoningEffortBudgets {
            minimal: 1024,
            low: 1536,
            medium: 2048,
            high: 3072,
            xhigh: 4096,
            max: 6144,
        }),
        openai_chat_mode: crate::types::OpenAiChatReasoningMode::Standard,
        min_effort: crate::types::ReasoningEffort::Low,
        max_effort: crate::types::ReasoningEffort::High,
    });
    model
}

fn thinking_request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(crate::types::UserMessage {
            content: vec![UserPart::Text("hello".into())],
        })],
        tools: vec![crate::types::ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "echo".into(),
            description: "fixture".into(),
            parameters: json!({"type": "object"}),
        }],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(4096),
        temperature: None,
        stop: Vec::new(),
        reasoning: crate::types::ReasoningConfig::Effort(crate::types::ReasoningEffort::Low),
        reasoning_mode: crate::types::ReasoningMode::Standard,
        responses: None,
        output_format: crate::types::OutputFormat::Text,
        output_modalities: crate::types::OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: crate::types::CacheRetention::None,
        session_id: None,
    }
}

fn reasoning_tool_stream(deltas: Vec<Value>) -> Vec<(&'static str, Value)> {
    let mut events = vec![("messageStart", json!({"role": "assistant"}))];
    events.extend(deltas.into_iter().map(|delta| {
        (
            "contentBlockDelta",
            json!({"contentBlockIndex": 0, "delta": {"reasoningContent": delta}}),
        )
    }));
    events.extend([
        ("contentBlockStop", json!({"contentBlockIndex": 0})),
        ("contentBlockStart", json!({"contentBlockIndex": 1, "start": {"toolUse": {"toolUseId": "call-1", "name": "echo"}}})),
        ("contentBlockDelta", json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "{}"}}})),
        ("contentBlockStop", json!({"contentBlockIndex": 1})),
        ("messageStop", json!({"stopReason": "tool_use"})),
        ("metadata", json!({"usage": {"inputTokens": 3, "outputTokens": 2, "totalTokens": 5}})),
    ]);
    events
}

fn drive_thinking(
    events: &[(&str, Value)],
    chunk: usize,
) -> Result<crate::types::Response, AiError> {
    let model = thinking_model();
    let bytes = events
        .iter()
        .flat_map(|(kind, payload)| {
            frame(
                &[(":message-type", "event"), (":event-type", kind)],
                payload,
            )
        })
        .collect::<Vec<_>>();
    let mut decoder = BedrockEventStreamDecoder::new();
    let mut builder = ResponseBuilder::new(model.spec.id.clone(), Protocol::BedrockConverse, None);
    let mut state = BedrockStreamState::default();
    let mut output = Vec::new();
    for bytes in bytes.chunks(chunk) {
        for message in decoder.push(bytes).map_err(AiError::Decode)? {
            output.extend(decode_stream_event(
                &model,
                &message,
                &mut builder,
                &mut state,
            )?);
        }
    }
    decoder.finish().map_err(AiError::Decode)?;
    finish_stream(&mut builder, &mut state, &mut output)?;
    output
        .into_iter()
        .find_map(|event| match event {
            StreamEvent::Finished(response) => Some(response),
            _ => None,
        })
        .ok_or_else(|| invalid_provider_field("fixture missing terminal event"))
}

fn continuation(response: crate::types::Response) -> Request {
    let mut request = thinking_request();
    request.messages.push(Message::Assistant(response.message));
    request
        .messages
        .push(Message::User(crate::types::UserMessage {
            content: vec![UserPart::ToolResult(crate::types::ToolResult {
                tool_call_id: ToolCallId("call-1".into()),
                content: vec![ToolResultPart::Text("actual supplied result".into())],
                is_error: true,
                added_tool_names: None,
            })],
        }));
    request
}

#[test]
fn thinking_budgets_off_holes_and_answer_room_match_the_converse_wire() {
    let model = thinking_model();
    let mut request = thinking_request();
    let body: Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    assert_eq!(
        body["additionalModelRequestFields"]["thinking"],
        json!({"type": "enabled", "budget_tokens": 1536})
    );
    assert!(body.get("thinking").is_none());
    assert!(body.get("anthropic_version").is_none());
    request.reasoning = crate::types::ReasoningConfig::Off;
    let body: Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    assert_eq!(
        body["additionalModelRequestFields"]["thinking"],
        json!({"type": "disabled"})
    );
    for budget in [0, 1023, 4096, 4097] {
        request.reasoning = crate::types::ReasoningConfig::Budget(budget);
        assert!(build_request(&model, &request).is_err());
    }
    request.reasoning = crate::types::ReasoningConfig::Budget(1024);
    assert!(build_request(&model, &request).is_ok());
    request.reasoning =
        crate::types::ReasoningConfig::Effort(crate::types::ReasoningEffort::Medium);
    assert!(build_request(&model, &request).is_err());
    let mut nonthinking = model.clone();
    std::sync::Arc::make_mut(&mut nonthinking.spec)
        .capabilities
        .reasoning = None;
    assert!(build_request(&nonthinking, &thinking_request()).is_err());
}

#[test]
fn thinking_rejects_sampling_and_forced_tool_combinations() {
    let model = thinking_model();
    let mut request = thinking_request();
    request.temperature = Some(0.7);
    assert!(build_request(&model, &request).is_err());
    request.temperature = Some(1.0);
    assert!(build_request(&model, &request).is_ok());
    for choice in [ToolChoice::Required, ToolChoice::Named("echo".into())] {
        request.tool_choice = choice;
        assert!(build_request(&model, &request).is_err());
    }
}

#[test]
fn signed_thinking_fragments_and_supplied_tool_result_replay_without_changes() {
    let events = reasoning_tool_stream(vec![
        json!({"text": "think"}),
        json!({"text": "ing"}),
        json!({"signature": "SIGN-A"}),
        json!({"signature": "-B"}),
    ]);
    for chunk in [1, 2, 7, 31, usize::MAX] {
        let response = drive_thinking(&events, chunk).unwrap();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert!(!format!("{response:?}").contains("SIGN-A"));
        let request = continuation(response);
        let body: Value =
            serde_json::from_slice(&build_request(&thinking_model(), &request).unwrap().body)
                .unwrap();
        assert_eq!(
            body["messages"][1]["content"][0],
            json!({"reasoningContent": {"reasoningText": {"text": "thinking", "signature": "SIGN-A-B"}}})
        );
        assert_eq!(
            body["messages"][1]["content"][1]["toolUse"]["toolUseId"],
            "call-1"
        );
        assert_eq!(
            body["messages"][2]["content"][0],
            json!({"toolResult": {"toolUseId": "call-1", "content": [{"text": "actual supplied result"}], "status": "error"}})
        );
    }
}

#[test]
fn redacted_chunks_are_decoded_separately_then_reencoded_as_one_blob() {
    let events = reasoning_tool_stream(vec![
        json!({"redactedContent": "AQ=="}),
        json!({"redactedContent": "AgM="}),
    ]);
    for chunk in [1, 7, 31] {
        let request = continuation(drive_thinking(&events, chunk).unwrap());
        let body: Value =
            serde_json::from_slice(&build_request(&thinking_model(), &request).unwrap().body)
                .unwrap();
        assert_eq!(
            body["messages"][1]["content"][0],
            json!({"reasoningContent": {"redactedContent": "AQID"}})
        );
    }
}

#[test]
fn malformed_or_unsigned_reasoning_blocks_fail_before_a_finished_response() {
    for deltas in [
        vec![json!({"text": "missing signature"})],
        vec![json!({"redactedContent": "not base64!"})],
        vec![json!({"text": "mixed union", "signature": "signature"})],
        vec![
            json!({"text": "mixed blocks"}),
            json!({"redactedContent": "AQ=="}),
        ],
        vec![
            json!({"redactedContent": "AQ=="}),
            json!({"signature": "mixed"}),
        ],
        vec![json!({"signature": 7})],
    ] {
        assert!(drive_thinking(&reasoning_tool_stream(deltas), 7).is_err());
    }
}

#[test]
fn incompatible_reasoning_is_rejected_or_dropped_without_plaintext_downgrade() {
    let events = reasoning_tool_stream(vec![
        json!({"text": "private reasoning"}),
        json!({"signature": "signature"}),
    ]);
    let mut request = continuation(drive_thinking(&events, 7).unwrap());
    let Message::Assistant(assistant) = &mut request.messages[1] else {
        panic!()
    };
    let AssistantPart::Reasoning(reasoning) = &mut assistant.content[0] else {
        panic!()
    };
    reasoning.state.as_mut().unwrap().model = crate::types::ModelId("different-model".into());
    assert!(build_request(&thinking_model(), &request).is_err());
    request.compatibility = CompatibilityMode::Lossy;
    let parts = build_request(&thinking_model(), &request).unwrap();
    let body: Value = serde_json::from_slice(&parts.body).unwrap();
    assert!(!body.to_string().contains("private reasoning"));
    assert!(!body.to_string().contains("reasoningContent"));
    assert!(parts
        .diagnostics
        .iter()
        .any(|d| d.code == "dropped_reasoning_state"));
}

#[test]
fn signature_buffer_uses_the_existing_aggregate_response_limit() {
    let model = thinking_model();
    let mut builder = ResponseBuilder::new(model.spec.id.clone(), Protocol::BedrockConverse, None);
    builder
        .reserve_buffered_content(crate::stream::MAX_RESPONSE_CONTENT_BYTES)
        .unwrap();
    let message = BedrockEventStreamMessage {
        headers: [(":message-type".into(), "event".into()), (":event-type".into(), "contentBlockDelta".into())].into(),
        payload: serde_json::to_vec(&json!({"contentBlockIndex": 0, "delta": {"reasoningContent": {"signature": "one more byte"}}})).unwrap().into(),
    };
    assert!(matches!(
        decode_stream_event(
            &model,
            &message,
            &mut builder,
            &mut BedrockStreamState::default()
        ),
        Err(AiError::Decode(DecodeError::ResponseTooLarge))
    ));
}
