//! Unit tests for `crate::protocol::google`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `google.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::google`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::catalog::Model;
use crate::test_fixtures::base_request;
use crate::types::{
    AssistantMessage, Capabilities, Endpoint, EndpointId, ModalitySet, ModelId, ModelLimits,
    ModelSpec, ToolCall, ToolCallId, ToolResult, ToolResultPart, UserMessage,
};

#[test]
fn inline_audio_preserves_bytes_mime_and_part_order() {
    use crate::types::{AudioFormat, Modality};
    for (format, mime) in [
        (AudioFormat::Wav, "audio/wav"),
        (AudioFormat::Mp3, "audio/mp3"),
        (AudioFormat::Aac, "audio/aac"),
        (AudioFormat::Flac, "audio/flac"),
        (AudioFormat::Opus, "audio/ogg"),
    ] {
        let mut model = model();
        std::sync::Arc::make_mut(&mut model.spec)
            .capabilities
            .input_modalities = ModalitySet::none().with(Modality::Audio);
        let request = Request {
            messages: vec![Message::User(UserMessage {
                content: vec![
                    UserPart::Text("before".into()),
                    UserPart::Media(Media::audio_bytes(
                        bytes::Bytes::from_static(b"sound"),
                        format,
                    )),
                    UserPart::Text("after".into()),
                ],
            })],
            ..base_request()
        };
        let parts = build_request(&model, &request).unwrap();
        assert!(parts.diagnostics.is_empty());
        let body: Value = serde_json::from_slice(&parts.body).unwrap();
        assert_eq!(
            body["contents"][0]["parts"],
            serde_json::json!([
                {"text": "before"},
                {"inlineData": {"mimeType": mime, "data": "c291bmQ="}},
                {"text": "after"},
            ])
        );
    }
}

#[test]
fn unsupported_google_audio_is_rejected_or_diagnosed_and_omitted() {
    use crate::types::{AudioFormat, AudioMedia, AudioPayload, Modality, ProviderMediaRef};
    for (has_capability, payload, format) in [
        (
            false,
            AudioPayload::Inline(bytes::Bytes::from_static(b"sound")),
            AudioFormat::Mp3,
        ),
        (
            true,
            AudioPayload::Inline(bytes::Bytes::from_static(b"sound")),
            AudioFormat::Pcm16,
        ),
        (
            true,
            AudioPayload::ProviderRef(ProviderMediaRef {
                protocol: Protocol::GoogleGenerativeAi,
                id: "not-a-fetch-url".into(),
                expires_at: None,
            }),
            AudioFormat::Wav,
        ),
        (
            true,
            AudioPayload::InlineWithProviderRef {
                data: bytes::Bytes::from_static(b"sound"),
                reference: ProviderMediaRef {
                    protocol: Protocol::OpenAiChat,
                    id: "chat-only".into(),
                    expires_at: None,
                },
            },
            AudioFormat::Wav,
        ),
    ] {
        let mut model = model();
        if has_capability {
            std::sync::Arc::make_mut(&mut model.spec)
                .capabilities
                .input_modalities = ModalitySet::none().with(Modality::Audio);
        }
        let mut request = Request {
            messages: vec![Message::User(UserMessage {
                content: vec![
                    UserPart::Text("listen".into()),
                    UserPart::Media(Media::Audio(AudioMedia {
                        payload,
                        format,
                        transcript: None,
                    })),
                ],
            })],
            ..base_request()
        };
        assert!(build_request(&model, &request).is_err());
        request.compatibility = crate::CompatibilityMode::Lossy;
        let parts = build_request(&model, &request).unwrap();
        assert!(!parts.diagnostics.is_empty());
        let body: Value = serde_json::from_slice(&parts.body).unwrap();
        assert_eq!(
            body["contents"][0]["parts"],
            serde_json::json!([{"text": "listen"}])
        );
    }
}

fn model() -> Model {
    model_with_api_name("gemini-2.5-flash")
}

fn model_with_api_name(api_name: &str) -> Model {
    Model {
        spec: std::sync::Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("gemini-test".to_owned()),
            api_name: api_name.to_owned(),
            display_name: None,
            endpoint: EndpointId("google".to_owned()),
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
            cache: Default::default(),
        }),
        endpoint: std::sync::Arc::new(Endpoint {
            id: EndpointId("google".to_owned()),
            base_url: url::Url::parse("https://example.invalid/v1beta/").unwrap(),
            auth: crate::auth::Auth::None,
            default_headers: http::HeaderMap::new(),
            transport: Default::default(),
            runtime: Default::default(),
            timeout: std::time::Duration::from_secs(10),
        }),
    }
}

