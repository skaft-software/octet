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
                        preparation: Default::default(),
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

#[test]
fn prefixed_gemini_versions_route_tool_result_images_without_moving_audio() {
    use crate::types::{AudioFormat, Modality};
    let mut model = model_with_api_name("vendor-gemini-2.5-flash");
    std::sync::Arc::make_mut(&mut model.spec)
        .capabilities
        .input_modalities = ModalitySet::none()
        .with(Modality::Image)
        .with(Modality::Audio);
    let call = |id: &str| {
        AssistantPart::ToolCall(ToolCall {
            async_execution: false,
            id: ToolCallId(id.into()),
            name: "read".into(),
            arguments_json: "{}".into(),
            argument_error: None,
        })
    };
    let result = |id: &str, media: Media| {
        UserPart::ToolResult(ToolResult {
            tool_call_id: ToolCallId(id.into()),
            content: vec![
                ToolResultPart::Text(format!("read {id}")),
                ToolResultPart::Media(media),
            ],
            is_error: false,
            added_tool_names: None,
            tool_name: None,
        })
    };
    let request = Request {
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("look and listen".into())],
            }),
            Message::Assistant(AssistantMessage {
                content: vec![call("call_1"), call("call_2")],
                model: ModelId("claude-test".into()),
                protocol: Protocol::AnthropicMessages,
            }),
            Message::User(UserMessage {
                content: vec![
                    result(
                        "call_1",
                        Media::image_bytes(bytes::Bytes::from_static(b"png"), mime::IMAGE_PNG),
                    ),
                    result(
                        "call_2",
                        Media::audio_bytes(bytes::Bytes::from_static(b"sound"), AudioFormat::Mp3),
                    ),
                ],
            }),
        ],
        ..base_request()
    };
    let parts = build_request(&model, &request).unwrap();
    assert!(parts.diagnostics.is_empty(), "{:?}", parts.diagnostics);
    let body: Value = serde_json::from_slice(&parts.body).unwrap();
    let turn = &body["contents"][2]["parts"];
    assert_eq!(
        turn[0]["functionResponse"]["response"]["output"],
        "read call_1"
    );
    assert_eq!(
        turn[1]["functionResponse"]["response"]["output"],
        "read call_2"
    );
    assert_eq!(
        turn[2],
        serde_json::json!({"inlineData": {"mimeType": "image/png", "data": "cG5n"}})
    );
    assert_eq!(
        turn[3],
        serde_json::json!({"inlineData": {"mimeType": "audio/mp3", "data": "c291bmQ="}})
    );
    assert_eq!(turn.as_array().unwrap().len(), 4);

    std::sync::Arc::make_mut(&mut model.spec).api_name = "vendor-gemini-4-argon".into();
    let body: Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    let parts = &body["contents"][2]["parts"];
    assert_eq!(parts[0]["functionResponse"]["id"], "call_1");
    assert_eq!(parts[0]["functionResponse"]["parts"][0], turn[2]);
    assert_eq!(parts[2], turn[3]);
    assert_eq!(parts.as_array().unwrap().len(), 3);

    // A route without the matching input modality still refuses in Strict.
    let text_only = model_with_api_name("gemini-4-argon-contributor");
    assert!(build_request(&text_only, &request).is_err());
}

fn model() -> Model {
    model_with_api_name("gemini-2.5-flash")
}

