//! Unit tests for `crate::types`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::types`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::pricing::TokenRate;
use crate::test_fixtures::{base_request, reasoning_capability};
use std::time::SystemTime;

#[test]
fn inline_media_bytes_serialize_as_base64_strings() {
    let payload = bytes::Bytes::from(vec![137u8, 80, 78, 71, 13, 10]);
    let image = Media::image_bytes(payload.clone(), mime::IMAGE_PNG);
    let json = serde_json::to_string(&image).unwrap();
    assert!(
        json.contains("\"iVBORw0K\""),
        "inline image bytes must serialize as a base64 string, got: {json}"
    );

    let audio = Media::audio_bytes(payload.clone(), AudioFormat::Wav);
    let json = serde_json::to_string(&audio).unwrap();
    assert!(
        json.contains("\"iVBORw0K\""),
        "inline audio bytes must serialize as a base64 string, got: {json}"
    );

    let both = AudioPayload::InlineWithProviderRef {
        data: payload,
        reference: ProviderMediaRef {
            protocol: Protocol::OpenAiResponses,
            id: "ref".into(),
            expires_at: None,
        },
    };
    let json = serde_json::to_string(&both).unwrap();
    assert!(
        json.contains("\"iVBORw0K\""),
        "inline-with-ref bytes must serialize as a base64 string, got: {json}"
    );
}

#[test]
fn inline_media_base64_round_trips() {
    let payload = bytes::Bytes::from(vec![0u8, 255, 128, 7]);
    let image = Media::image_bytes(payload.clone(), mime::IMAGE_PNG);
    let json = serde_json::to_string(&image).unwrap();
    let back: Media = serde_json::from_str(&json).unwrap();
    match back {
        Media::Image(image) => match image.source {
            ImageSource::Inline(data) => assert_eq!(data, payload),
            other => panic!("expected inline source, got {other:?}"),
        },
        other => panic!("expected image, got {other:?}"),
    }
}

#[test]
fn inline_media_accepts_legacy_number_array_form() {
    // Sessions written before the base64 representation stored inline
    // bytes as serde_json's default number array. They must stay readable.
    let legacy =
        r#"{"Image":{"source":{"Inline":[137,80,78,71]},"media_type":"image/png","detail":null}}"#;
    let media: Media = serde_json::from_str(legacy).unwrap();
    match media {
        Media::Image(image) => match image.source {
            ImageSource::Inline(data) => {
                assert_eq!(data, bytes::Bytes::from(vec![137u8, 80, 78, 71]))
            }
            other => panic!("expected inline source, got {other:?}"),
        },
        other => panic!("expected image, got {other:?}"),
    }
}

#[test]
fn inline_media_json_overhead_is_base64_sized() {
    let payload = bytes::Bytes::from(vec![42u8; 100 * 1024]);
    let image = Media::image_bytes(payload, mime::IMAGE_PNG);
    let json = serde_json::to_vec(&image).unwrap();
    // base64 is ~1.34x the raw size; the old number-array form was ~4x.
    assert!(
        json.len() < 100 * 1024 * 3 / 2,
        "serialized inline image is {} bytes for 102400 raw bytes",
        json.len()
    );
}

#[test]
fn prompt_cache_lifetimes_are_explicit_and_default_to_unknown() {
    let defaults = CacheCompatibility::default();
    assert_eq!(defaults.prompt_cache, PromptCacheLifetimes::default());
    let mut legacy = serde_json::to_value(&defaults).unwrap();
    legacy.as_object_mut().unwrap().remove("prompt_cache");
    let parsed: CacheCompatibility = serde_json::from_value(legacy).unwrap();
    assert_eq!(parsed.prompt_cache.short, None);
    assert_eq!(parsed.prompt_cache.long, None);

    for lifetimes in [
        serde_json::json!({}),
        serde_json::json!({"short": 300}),
        serde_json::json!({"long": 3600}),
        serde_json::json!({"short": 300, "long": 3600}),
    ] {
        let cache: CacheCompatibility =
            serde_json::from_value(serde_json::json!({"prompt_cache": lifetimes})).unwrap();
        assert_eq!(cache.prompt_cache.short, lifetimes["short"].as_u64());
        assert_eq!(cache.prompt_cache.long, lifetimes["long"].as_u64());
        let round_trip: CacheCompatibility =
            serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(cache, round_trip);
    }
    assert!(serde_json::from_str::<CacheRetention>("\"warm_short\"").is_err());
}