#[test]
fn maps_structured_output_to_native_json_schema() {
    let mut request = Request {
        output_format: OutputFormat::JsonSchema(crate::types::JsonSchemaFormat {
            name: "answer".to_owned(),
            description: None,
            schema: serde_json::json!({"type": "object"}),
            strict: true,
        }),
        ..base_request()
    };
    let parts = build_request(&model(), &request).unwrap();
    let body: Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(
        body["generationConfig"]["responseJsonSchema"]["type"],
        "object"
    );

    let mut zen = model();
    std::sync::Arc::make_mut(&mut zen.spec)
        .cache
        .send_session_affinity_headers = true;
    std::sync::Arc::make_mut(&mut zen.endpoint).id = EndpointId("opencode-google".into());
    request.cache_retention = crate::types::CacheRetention::None;
    request.session_id = Some("zen-session".into());
    assert_eq!(
        build_request(&zen, &request).unwrap().headers["x-opencode-session"],
        "zen-session"
    );
    request.session_id = None;
    let parts = build_request(&zen, &request).unwrap();
    assert!(parts.headers.get("x-opencode-session").is_none());

    // OpenCode Go serves Gemini through the same Google route and also
    // requires the session header.
    let mut go = zen.clone();
    std::sync::Arc::make_mut(&mut go.endpoint).id = EndpointId("opencode-go-google".into());
    request.session_id = Some("go-session".into());
    assert_eq!(
        build_request(&go, &request).unwrap().headers["x-opencode-session"],
        "go-session"
    );
}

#[test]
fn uses_model_specific_google_tool_call_id_shape() {
    let request = Request {
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: ToolCallId("call-1".to_owned()),
                    name: "lookup".to_owned(),
                    arguments_json: r#"{"city":"Paris"}"#.to_owned(),
                    argument_error: None,
                })],
                model: ModelId("gemini-test".to_owned()),
                protocol: Protocol::GoogleGenerativeAi,
            }),
            Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("call-1".to_owned()),
                    content: vec![ToolResultPart::Text("Paris is sunny".to_owned())],
                    is_error: false,
                    added_tool_names: None,
                })],
            }),
        ],
        ..base_request()
    };

    let legacy = build_request(&model(), &request).unwrap();
    assert_eq!(
        legacy.url.path(),
        "/v1beta/models/gemini-2.5-flash:streamGenerateContent"
    );
    assert_eq!(legacy.url.query(), Some("alt=sse"));
    let legacy_body: Value = serde_json::from_slice(&legacy.body).unwrap();
    let legacy_call = &legacy_body["contents"][0]["parts"][0]["functionCall"];
    assert_eq!(legacy_call["name"], "lookup");
    assert!(legacy_call.get("id").is_none());
    let legacy_response = &legacy_body["contents"][1]["parts"][0]["functionResponse"];
    assert_eq!(legacy_response["response"]["output"], "Paris is sunny");
    assert!(legacy_response.get("id").is_none());

    let modern = build_request(&model_with_api_name("gemini-3-flash"), &request).unwrap();
    let modern_body: Value = serde_json::from_slice(&modern.body).unwrap();
    assert_eq!(
        modern_body["contents"][0]["parts"][0]["functionCall"]["id"],
        "call-1"
    );
    assert_eq!(
        modern_body["contents"][1]["parts"][0]["functionResponse"]["id"],
        "call-1"
    );
    assert!(!google_requires_tool_call_id("gemini-2.5-flash"));
    assert!(google_requires_tool_call_id("gemini-3-flash"));
    assert!(google_requires_tool_call_id("claude-3-7-sonnet"));
    assert!(!google_requires_tool_call_id("gemma-3-27b"));
}

#[test]
fn thought_signature_is_metadata_not_reasoning_classification() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let frame = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"text":"visible","thoughtSignature":"opaque"}]},"finishReason":"STOP"}]}"#.to_owned(),
    };
    let events = decode_stream_event(&stream_model, &frame, &mut builder).unwrap();
    let response = events
        .into_iter()
        .find_map(|event| match event {
            StreamEvent::Finished(response) => Some(response),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        response.message.content.as_slice(),
        [
            AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature { .. }),
            AssistantPart::Text(text)
        ] if text == "visible"
    ));
}

#[test]
fn incremental_argument_size_matches_recursive_merge_and_escaping() {
    let mut prior = serde_json::json!({});
    let mut size = argument_json_size(&prior).unwrap();
    for update in [
        serde_json::json!({"nested": {"a": "long prefix", "b": 1}, "array": [1, 2]}),
        serde_json::json!({"nested": {"a": "x", "quoted\"": "\n\t\"é"}}),
        serde_json::json!({"array": {"empty": {}}, "new": null}),
        serde_json::json!({"nested": false, "array": {"empty": {"x": 3}}}),
        serde_json::json!({}),
    ] {
        size = size
            .checked_add_signed(merged_size_change(&prior, &update).unwrap())
            .unwrap();
        merge_json_object(&mut prior, update);
        assert_eq!(size, serde_json::to_vec(&prior).unwrap().len());
    }
}