fn model_with_api_name(api_name: &str) -> Model {
    Model {
        spec: std::sync::Arc::new(ModelSpec {
            context_window_options: None,
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
fn trailing_model_turn_gets_a_wire_only_user_continuation() {
    use crate::CompatibilityMode::{Lossy, Strict};

    let model = model_with_api_name("gemini-4-argon");
    for (compatibility, trailing_user) in [
        (Strict, None),
        (Lossy, None),
        (Strict, Some(Vec::new())),
        (
            Lossy,
            Some(vec![UserPart::Media(Media::image_bytes(
                bytes::Bytes::from_static(b"png"),
                mime::IMAGE_PNG,
            ))]),
        ),
    ] {
        let mut request = Request {
            messages: vec![
                Message::User(UserMessage {
                    content: vec![UserPart::Text("Start the task.".into())],
                }),
                Message::Assistant(AssistantMessage {
                    content: vec![
                        AssistantPart::ProviderMetadata(
                            ProviderPartMetadata::GoogleThoughtSignature {
                                signature: "signed-text".into(),
                            },
                        ),
                        AssistantPart::Text("Part one.".into()),
                    ],
                    model: model.spec.id.clone(),
                    protocol: Protocol::GoogleGenerativeAi,
                }),
            ],
            compatibility,
            ..base_request()
        };
        if let Some(content) = trailing_user {
            request
                .messages
                .push(Message::User(UserMessage { content }));
        }
        let original = serde_json::to_value(&request).unwrap();
        let parts = build_request(&model, &request).unwrap();
        let body: Value = serde_json::from_slice(&parts.body).unwrap();
        // Check the encoded tail: empty or lossy-dropped user content does not
        // supply the user turn required by generateContent.
        assert_eq!(
            body["contents"],
            serde_json::json!([
                {"role": "user", "parts": [{"text": "Start the task."}]},
                {"role": "model", "parts": [{"text": "Part one.", "thoughtSignature": "signed-text"}]},
                {"role": "user", "parts": [{"text": "Continue."}]},
            ])
        );
        assert_eq!(serde_json::to_value(&request).unwrap(), original);
    }
}

// Endpoint fixtures mirror the declared Google-wire routes. No request is sent.
fn assert_route_continuation(endpoint: &str, base_url: &str) {
    let mut model = model_with_api_name("gemini-4-argon");
    std::sync::Arc::make_mut(&mut model.spec).endpoint = EndpointId(endpoint.into());
    let route = std::sync::Arc::make_mut(&mut model.endpoint);
    route.id = EndpointId(endpoint.into());
    route.base_url = url::Url::parse(base_url).unwrap();
    let request = Request {
        messages: vec![
            Message::User(UserMessage {
                content: vec![UserPart::Text("Start".into())],
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
                        signature: "c2lnLys=".into(),
                    }),
                    AssistantPart::ToolCall(ToolCall {
                        async_execution: false,
                        id: ToolCallId("call-1".into()),
                        name: "lookup".into(),
                        arguments_json: "{}".into(),
                        argument_error: None,
                    }),
                ],
                model: model.spec.id.clone(),
                protocol: Protocol::GoogleGenerativeAi,
            }),
            Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("call-1".into()),
                    content: vec![ToolResultPart::Text("result".into())],
                    is_error: false,
                    added_tool_names: None,
                    tool_name: None,
                })],
            }),
            Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("Done".into())],
                model: model.spec.id.clone(),
                protocol: Protocol::GoogleGenerativeAi,
            }),
        ],
        ..base_request()
    };
    let original = serde_json::to_value(&request).unwrap();
    let encoded = build_request(&model, &request).unwrap();
    assert_eq!(
        encoded.url.as_str(),
        format!("{base_url}models/gemini-4-argon:streamGenerateContent?alt=sse")
    );
    let body: Value = serde_json::from_slice(&encoded.body).unwrap();
    assert_eq!(
        body["contents"],
        serde_json::json!([
            {"role": "user", "parts": [{"text": "Start"}]},
            {"role": "model", "parts": [{"thoughtSignature": "c2lnLys=", "functionCall": {"id": "call-1", "name": "lookup", "args": {}}}]},
            {"role": "user", "parts": [{"functionResponse": {"id": "call-1", "name": "lookup", "response": {"output": "result"}}}]},
            {"role": "model", "parts": [{"text": "Done"}]},
            {"role": "user", "parts": [{"text": "Continue."}]}
        ]),
        "{endpoint}"
    );
    assert_eq!(serde_json::to_value(&request).unwrap(), original);
}

#[test]
fn trailing_model_continuation_on_google_route() {
    assert_route_continuation(
        "gemini",
        "https://generativelanguage.googleapis.com/v1beta/",
    );
}

#[test]
fn trailing_model_continuation_on_vertex_route() {
    assert_route_continuation("vertex", "https://us-central1-aiplatform.googleapis.com/v1/projects/placeholder/locations/us-central1/publishers/google/");
}

#[test]
fn trailing_model_continuation_on_opencode_route() {
    assert_route_continuation("opencode-google", "https://opencode.ai/zen/v1/");
}

#[test]
fn trailing_model_continuation_on_opencode_go_route() {
    assert_route_continuation("opencode-go-google", "https://opencode.ai/inference/go/v1/");
}

#[test]
fn existing_user_turn_needs_no_continuation() {
    let request = Request {
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Start the task.".into())],
        })],
        ..base_request()
    };
    let parts = build_request(&model(), &request).unwrap();
    let body: Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(
        body["contents"],
        serde_json::json!([
            {"role": "user", "parts": [{"text": "Start the task."}]},
        ])
    );
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
                    tool_name: None,
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
    assert_eq!(modern_body["contents"].as_array().unwrap().len(), 2);
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

