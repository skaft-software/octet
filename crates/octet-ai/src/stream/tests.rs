//! Unit tests for `crate::stream`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::stream`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::types::{AudioFormat, AudioMedia, AudioPayload, StopReason};
use futures_util::StreamExt;

#[tokio::test]
async fn test_response_builder_full() {
    let mut builder = ResponseBuilder::new(
        ModelId("test-model".to_string()),
        Protocol::OpenAiChat,
        None,
    );

    builder
        .on_event(&StreamEvent::Started {
            response_id: Some("resp_1".to_string()),
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::TextStart { index: 0 })
        .unwrap();
    builder
        .on_event(&StreamEvent::TextDelta {
            index: 0,
            delta: "Hello ".to_string(),
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::TextDelta {
            index: 0,
            delta: "world!".to_string(),
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::TextEnd { index: 0 })
        .unwrap();

    builder
        .on_event(&StreamEvent::MediaCompleted {
            index: 1,
            media: Media::Audio(AudioMedia {
                payload: AudioPayload::Inline(bytes::Bytes::from("voice")),
                format: AudioFormat::Wav,
                transcript: Some("hello".to_string()),
            }),
        })
        .unwrap();

    builder.set_stop_reason(StopReason::EndTurn);

    let resp = builder.finish().unwrap();
    assert_eq!(resp.response_id, Some("resp_1".to_string()));
    assert_eq!(resp.message.content.len(), 2);
    if let AssistantPart::Text(ref t) = resp.message.content[0] {
        assert_eq!(t, "Hello world!");
    } else {
        panic!("Expected Text part first");
    }
}

#[test]
fn response_builder_bounds_parts_events_and_aggregate_bytes() {
    let mut parts = ResponseBuilder::new(ModelId("m".into()), Protocol::OpenAiChat, None);
    for index in 0..MAX_RESPONSE_PARTS {
        parts.on_event(&StreamEvent::TextStart { index }).unwrap();
    }
    assert!(matches!(
        parts.on_event(&StreamEvent::TextStart {
            index: MAX_RESPONSE_PARTS
        }),
        Err(AiError::Decode(DecodeError::TooManyResponseParts))
    ));

    let mut events = ResponseBuilder::new(ModelId("m".into()), Protocol::OpenAiChat, None);
    for _ in 0..MAX_RESPONSE_EVENTS {
        events
            .on_event(&StreamEvent::Usage(Usage::default()))
            .unwrap();
    }
    assert!(matches!(
        events.on_event(&StreamEvent::Usage(Usage::default())),
        Err(AiError::Decode(DecodeError::TooManyStreamEvents))
    ));

    let mut bytes = ResponseBuilder::new(ModelId("m".into()), Protocol::OpenAiChat, None);
    bytes
        .on_event(&StreamEvent::TextStart { index: 0 })
        .unwrap();
    let chunk = "x".repeat(1024 * 1024);
    for _ in 0..64 {
        bytes
            .on_event(&StreamEvent::TextDelta {
                index: 0,
                delta: chunk.clone(),
            })
            .unwrap();
    }
    assert!(matches!(
        bytes.on_event(&StreamEvent::TextDelta {
            index: 0,
            delta: "x".into()
        }),
        Err(AiError::Decode(DecodeError::ResponseTooLarge))
    ));
}

fn opaque_reasoning_fixtures(payload: &str) -> Vec<(ReasoningState, usize)> {
    use crate::types::ReasoningStateKind;
    [
        (
            Protocol::AnthropicMessages,
            ReasoningStateKind::AnthropicSignature {
                signature: payload.into(),
            },
            payload.len(),
        ),
        (
            Protocol::AnthropicMessages,
            ReasoningStateKind::AnthropicRedacted {
                data: payload.into(),
            },
            payload.len(),
        ),
        (
            Protocol::BedrockConverse,
            ReasoningStateKind::AnthropicSignature {
                signature: payload.into(),
            },
            payload.len(),
        ),
        (
            Protocol::BedrockConverse,
            ReasoningStateKind::AnthropicRedacted {
                data: payload.into(),
            },
            payload.len(),
        ),
        (
            Protocol::OpenAiResponses,
            ReasoningStateKind::OpenAiReasoning {
                item_id: Some(payload.into()),
                encrypted_content: None,
            },
            payload.len(),
        ),
        (
            Protocol::OpenAiResponses,
            ReasoningStateKind::OpenAiReasoning {
                item_id: None,
                encrypted_content: Some(payload.into()),
            },
            payload.len(),
        ),
        (
            Protocol::OpenAiResponses,
            ReasoningStateKind::OpenAiReasoning {
                item_id: Some(payload.into()),
                encrypted_content: Some(payload.into()),
            },
            payload.len() * 2,
        ),
    ]
    .into_iter()
    .map(|(protocol, kind, bytes)| {
        (
            ReasoningState {
                protocol,
                model: ModelId("m".into()),
                kind,
            },
            bytes,
        )
    })
    .collect()
}

#[test]
fn opaque_reasoning_bounds_replacement_and_error_preservation() {
    for (((state, bytes), (larger, _)), (empty, _)) in opaque_reasoning_fixtures("é")
        .into_iter()
        .zip(opaque_reasoning_fixtures("éx"))
        .zip(opaque_reasoning_fixtures(""))
    {
        let mut builder = ResponseBuilder::new(state.model.clone(), state.protocol, None);
        // Synthetic existing content keeps boundary tests small.
        builder
            .add_content_bytes(MAX_RESPONSE_CONTENT_BYTES - bytes - 3)
            .unwrap();
        builder.reserve_buffered_content(3).unwrap();
        builder.set_reasoning_state(0, state.clone()).unwrap();
        assert_eq!(
            builder.aggregate_content_bytes,
            MAX_RESPONSE_CONTENT_BYTES - 3
        );
        let retained = serde_json::to_value(&builder.reasoning_states[&0]).unwrap();

        // Repeating the same state must not accumulate its retained bytes.
        builder.set_reasoning_state(0, state.clone()).unwrap();
        assert_eq!(
            builder.aggregate_content_bytes,
            MAX_RESPONSE_CONTENT_BYTES - 3
        );
        for (index, replacement) in [(0, larger), (1, state.clone())] {
            assert!(matches!(
                builder.set_reasoning_state(index, replacement),
                Err(AiError::Decode(DecodeError::ResponseTooLarge))
            ));
            assert_eq!(
                builder.aggregate_content_bytes,
                MAX_RESPONSE_CONTENT_BYTES - 3
            );
            assert_eq!(builder.buffered_content_bytes, 3);
            assert_eq!(builder.reasoning_states.len(), 1);
            assert_eq!(
                serde_json::to_value(&builder.reasoning_states[&0]).unwrap(),
                retained
            );
        }
        builder.set_reasoning_state(0, empty).unwrap();
        assert_eq!(
            builder.aggregate_content_bytes,
            MAX_RESPONSE_CONTENT_BYTES - bytes - 3
        );
        builder.set_reasoning_state(0, state).unwrap();
        assert_eq!(
            builder.aggregate_content_bytes,
            MAX_RESPONSE_CONTENT_BYTES - 3
        );
        assert!(matches!(
            builder.add_content_bytes(1),
            Err(AiError::Decode(DecodeError::ResponseTooLarge))
        ));
    }
}

#[test]
fn opaque_reasoning_cross_variant_replacement_releases_old_bytes() {
    let mut builder = ResponseBuilder::new(ModelId("m".into()), Protocol::OpenAiResponses, None);
    builder.add_content_bytes(7).unwrap();
    for (state, bytes) in opaque_reasoning_fixtures("opaque") {
        builder.set_reasoning_state(0, state).unwrap();
        assert_eq!(builder.aggregate_content_bytes, 7 + bytes);
    }
    builder
        .set_reasoning_state(
            0,
            ReasoningState {
                model: ModelId("m".into()),
                protocol: Protocol::OpenAiResponses,
                kind: crate::types::ReasoningStateKind::OpenAiReasoning {
                    item_id: None,
                    encrypted_content: None,
                },
            },
        )
        .unwrap();
    assert_eq!(builder.aggregate_content_bytes, 7);
}

#[test]
fn opaque_reasoning_temp_buffer_transfer_is_counted_once() {
    for (state, bytes) in opaque_reasoning_fixtures("opaque") {
        let mut builder = ResponseBuilder::new(state.model.clone(), state.protocol, None);
        builder
            .add_content_bytes(MAX_RESPONSE_CONTENT_BYTES - bytes)
            .unwrap();
        builder
            .replace_temp_buffer("opaque".into(), "x".repeat(bytes))
            .unwrap();
        assert_eq!(builder.buffered_content_bytes, bytes);
        assert_eq!(builder.take_temp_buffer("opaque").unwrap().len(), bytes);
        assert_eq!(builder.buffered_content_bytes, 0);
        builder.set_reasoning_state(0, state.clone()).unwrap();
        builder.set_reasoning_state(0, state).unwrap();
        assert_eq!(builder.aggregate_content_bytes, MAX_RESPONSE_CONTENT_BYTES);
        assert!(builder.temp_buffers.is_empty());
    }
}

#[test]
fn unrepairable_tool_arguments_keep_a_marked_envelope() {
    let mut builder = ResponseBuilder::new(
        ModelId("test-model".to_string()),
        Protocol::OpenAiChat,
        None,
    );

    builder
        .on_event(&StreamEvent::ToolCallStart {
            async_execution: false,
            index: 0,
            id: ToolCallId("call_1".to_string()),
            name: "grep".to_string(),
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: "invalid-json".to_string(),
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::ToolCallEnd {
            index: 0,
            argument_error: None,
        })
        .unwrap();

    // The stop reason only decides whether unrepairable arguments are a
    // max-token truncation; without one the malformed call keeps its envelope
    // so the model can be handed a paired error result.
    builder.set_stop_reason(StopReason::ToolUse);
    let response = builder.finish().unwrap();
    let AssistantPart::ToolCall(call) = &response.message.content[0] else {
        panic!("expected the marked tool call");
    };
    assert_eq!(call.id.0, "call_1");
    assert_eq!(call.name, "grep");
    assert_eq!(call.arguments_json, "{}");
    assert_eq!(call.argument_error, Some(ToolCallArgumentError::Malformed));
    assert!(response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "malformed_tool_arguments"));
    assert!(!response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code.eq("discarded_truncated_tool_arguments")));
}

#[test]
fn strict_optional_nulls_are_omitted_in_streamed_and_completed_calls() {
    let definition = ToolDef {
        async_execution: false,
        name: "lookup".into(),
        description: String::new(),
        constrained_sampling: Some(crate::types::ConstrainedSampling::JsonSchema {
            strict: crate::types::ConstrainedSamplingStrict::Require,
        }),
        parameters: serde_json::json!({"type":"object", "properties":{
            "city":{"type":"string"}, "note":{"type":"string"},
            "nullable":{"type":["string","null"]},
            "rows":{"type":"array", "items":{"type":"object", "properties":{"optional":{"type":"integer"}}}},
            "variant":{"anyOf":[{"type":"string"},{"type":"null"}]}
        }, "required":["city"]}),
    };
    for explicit_end in [false, true] {
        for strict in [false, true] {
            let mut builder =
                ResponseBuilder::new(ModelId("test".into()), Protocol::OpenAiChat, None);
            builder
                .set_tool_definitions(std::slice::from_ref(&definition))
                .unwrap();
            builder.strict_tool_sampling = strict;
            builder
                .on_event(&StreamEvent::ToolCallStart {
                    index: 0,
                    id: ToolCallId("call".into()),
                    name: "lookup".into(),
                    async_execution: false,
                })
                .unwrap();
            builder.on_event(&StreamEvent::ToolCallArgsDelta { index:0,
                delta: serde_json::json!({"city":"Paris","note":null,"nullable":null,"rows":[{"optional":null}],"variant":null}).to_string() }).unwrap();
            if explicit_end {
                builder
                    .on_event(&StreamEvent::ToolCallEnd {
                        index: 0,
                        argument_error: None,
                    })
                    .unwrap();
            }
            builder.set_stop_reason(StopReason::ToolUse);
            let response = builder.finish().unwrap();
            let AssistantPart::ToolCall(call) = &response.message.content[0] else {
                panic!("tool call");
            };
            if strict {
                assert!(call.argument_error.is_none());
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&call.arguments_json).unwrap(),
                    serde_json::json!({"city":"Paris","nullable":null,"rows":[{}],"variant":null})
                );
            } else {
                assert_eq!(
                    call.argument_error,
                    Some(ToolCallArgumentError::SchemaMismatch)
                );
            }
        }
    }
}