#[test]
fn explicit_prompt_cache_mode_is_opt_in_and_backwards_compatible() {
    let mut serialized = serde_json::to_value(CacheCompatibility::default()).unwrap();
    assert!(!serialized["supports_explicit_prompt_cache_mode"]
        .as_bool()
        .unwrap());
    serialized
        .as_object_mut()
        .unwrap()
        .remove("supports_explicit_prompt_cache_mode");
    let parsed: CacheCompatibility = serde_json::from_value(serialized).unwrap();
    assert!(!parsed.supports_explicit_prompt_cache_mode);
}

#[test]
fn test_modality_set_algebra() {
    let empty = ModalitySet::none();
    assert!(!empty.contains(Modality::Image));
    assert!(!empty.contains(Modality::Audio));

    let with_image = empty.with(Modality::Image);
    assert!(with_image.contains(Modality::Image));
    assert!(!with_image.contains(Modality::Audio));

    let with_both = with_image.with(Modality::Audio);
    assert!(with_both.contains(Modality::Image));
    assert!(with_both.contains(Modality::Audio));
}

#[test]
fn test_model_spec_serde_round_trip() {
    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("test-model".to_string()),
        endpoint: EndpointId("test-endpoint".to_string()),
        api_name: "gpt-4o-mini".to_string(),
        display_name: None,
        protocol: Protocol::OpenAiChat,
        capabilities: Capabilities {
            responses_features: Default::default(),
            input_modalities: ModalitySet::none().with(Modality::Image),
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: true,
            reasoning: Some(ReasoningCapability {
                preserves_state: false,
                ..reasoning_capability()
            }),
            responses_lite: false,
            agent_delegation: None,
            structured_output: true,
            deferred_tool_loading: false,
        },
        limits: ModelLimits {
            context_window: 128000,
            max_output_tokens: 4096,
        },
        pricing: Some(Pricing {
            input: TokenRate(15),
            output: TokenRate(60),
            cache_read: TokenRate(7),
            cache_write_5m: TokenRate(15),
            cache_write_1h: None,
            reasoning: None,
            tiers: vec![],
        }),
        cache: CacheCompatibility::default(),
    };

    let serialized = serde_json::to_string(&spec).unwrap();
    let deserialized: ModelSpec = serde_json::from_str(&serialized).unwrap();
    assert_eq!(spec.id, deserialized.id);
    assert_eq!(spec.protocol, deserialized.protocol);
    assert_eq!(
        spec.capabilities.reasoning.unwrap().control,
        ReasoningControl::Effort
    );
}