fn streamed_parts(parts: Vec<Vec<Value>>) -> crate::types::Response {
    let stream_model = model_with_api_name("gemini-4-argon-contributor");
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    for parts in parts {
        let frame = SseEvent {
            event: None,
            data: serde_json::json!({"candidates": [{"content": {"parts": parts}}]}).to_string(),
        };
        decode_stream_event(&stream_model, &frame, &mut builder).unwrap();
    }
    decode_stream_event(
        &stream_model,
        &SseEvent {
            event: None,
            data: r#"{"candidates":[{"finishReason":"STOP"}]}"#.into(),
        },
        &mut builder,
    )
    .unwrap()
    .into_iter()
    .find_map(|event| match event {
        StreamEvent::Finished(response) => Some(response),
        _ => None,
    })
    .unwrap()
}

#[test]
fn signed_text_does_not_merge_across_a_function_call() {
    for thought in [false, true] {
        let response = streamed_parts(vec![
            vec![
                serde_json::json!({"text": "before", "thought": thought, "thoughtSignature": "sig-before"}),
            ],
            vec![
                serde_json::json!({"functionCall": {"id": "read-1", "name": "read", "args": {"path": "clip.mp3"}}, "thoughtSignature": "sig-call"}),
            ],
            vec![
                serde_json::json!({"text": "after", "thought": thought, "thoughtSignature": "sig-after"}),
            ],
        ]);
        let [AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
            signature: before_signature,
        }), before, AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
            signature: call_signature,
        }), AssistantPart::ToolCall(call), AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
            signature: after_signature,
        }), after] = response.message.content.as_slice()
        else {
            panic!("expected three separate signed parts: {response:?}");
        };
        assert_eq!(before_signature, "sig-before");
        assert_eq!(call_signature, "sig-call");
        assert_eq!(after_signature, "sig-after");
        assert_eq!(call.id.0, "read-1");
        if thought {
            assert!(
                matches!(before, AssistantPart::Reasoning(part) if part.text.as_deref() == Some("before"))
            );
            assert!(
                matches!(after, AssistantPart::Reasoning(part) if part.text.as_deref() == Some("after"))
            );
        } else {
            assert!(matches!(before, AssistantPart::Text(text) if text == "before"));
            assert!(matches!(after, AssistantPart::Text(text) if text == "after"));
        }
    }
}

#[test]
fn different_text_signatures_at_the_same_chunk_position_keep_separate_parts() {
    let response = streamed_parts(vec![
        vec![serde_json::json!({"text": "before", "thoughtSignature": "sig-before"})],
        vec![serde_json::json!({"text": "after", "thoughtSignature": "sig-after"})],
    ]);
    assert!(matches!(response.message.content.as_slice(), [
        AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature { signature: before_signature }),
        AssistantPart::Text(before),
        AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature { signature: after_signature }),
        AssistantPart::Text(after),
    ] if before_signature == "sig-before" && before == "before" && after_signature == "sig-after" && after == "after"));
}

#[test]
fn text_continuation_and_empty_terminal_signature_follow_the_active_part() {
    let response = streamed_parts(vec![
        vec![
            serde_json::json!({"text": "planning", "thought": true}),
            serde_json::json!({"text": "hello"}),
        ],
        vec![serde_json::json!({"text": " world"})],
        vec![serde_json::json!({"text": "", "thoughtSignature": "sig-text"})],
    ]);
    assert!(matches!(response.message.content.as_slice(), [
        AssistantPart::Reasoning(part),
        AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature { signature }),
        AssistantPart::Text(text),
    ] if part.text.as_deref() == Some("planning") && signature == "sig-text" && text == "hello world"));
}