#[test]
fn argument_update_reserves_before_mutating_the_retained_object() {
    let mut builder =
        ResponseBuilder::new(model().spec.id.clone(), Protocol::GoogleGenerativeAi, None);
    let mut events = Vec::new();
    let call = |args| GoogleFunctionCall {
        id: Some("call_1".to_owned()),
        name: "read".to_owned(),
        args,
    };
    decode_google_function_call(
        &mut events,
        &mut builder,
        call(serde_json::json!({"path": "a"})),
        None,
    )
    .unwrap();
    let prior = builder.google_function_args[&0].clone();
    assert_eq!(builder.buffered_content_bytes, prior.1);
    assert!(!events
        .iter()
        .any(|event| matches!(event, StreamEvent::ToolCallArgsDelta { .. })));
    builder.aggregate_content_bytes = crate::stream::MAX_RESPONSE_CONTENT_BYTES - prior.1;
    assert!(matches!(
        decode_google_function_call(
            &mut events,
            &mut builder,
            call(serde_json::json!({"line": 2})),
            None
        ),
        Err(AiError::Decode(DecodeError::ResponseTooLarge))
    ));
    assert_eq!(builder.google_function_args[&0], prior);
    assert_eq!(builder.buffered_content_bytes, prior.1);
}

#[test]
fn google_argument_size_enforces_the_tool_limit_before_retention() {
    assert!(matches!(
        argument_json_size(&"x".repeat(MAX_TOOL_ARGUMENT_BYTES)),
        Err(AiError::Decode(DecodeError::ToolArgumentsTooLarge))
    ));
}

#[test]
fn merges_cumulative_function_arguments_before_finish() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let first = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_1","name":"read","args":{"path":"a"}}}]}}]}"#.to_owned(),
    };
    let last = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_1","name":"read","args":{"path":"a","line":2}}}]},"finishReason":"STOP"}]}"#.to_owned(),
    };
    decode_stream_event(&stream_model, &first, &mut builder).unwrap();
    let response = decode_stream_event(&stream_model, &last, &mut builder)
        .unwrap()
        .into_iter()
        .find_map(|event| match event {
            StreamEvent::Finished(response) => Some(response),
            _ => None,
        })
        .unwrap();
    assert!(matches!(response.stop_reason, StopReason::ToolUse));
    assert!(matches!(
        response.message.content.as_slice(),
        [AssistantPart::ToolCall(call)] if call.arguments_json == r#"{"line":2,"path":"a"}"#
    ));
}

#[test]
fn parallel_calls_in_separate_chunks_keep_their_identity_arguments_and_signatures() {
    for ids in [Some(["call_1", "call_2"]), None, Some(["", ""])] {
        let stream_model = model_with_api_name("gemini-4-argon-contributor");
        let mut builder = ResponseBuilder::new(
            stream_model.spec.id.clone(),
            Protocol::GoogleGenerativeAi,
            None,
        );
        let mut events = Vec::new();
        for (position, (name, args, signature)) in [
            ("read", serde_json::json!({"path": "first.txt"}), "sig-1"),
            ("bash", serde_json::json!({"command": "pwd"}), "sig-2"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut call = serde_json::json!({"name": name, "args": args});
            if let Some(ids) = ids {
                call["id"] = ids[position].into();
            }
            let frame = SseEvent {
                event: None,
                data: serde_json::json!({"candidates": [{"content": {"parts": [{
                    "functionCall": call, "thoughtSignature": signature
                }]}}]})
                .to_string(),
            };
            events.extend(decode_stream_event(&stream_model, &frame, &mut builder).unwrap());
        }
        let terminal = SseEvent {
            event: None,
            data: r#"{"candidates":[{"finishReason":"STOP"}]}"#.into(),
        };
        events.extend(decode_stream_event(&stream_model, &terminal, &mut builder).unwrap());
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
        let [AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
            signature: first_signature,
        }), AssistantPart::ToolCall(first), AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
            signature: second_signature,
        }), AssistantPart::ToolCall(second)] = response.message.content.as_slice()
        else {
            panic!("expected two signed calls: {response:?}");
        };
        assert_eq!(first_signature, "sig-1");
        assert_eq!(second_signature, "sig-2");
        assert_ne!(first.id, second.id);
        assert_eq!(first.name, "read");
        assert_eq!(first.arguments_json, r#"{"path":"first.txt"}"#);
        assert_eq!(second.name, "bash");
        assert_eq!(second.arguments_json, r#"{"command":"pwd"}"#);
        if let Some(ids) = ids.filter(|ids| !ids[0].is_empty()) {
            assert_eq!(first.id.0, ids[0]);
            assert_eq!(second.id.0, ids[1]);
        }
    }
}