#[test]
fn replacing_tool_arguments_reclaims_preview_budget_and_fails_atomically() {
    let mut builder = ResponseBuilder::new(ModelId("test".into()), Protocol::PiMessages, None);
    builder
        .on_event(&StreamEvent::ToolCallStart {
            index: 0,
            id: ToolCallId("id".into()),
            name: "t".into(),
            async_execution: false,
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: "1234".into(),
        })
        .unwrap();
    builder
        .reserve_buffered_content(MAX_RESPONSE_CONTENT_BYTES - builder.aggregate_content_bytes)
        .unwrap();
    assert!(builder.replace_tool_arguments(0, "12345".into()).is_err());
    assert_eq!(builder.tool_call_builders[&0].arguments_json, "1234");
    builder.replace_tool_arguments(0, "{}".into()).unwrap();
    builder.replace_tool_arguments(0, "1234".into()).unwrap();
    assert!(builder
        .replace_tool_arguments(0, "x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1))
        .is_err());
    assert_eq!(builder.tool_call_builders[&0].arguments_json, "1234");
}

#[test]
fn schema_mismatch_marks_the_completed_event_and_retains_normalized_call() {
    let definitions = [ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "strict".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"],
            "additionalProperties": false,
        }),
    }];
    let mut builder = ResponseBuilder::new(
        ModelId("test-model".to_string()),
        Protocol::OpenAiChat,
        None,
    );
    builder.set_tool_definitions(&definitions).unwrap();
    let mut events = Vec::new();
    crate::protocol::emit_event(
        &mut events,
        &mut builder,
        StreamEvent::ToolCallStart {
            async_execution: false,
            index: 0,
            id: ToolCallId("call-canonical".to_owned()),
            name: "strict".to_owned(),
        },
    )
    .unwrap();
    crate::protocol::emit_event(
        &mut events,
        &mut builder,
        StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: r#"{"unexpected":"provider-secret","count":"bad"}"#.to_owned(),
        },
    )
    .unwrap();
    crate::protocol::emit_event(
        &mut events,
        &mut builder,
        StreamEvent::ToolCallEnd {
            index: 0,
            argument_error: None,
        },
    )
    .unwrap();

    assert!(matches!(
        events.last(),
        Some(StreamEvent::ToolCallEnd {
            argument_error: Some(ToolCallArgumentError::SchemaMismatch),
            ..
        })
    ));
    builder.set_stop_reason(StopReason::ToolUse);
    let response = builder.finish().unwrap();
    let AssistantPart::ToolCall(call) = &response.message.content[0] else {
        panic!("expected retained tool call");
    };
    assert_eq!(call.id.0, "call-canonical");
    assert_eq!(
        call.arguments_json,
        r#"{"count":"bad","unexpected":"provider-secret"}"#
    );
    assert_eq!(
        call.argument_error,
        Some(ToolCallArgumentError::SchemaMismatch)
    );
}