#[test]
fn repeated_and_prefix_related_text_chunks_are_preserved_as_deltas() {
    for thought in [false, true] {
        let response = streamed_parts(vec![
            vec![serde_json::json!({"text": "a", "thought": thought})],
            vec![serde_json::json!({"text": "a", "thought": thought})],
            vec![serde_json::json!({"text": "aa", "thought": thought})],
            vec![serde_json::json!({"text": "a", "thought": thought})],
            vec![
                serde_json::json!({"text": "", "thought": thought, "thoughtSignature": "c2lnbmVk"}),
            ],
        ]);
        let [AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
            signature,
        }), part] = response.message.content.as_slice()
        else {
            panic!("expected one signed part: {response:?}");
        };
        assert_eq!(signature, "c2lnbmVk");
        if thought {
            assert!(matches!(part, AssistantPart::Reasoning(reasoning)
                if reasoning.text.as_deref() == Some("aaaaa")));
        } else {
            assert!(matches!(part, AssistantPart::Text(text) if text == "aaaaa"));
        }
        let mut request = base_request();
        request.messages.push(Message::Assistant(response.message));
        let contents = google_contents(
            &model_with_api_name("gemini-4-argon-contributor"),
            &request,
            &mut |audio| Ok(google_inline_audio_part(audio)),
        )
        .unwrap();
        let replay = &contents[0]["parts"][0];
        assert_eq!(replay["text"], "aaaaa");
        assert_eq!(replay["thoughtSignature"], "c2lnbmVk");
        assert_eq!(replay.get("thought"), thought.then_some(&Value::Bool(true)));
    }
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

#[test]
fn xhigh_level_models_register_and_send_xhigh() {
    for id in ["gemini-4-hydrogen", "gemini-4-neon"] {
        let mut model = model_with_api_name(id);
        let cap = crate::providers::reasoning::pinned_reasoning_capability(&serde_json::json!({
            "reasoning": true, "reasoning_options": [{"type":"effort", "values":["low","medium","high","xhigh"]}]
        }), id, model.spec.protocol, 65536).unwrap();
        std::sync::Arc::make_mut(&mut model.spec)
            .capabilities
            .reasoning = Some(cap);
        crate::catalog::validate_model_spec(&model.spec).unwrap();
        let request = Request {
            reasoning: ReasoningConfig::Effort(crate::ReasoningEffort::Xhigh),
            ..base_request()
        };
        let body: Value =
            serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
        assert_eq!(
            body["generationConfig"]["thinkingConfig"],
            serde_json::json!({"thinkingLevel":"XHIGH","includeThoughts":true})
        );
    }
}

#[test]
fn xhigh_budget_uses_high_for_validation_and_encoding() {
    let mut model = model();
    let mut cap = crate::test_fixtures::token_budget_capability();
    cap.max_effort = crate::ReasoningEffort::Xhigh;
    std::sync::Arc::make_mut(&mut model.spec)
        .capabilities
        .reasoning = Some(cap);
    let request = Request {
        reasoning: ReasoningConfig::Effort(crate::ReasoningEffort::Xhigh),
        max_output_tokens: Some(9000),
        ..base_request()
    };
    let body: Value =
        serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        serde_json::json!({"thinkingBudget":8192,"includeThoughts":true})
    );
}

fn decode_fixture(data: Value) -> Result<Vec<StreamEvent>, AiError> {
    let model = model();
    let mut builder = ResponseBuilder::new(model.spec.id.clone(), model.spec.protocol, None);
    decode_stream_event(
        &model,
        &SseEvent {
            event: None,
            data: data.to_string(),
        },
        &mut builder,
    )
}

#[test]
fn google_error_finishes_preserve_the_reason_as_other() {
    for reason in [
        "MISSING_THOUGHT_SIGNATURE",
        "MALFORMED_RESPONSE",
        "ESCALATION",
        "PUP_LIMITED_DISABLED",
        "MODEL_ARMOR",
    ] {
        let events =
            decode_fixture(serde_json::json!({"candidates":[{"finishReason":reason}]})).unwrap();
        assert!(
            matches!(events.last(), Some(StreamEvent::Finished(response)) if response.stop_reason == StopReason::Other(reason.to_owned())),
            "{reason}"
        );
    }
}

#[test]
fn prompt_block_is_permanent_but_incomplete_frame_is_not_terminal() {
    let events = decode_fixture(serde_json::json!({"candidates":[]})).unwrap();
    assert!(!events
        .iter()
        .any(|event| matches!(event, StreamEvent::Finished(_))));
    for (reason, label) in [
        ("PROHIBITED_CONTENT", "PROHIBITED_CONTENT"),
        ("private upstream text", "unknown"),
    ] {
        let Err(AiError::Provider(error)) = decode_fixture(serde_json::json!({"promptFeedback":{
            "blockReason":reason, "blockReasonMessage":"private", "safetyRatings":[{"category":"private"}]}}))
        else {
            panic!("expected provider error")
        };
        assert!(error.is_permanent());
        assert_eq!(error.message, format!("Google prompt blocked: {label}"));
    }
}