#[test]
fn cumulative_call_snapshots_follow_ids_not_chunk_positions() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let first = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_1","name":"read","args":{"path":"a"}}},{"functionCall":{"id":"call_2","name":"read","args":{"path":"b"}}}]}}]}"#.into(),
    };
    let last = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_2","name":"read","args":{"path":"b","line":2}}},{"functionCall":{"id":"call_1","name":"read","args":{"path":"a","line":1}}}]},"finishReason":"STOP"}]}"#.into(),
    };
    decode_stream_event(&stream_model, &first, &mut builder).unwrap();
    let events = decode_stream_event(&stream_model, &last, &mut builder).unwrap();
    let response = events
        .into_iter()
        .find_map(|event| match event {
            StreamEvent::Finished(response) => Some(response),
            _ => None,
        })
        .unwrap();
    let [AssistantPart::ToolCall(first), AssistantPart::ToolCall(second)] =
        response.message.content.as_slice()
    else {
        panic!("expected two calls");
    };
    assert_eq!(first.id.0, "call_1");
    assert_eq!(first.arguments_json, r#"{"line":1,"path":"a"}"#);
    assert_eq!(second.id.0, "call_2");
    assert_eq!(second.arguments_json, r#"{"line":2,"path":"b"}"#);
}

#[test]
fn rejects_a_tool_name_change_for_the_same_call_id() {
    let mut builder =
        ResponseBuilder::new(model().spec.id.clone(), Protocol::GoogleGenerativeAi, None);
    let mut events = Vec::new();
    for (name, should_succeed) in [("read", true), ("bash", false)] {
        let result = decode_google_function_call(
            &mut events,
            &mut builder,
            GoogleFunctionCall {
                id: Some("call_1".into()),
                name: name.into(),
                args: serde_json::json!({}),
            },
            None,
        );
        if should_succeed {
            result.unwrap();
        } else {
            assert!(matches!(
                result,
                Err(AiError::Decode(DecodeError::InvalidProviderField(_)))
            ));
        }
    }
}

#[test]
fn idless_identical_calls_are_not_deduplicated() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let frame = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{}}},{"functionCall":{"name":"read","args":{}}}]}}]}"#.into(),
    };
    for _ in 0..2 {
        decode_stream_event(&stream_model, &frame, &mut builder).unwrap();
    }
    assert_eq!(builder.tool_call_builders.len(), 4);
    let ids = builder
        .tool_call_builders
        .values()
        .map(|call| &call.id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 4);
}

#[test]
fn calls_across_chunks_still_enforce_the_response_part_limit() {
    for with_id in [false, true] {
        let mut builder =
            ResponseBuilder::new(model().spec.id.clone(), Protocol::GoogleGenerativeAi, None);
        let mut events = Vec::new();
        for position in 0..=crate::stream::MAX_RESPONSE_PARTS {
            let result = decode_google_function_call(
                &mut events,
                &mut builder,
                GoogleFunctionCall {
                    id: with_id.then(|| format!("call_{position}")),
                    name: "read".into(),
                    args: serde_json::json!({}),
                },
                None,
            );
            if position < crate::stream::MAX_RESPONSE_PARTS {
                result.unwrap();
            } else {
                assert!(matches!(
                    result,
                    Err(AiError::Decode(DecodeError::TooManyResponseParts))
                ));
            }
        }
    }
}

#[test]
fn decodes_thought_usage_and_terminal_text() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let frame = SseEvent {
        event: None,
        data: r#"{"responseId":"response-1","candidates":[{"content":{"parts":[{"text":"private thought","thought":true,"thoughtSignature":"sig-1"},{"text":"visible answer"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"cachedContentTokenCount":2,"candidatesTokenCount":1,"thoughtsTokenCount":2,"totalTokenCount":0}}"#.to_owned(),
    };
    let events = decode_stream_event(&stream_model, &frame, &mut builder).unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ReasoningDelta { delta, .. } if delta == "private thought"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::TextDelta { delta, .. } if delta == "visible answer"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Usage(usage) if usage.input_tokens == 8
            && usage.cache_read_tokens == 2
            && usage.output_tokens == 3
            && usage.reasoning_tokens == 2
            && usage.total_tokens == 13
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Finished(response) if response.response_id.as_deref() == Some("response-1")
    )));
}

#[test]
fn reports_native_provider_errors_without_stream_events() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let frame = SseEvent {
        event: None,
        data: r#"{"responseId":"response-error","error":{"code":400,"status":"INVALID_ARGUMENT","message":"invalid request"}}"#.to_owned(),
    };
    let error = decode_stream_event(&stream_model, &frame, &mut builder).unwrap_err();
    assert!(matches!(error, AiError::Provider(_)));
    assert!(!builder.started);
}