#[test]
fn max_token_response_retains_call_envelope_without_guessing_truncated_arguments() {
    let mut builder = ResponseBuilder::new(
        ModelId("test-model".to_string()),
        Protocol::OpenAiChat,
        None,
    );
    builder
        .on_event(&StreamEvent::ToolCallStart {
            async_execution: false,
            index: 0,
            id: ToolCallId("call_truncated".to_string()),
            name: "write".to_string(),
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: r#"{"path":"src/main.rs","content":"unterminated"#.to_string(),
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::ToolCallEnd {
            index: 0,
            argument_error: None,
        })
        .unwrap();
    builder.set_stop_reason(StopReason::MaxTokens);

    let response = builder.finish().unwrap();
    assert_eq!(response.stop_reason, StopReason::MaxTokens);
    let AssistantPart::ToolCall(call) = &response.message.content[0] else {
        panic!("expected retained tool call");
    };
    assert_eq!(call.id.0, "call_truncated");
    assert_eq!(call.name, "write");
    assert_eq!(call.arguments_json, "{}");
    assert!(response
        .diagnostics
        .iter()
        .any(|diagnostic| { diagnostic.code == "discarded_truncated_tool_arguments" }));
}

#[test]
fn finish_mut_keeps_progress_when_strict_tool_argument_validation_fails() {
    // Untrusted argument values have their own validation budget: a provider
    // array beyond it is a fatal decode failure, and `finish_mut` must return it
    // without consuming the builder that still holds the stream-progress
    // counters the transport annotates onto the failure.
    let definitions = [ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "write".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"lines": {"type": "array", "items": {"type": "integer"}}},
            "required": ["lines"],
            "additionalProperties": false,
        }),
    }];
    let mut builder = ResponseBuilder::new(
        ModelId("test-model".to_string()),
        Protocol::OpenAiChat,
        None,
    );
    builder.observe_provider_stream_event().unwrap();
    builder.set_tool_definitions(&definitions).unwrap();
    builder
        .on_event(&StreamEvent::ToolCallStart {
            async_execution: false,
            index: 0,
            id: ToolCallId("call_bad".to_string()),
            name: "write".to_string(),
        })
        .unwrap();
    builder
        .on_event(&StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: serde_json::json!({"lines": vec![1; 5_000]}).to_string(),
        })
        .unwrap();
    builder.set_stop_reason(StopReason::ToolUse);

    assert!(builder.finish_mut().is_err());
    assert_eq!(builder.provider_event_count, 1);
    assert!(builder.event_count >= 2);
    assert!(builder.aggregate_content_bytes > 0);
}