#[test]
fn test_message_serde_round_trip() {
    let now = SystemTime::now();

    // 1. User message with text, inline image, and URL image
    let user_msg = Message::User(UserMessage {
        content: vec![
            UserPart::Text("Hello".to_string()),
            UserPart::Media(Media::image_bytes(
                bytes::Bytes::from("fake_png"),
                "image/png".parse().unwrap(),
            )),
            UserPart::Media(Media::image_url(
                url::Url::parse("https://example.com/img.jpg").unwrap(),
                None,
            )),
        ],
    });

    let serialized = serde_json::to_string(&user_msg).unwrap();
    let _deserialized: Message = serde_json::from_str(&serialized).unwrap();

    // 2. User message with AudioPayload variants
    let audio_inline = Message::User(UserMessage {
        content: vec![UserPart::Media(Media::audio_bytes(
            bytes::Bytes::from("fake_wav"),
            AudioFormat::Wav,
        ))],
    });
    let serialized = serde_json::to_string(&audio_inline).unwrap();
    let _deserialized: Message = serde_json::from_str(&serialized).unwrap();

    let ref_msg = Message::User(UserMessage {
        content: vec![UserPart::Media(Media::audio_ref(
            ProviderMediaRef {
                protocol: Protocol::OpenAiChat,
                id: "ref_123".to_string(),
                expires_at: Some(now),
            },
            AudioFormat::Mp3,
        ))],
    });
    let serialized = serde_json::to_string(&ref_msg).unwrap();
    let _deserialized: Message = serde_json::from_str(&serialized).unwrap();

    // 3. Assistant message with completed audio + transcript (InlineWithProviderRef)
    let assistant_msg = Message::Assistant(AssistantMessage {
        content: vec![
            AssistantPart::Text("Here is your speech".to_string()),
            AssistantPart::Media(Media::Audio(AudioMedia {
                payload: AudioPayload::InlineWithProviderRef {
                    data: bytes::Bytes::from("speech_bytes"),
                    reference: ProviderMediaRef {
                        protocol: Protocol::OpenAiChat,
                        id: "audio_id_456".to_string(),
                        expires_at: Some(now),
                    },
                },
                format: AudioFormat::Wav,
                transcript: Some("Here is your speech".to_string()),
            })),
        ],
        model: ModelId("gpt-4o-audio".to_string()),
        protocol: Protocol::OpenAiChat,
    });
    let serialized = serde_json::to_string(&assistant_msg).unwrap();
    let deserialized: Message = serde_json::from_str(&serialized).unwrap();
    if let Message::Assistant(msg) = deserialized {
        assert_eq!(msg.model, ModelId("gpt-4o-audio".to_string()));
        assert_eq!(msg.protocol, Protocol::OpenAiChat);
    } else {
        panic!("Expected assistant message");
    }

    // 4. Assistant message with reasoning state variants
    let reasoning_msg = Message::Assistant(AssistantMessage {
        content: vec![AssistantPart::Reasoning(ReasoningPart {
            text: Some("Let's think...".to_string()),
            state: Some(ReasoningState {
                protocol: Protocol::AnthropicMessages,
                model: ModelId("claude-3-5".to_string()),
                kind: ReasoningStateKind::AnthropicSignature {
                    signature: "sig_abc".to_string(),
                },
            }),
        })],
        model: ModelId("claude-3-5".to_string()),
        protocol: Protocol::AnthropicMessages,
    });
    let serialized = serde_json::to_string(&reasoning_msg).unwrap();
    let _deserialized: Message = serde_json::from_str(&serialized).unwrap();
}

#[test]
fn test_tool_call_arguments_value() {
    let tc = ToolCall {
        async_execution: false,
        id: ToolCallId("call_1".to_string()),
        name: "grep".to_string(),
        arguments_json: r#"{"pattern": "test"}"#.to_string(),
        argument_error: None,
    };
    let parsed = tc.arguments_value().unwrap();
    assert_eq!(parsed["pattern"], "test");

    let tc_invalid = ToolCall {
        async_execution: false,
        id: ToolCallId("call_2".to_string()),
        name: "grep".to_string(),
        arguments_json: r#""just a string""#.to_string(),
        argument_error: None,
    };
    assert!(tc_invalid.arguments_value().is_err());
}