#[tokio::test]
async fn test_response_builder_oversized_args() {
    let mut builder = ResponseBuilder::new(
        ModelId("test-model".to_string()),
        Protocol::OpenAiChat,
        None,
    );

    builder
        .on_event(&StreamEvent::ToolCallStart {
            async_execution: false,
            index: 0,
            id: ToolCallId("call_1".to_string()),
            name: "grep".to_string(),
        })
        .unwrap();

    let delta = "x".repeat(16 * 1024 * 1024 + 1);
    let res = builder.on_event(&StreamEvent::ToolCallArgsDelta { index: 0, delta });
    assert!(matches!(
        res,
        Err(AiError::Decode(DecodeError::ToolArgumentsTooLarge))
    ));
}

#[tokio::test]
async fn test_guard_missing_start() {
    let raw_stream = futures_util::stream::iter(vec![Ok(StreamEvent::TextStart { index: 0 })]);
    let mut guarded = guard(raw_stream);
    let res = guarded.next().await.unwrap();
    assert!(matches!(
        res,
        Err(AiError::StreamProtocol(StreamProtocolError::MissingStart))
    ));
}

#[tokio::test]
async fn test_guard_rejects_lifecycle_before_start() {
    let raw_stream = futures_util::stream::iter(vec![Ok(StreamEvent::ProviderLifecycle(
        ProviderLifecycle {
            state: ProviderLifecycleState::Loading,
            detail: Some("warming".into()),
        },
    ))]);
    let mut guarded = guard(raw_stream);
    let res = guarded.next().await.unwrap();
    assert!(matches!(
        res,
        Err(AiError::StreamProtocol(StreamProtocolError::MissingStart))
    ));
}

#[tokio::test]
async fn test_guard_duplicate_start() {
    let raw_stream = futures_util::stream::iter(vec![
        Ok(StreamEvent::Started { response_id: None }),
        Ok(StreamEvent::Started { response_id: None }),
    ]);
    let mut guarded = guard(raw_stream);
    let _started = guarded.next().await.unwrap();
    let res = guarded.next().await.unwrap();
    assert!(matches!(
        res,
        Err(AiError::StreamProtocol(StreamProtocolError::DuplicateStart))
    ));
}

#[tokio::test]
async fn test_drop_cancels_inner_stream() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct DropStream {
        yielded: bool,
        dropped: Arc<AtomicBool>,
    }
    impl futures_core::Stream for DropStream {
        type Item = Result<StreamEvent, AiError>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            if self.yielded {
                std::task::Poll::Pending
            } else {
                self.yielded = true;
                std::task::Poll::Ready(Some(Ok(StreamEvent::Started { response_id: None })))
            }
        }
    }
    impl Drop for DropStream {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let mut guarded = guard(DropStream {
        yielded: false,
        dropped: dropped.clone(),
    });
    assert!(matches!(
        guarded.next().await,
        Some(Ok(StreamEvent::Started { .. }))
    ));
    drop(guarded);
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn canonical_assembler_keeps_final_response_host_owned() {
    let mut assembler = CanonicalStreamAssembler::new(
        ModelId("host-model".to_owned()),
        Protocol::OpenAiChat,
        None,
        &[],
    )
    .expect("valid assembler");
    assert!(matches!(
        assembler.push(StreamEvent::TextStart { index: 0 }),
        Err(AiError::StreamProtocol(StreamProtocolError::MissingStart))
    ));
    assembler
        .push(StreamEvent::Started {
            response_id: Some("response-1".to_owned()),
        })
        .expect("started");
    assembler
        .push(StreamEvent::ProviderLifecycle(ProviderLifecycle {
            state: ProviderLifecycleState::Loading,
            detail: Some("warming".to_owned()),
        }))
        .expect("lifecycle feedback");
    assembler
        .push(StreamEvent::TextStart { index: 0 })
        .expect("text start");
    assembler
        .push(StreamEvent::TextDelta {
            index: 0,
            delta: "hello".to_owned(),
        })
        .expect("text delta");
    assembler
        .push(StreamEvent::TextEnd { index: 0 })
        .expect("text end");
    let response = assembler.finish(StopReason::EndTurn).expect("finished");
    assert_eq!(response.response_id.as_deref(), Some("response-1"));
    assert!(matches!(
        response.message.content.as_slice(),
        [AssistantPart::Text(text)] if text == "hello"
    ));
    assert!(matches!(
        assembler.push(StreamEvent::Started { response_id: None }),
        Err(AiError::StreamProtocol(
            StreamProtocolError::EventAfterFinish
        ))
    ));
}

#[tokio::test]
async fn test_guard_event_after_finish() {
    let raw_stream = futures_util::stream::iter(vec![
        Ok(StreamEvent::Started { response_id: None }),
        Ok(StreamEvent::Finished(Response {
            message: AssistantMessage {
                content: vec![],
                model: ModelId("m".to_string()),
                protocol: Protocol::OpenAiChat,
            },
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            cost: None,
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: None,
            diagnostics: vec![],
        })),
        Ok(StreamEvent::TextStart { index: 0 }),
    ]);
    let mut guarded = guard(raw_stream);
    let _started = guarded.next().await.unwrap();
    let _finished = guarded.next().await.unwrap();
    let res = guarded.next().await.unwrap();
    assert!(matches!(
        res,
        Err(AiError::StreamProtocol(
            StreamProtocolError::EventAfterFinish
        ))
    ));
}