#[test]
fn test_request_serde_round_trip() {
    let req = Request {
        system: Some("sys".to_string()),
        tools: vec![ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "tool".to_string(),
            description: "desc".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        max_output_tokens: Some(10),
        temperature: Some(0.7),
        stop: vec!["\n".to_string()],
        ..base_request()
    };
    let serialized = serde_json::to_string(&req).unwrap();
    let deserialized: Request = serde_json::from_str(&serialized).unwrap();
    assert_eq!(req.system, deserialized.system);
    assert_eq!(req.stop, deserialized.stop);
}

#[test]
fn test_usage_default() {
    let usage = Usage::default();
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_write_tokens, 0);
    assert_eq!(usage.cache_write_1h_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
    assert_eq!(usage.reasoning_tokens, 0);
    assert_eq!(usage.total_tokens, 0);
}

#[test]
fn test_stop_reason_custom_serde() {
    let stop = StopReason::EndTurn;
    let ser = serde_json::to_string(&stop).unwrap();
    assert_eq!(ser, "\"end_turn\"");

    let de: StopReason = serde_json::from_str("\"stop\"").unwrap();
    assert_eq!(de, StopReason::EndTurn);

    let de_other: StopReason = serde_json::from_str("\"something_else\"").unwrap();
    assert_eq!(de_other, StopReason::Other("something_else".to_string()));

    let de_deferred: StopReason = serde_json::from_str("\"deferred\"").unwrap();
    assert_eq!(de_deferred, StopReason::Deferred);
}

#[test]
fn test_stop_reason_as_canonical_matches_serde_form() {
    assert_eq!(StopReason::EndTurn.as_canonical(), "end_turn");
    assert_eq!(StopReason::MaxTokens.as_canonical(), "max_tokens");
    assert_eq!(StopReason::ToolUse.as_canonical(), "tool_use");
    assert_eq!(StopReason::StopSequence.as_canonical(), "stop_sequence");
    assert_eq!(StopReason::Refusal.as_canonical(), "refusal");
    assert_eq!(StopReason::PauseTurn.as_canonical(), "pause_turn");
    assert_eq!(StopReason::Deferred.as_canonical(), "deferred");
    assert_eq!(
        StopReason::Other("network_error".to_string()).as_canonical(),
        "network_error"
    );
    for stop in [
        StopReason::EndTurn,
        StopReason::MaxTokens,
        StopReason::ToolUse,
        StopReason::StopSequence,
        StopReason::Refusal,
        StopReason::PauseTurn,
        StopReason::Deferred,
        StopReason::Other("network_error".to_string()),
    ] {
        let serialized = serde_json::to_string(&stop).unwrap();
        assert_eq!(serialized, format!("\"{}\"", stop.as_canonical()));
    }
}
#[test]
fn opaque_reasoning_debug_redacts_without_changing_serialized_replay() {
    for kind in [
        ReasoningStateKind::AnthropicSignature {
            signature: "OPAQUE_SIGNATURE".into(),
        },
        ReasoningStateKind::AnthropicRedacted {
            data: "OPAQUE_REDACTED".into(),
        },
        ReasoningStateKind::OpenAiReasoning {
            item_id: Some("OPAQUE_ID".into()),
            encrypted_content: Some("OPAQUE_ENCRYPTED".into()),
        },
    ] {
        let state = ReasoningState {
            protocol: Protocol::BedrockConverse,
            model: ModelId("fixture".into()),
            kind,
        };
        assert!(!format!("{state:?}").contains("OPAQUE_"));
        let encoded = serde_json::to_string(&state).unwrap();
        assert!(encoded.contains("OPAQUE_"));
        let decoded: ReasoningState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    }
}
#[test]
fn review_regression_google_metadata_debug_is_opaque_but_replay_is_exact() {
    let metadata = ProviderPartMetadata::GoogleThoughtSignature {
        signature: "SYNTHETIC_OPAQUE_MARKER".into(),
    };
    assert!(!format!("{metadata:?}").contains("SYNTHETIC_OPAQUE_MARKER"));
    let message = AssistantMessage {
        model: ModelId("fixture".into()),
        protocol: Protocol::GoogleGenerativeAi,
        content: vec![
            AssistantPart::ProviderMetadata(metadata),
            AssistantPart::Text("answer".into()),
        ],
    };
    assert!(!format!("{message:?}").contains("SYNTHETIC_OPAQUE_MARKER"));
    let encoded = serde_json::to_string(&message).unwrap();
    assert!(encoded.contains("SYNTHETIC_OPAQUE_MARKER"));
    let replay: AssistantMessage = serde_json::from_str(&encoded).unwrap();
    assert_eq!(serde_json::to_string(&replay).unwrap(), encoded);
}
