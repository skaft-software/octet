//! Synthetic wire-contract regressions, not live-provider benchmark captures.
use crate::inference::*;
use crate::protocol::{harness, openai_chat, openai_responses};
use crate::stream::StreamEvent;
use crate::types::Protocol;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Instant;

fn chat_frame(choices: Value, extra: Value) -> String {
    let mut frame = json!({"id":"metrics-response", "choices":choices});
    frame
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    format!("data: {frame}\n\n")
}

async fn chat(extra: Value) -> crate::types::Response {
    let model = harness::model(Protocol::OpenAiChat, None);
    let data = chat_frame(json!([{"delta":{"content":"answer"}}]), json!({}))
        + &chat_frame(json!([{"delta":{},"finish_reason":"stop"}]), extra)
        + "data: [DONE]\n\n";
    // Exercise arbitrary byte boundaries through the real SSE/JSON/guard path.
    let events = harness::drive(&model, openai_chat::decode_stream_event, data.as_bytes(), 1)
        .await
        .unwrap();
    harness::finished(&events).clone()
}

#[tokio::test]
async fn recognized_chat_envelopes_preserve_native_counts_and_usage() {
    for (source, extra, billed) in [
        (
            ServerTimingSource::TimingsPredicted,
            json!({"timings":{"predicted_n":100,"predicted_ms":500},"usage":{"prompt_tokens":10,"completion_tokens":150}}),
            150,
        ),
        (
            ServerTimingSource::TimeInfoCompletion,
            json!({"time_info":{"completion_time":0.5,"queue_time":0.1},"usage":{"prompt_tokens":10,"completion_tokens":100}}),
            100,
        ),
        (
            ServerTimingSource::UsageCompletion,
            json!({"usage":{"prompt_tokens":10,"completion_tokens":100,"completion_time":0.5,"prompt_time":0.2}}),
            100,
        ),
        (
            ServerTimingSource::XGroqUsageCompletion,
            json!({"x_groq":{"id":"must-not-be-retained","usage":{"completion_tokens":100,"completion_time":0.5,"total_time":0.7}},"usage":{"prompt_tokens":10,"completion_tokens":100}}),
            100,
        ),
    ] {
        let response = chat(extra).await;
        assert_eq!(response.usage.output_tokens, billed);
        let metrics = response.inference.unwrap();
        assert!(
            metrics.client.is_none(),
            "codec does not invent a client clock"
        );
        assert_eq!(metrics.server_unavailable, None);
        let server = metrics.server.unwrap();
        assert_eq!(server.source, source);
        assert_eq!(server.tokens, 100);
        assert_eq!(server.tokens_per_second(), Some(200.0));
        assert!(!serde_json::to_string(&server)
            .unwrap()
            .contains("must-not-be-retained"));
    }
}

#[tokio::test]
async fn malformed_advisory_timing_does_not_invalidate_answer_or_billing() {
    for extra in [
        json!({"timings":null}),
        json!({"timings":[{"secret":"never retain"}]}),
        json!({"timings":{"predicted_n":100,"predicted_ms":-1}}),
        json!({"timings":{"predicted_n":100,"predicted_ms":1e100}}),
        json!({"timings":{"predicted_n":1.5,"predicted_ms":500}}),
        json!({"time_info":{"completion_time":"never retain"}}),
        json!({"usage":{"prompt_tokens":10,"completion_tokens":100,"completion_time":{ "secret":"never retain" }}}),
        json!({"x_groq":{"usage":{"completion_tokens":100,"completion_time":0}}}),
    ] {
        let response = chat(extra).await;
        assert!(!response.message.content.is_empty());
        let metrics = response.inference.unwrap();
        assert_eq!(
            metrics.server_unavailable,
            Some(ServerTimingUnavailable::Invalid)
        );
        assert!(metrics.server.is_none());
        assert!(!serde_json::to_string(&metrics)
            .unwrap()
            .contains("never retain"));
    }
}

#[tokio::test]
async fn missing_timing_is_explicit_and_timestamps_are_not_decode_measurements() {
    let response = chat(json!({"created":1,"system_fingerprint":"fp","usage":{"prompt_tokens":10,"completion_tokens":100}})).await;
    let metrics = response.inference.unwrap();
    assert!(metrics.server.is_none());
    assert_eq!(
        metrics.server_unavailable,
        Some(ServerTimingUnavailable::NotReported)
    );
}

#[tokio::test]
async fn provisional_snapshot_is_not_paired_with_later_billing() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let data = chat_frame(
        json!([{"delta":{"content":"a"}}]),
        json!({"time_info":{"completion_time":0.5}}),
    ) + &chat_frame(
        json!([{"delta":{},"finish_reason":"stop"}]),
        json!({"usage":{"prompt_tokens":10,"completion_tokens":200}}),
    ) + "data: [DONE]\n\n";
    let events = harness::drive(&model, openai_chat::decode_stream_event, data.as_bytes(), 3)
        .await
        .unwrap();
    let response = harness::finished(&events);
    assert_eq!(response.usage.output_tokens, 200);
    assert_eq!(
        response.inference.as_ref().unwrap().server_unavailable,
        Some(ServerTimingUnavailable::Provisional)
    );
}

#[tokio::test]
async fn trailing_usage_and_same_source_conflicts_are_handled_without_stale_rate() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let initial = chat_frame(json!([{"delta":{"content":"a"}}]), json!({}))
        + &chat_frame(json!([{"delta":{},"finish_reason":"stop"}]), json!({}));
    let usage = json!({"prompt_tokens":10,"completion_tokens":100,"completion_time":0.5});
    let data =
        initial.clone() + &chat_frame(json!([]), json!({"usage":usage})) + "data: [DONE]\n\n";
    let events = harness::drive(&model, openai_chat::decode_stream_event, data.as_bytes(), 2)
        .await
        .unwrap();
    assert_eq!(
        harness::finished(&events)
            .inference
            .as_ref()
            .unwrap()
            .server
            .as_ref()
            .unwrap()
            .tokens_per_second(),
        Some(200.0)
    );
    let data = chat_frame(json!([{"delta":{"content":"a"}}]), json!({}))
        + &chat_frame(
            json!([{"delta":{},"finish_reason":"stop"}]),
            json!({"timings":{"predicted_n":100,"predicted_ms":500}}),
        )
        + &chat_frame(
            json!([]),
            json!({"usage":{"prompt_tokens":10,"completion_tokens":100},"timings":{"predicted_n":100,"predicted_ms":600}}),
        )
        + "data: [DONE]\n\n";
    let events = harness::drive(&model, openai_chat::decode_stream_event, data.as_bytes(), 0)
        .await
        .unwrap();
    assert_eq!(
        harness::finished(&events)
            .inference
            .as_ref()
            .unwrap()
            .server_unavailable,
        Some(ServerTimingUnavailable::Conflicting)
    );
}

#[tokio::test]
async fn duplicate_advisory_numbers_are_invalid_not_decode_errors() {
    let model = harness::model(Protocol::OpenAiChat, None);
    for advisory in [
        r#""timings":{"predicted_n":100,"predicted_ms":500,"predicted_ms":600}"#,
        r#""usage":{"prompt_tokens":10,"completion_tokens":100,"completion_time":0.5,"completion_time":0.6}"#,
        r#""x_groq":{"usage":{"completion_tokens":100,"completion_time":0.5,"completion_time":0.6}}"#,
    ] {
        let data = chat_frame(json!([{"delta":{"content":"a"}}]), json!({}))
            + &format!("data: {{\"id\":\"metrics-response\",\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],{advisory}}}\n\n")
            + "data: [DONE]\n\n";
        let events = harness::drive(&model, openai_chat::decode_stream_event, data.as_bytes(), 1)
            .await
            .unwrap();
        assert_eq!(
            harness::finished(&events)
                .inference
                .as_ref()
                .unwrap()
                .server_unavailable,
            Some(ServerTimingUnavailable::Invalid)
        );
    }
}

#[tokio::test]
async fn response_identity_mismatch_cannot_publish_a_server_rate() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let data = chat_frame(json!([{"delta":{"content":"a"}}]), json!({}))
        + &chat_frame(
            json!([{"delta":{},"finish_reason":"stop"}]),
            json!({"id":"different-response","timings":{"predicted_n":100,"predicted_ms":500}}),
        )
        + "data: [DONE]\n\n";
    let events = harness::drive(&model, openai_chat::decode_stream_event, data.as_bytes(), 0)
        .await
        .unwrap();
    assert_eq!(
        harness::finished(&events)
            .inference
            .as_ref()
            .unwrap()
            .server_unavailable,
        Some(ServerTimingUnavailable::Conflicting)
    );
}

#[tokio::test]
async fn responses_terminal_native_timing_survives_completed_and_incomplete() {
    let model = harness::model(Protocol::OpenAiResponses, None);
    for (kind, details) in [
        ("response.completed", json!({})),
        (
            "response.incomplete",
            json!({"incomplete_details":{"reason":"max_output_tokens"}}),
        ),
    ] {
        let mut response = json!({"id":"r","usage":{"input_tokens":10,"output_tokens":150,"total_tokens":160},"timings":{"predicted_n":100,"predicted_ms":500}});
        response
            .as_object_mut()
            .unwrap()
            .extend(details.as_object().unwrap().clone());
        let data = format!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\"}}}}\n\ndata: {}\n\n",
            json!({"type":kind,"response":response})
        );
        let events = harness::drive(
            &model,
            openai_responses::decode_stream_event,
            data.as_bytes(),
            1,
        )
        .await
        .unwrap();
        let response = harness::finished(&events);
        assert_eq!(response.usage.output_tokens, 150);
        assert_eq!(
            response
                .inference
                .as_ref()
                .unwrap()
                .server
                .as_ref()
                .unwrap()
                .tokens_per_second(),
            Some(200.0)
        );
    }
}

#[test]
fn nonstreaming_completed_audio_and_native_timing_are_independent() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let mut body: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/openai_chat/audio_output.json"
    ))
    .unwrap();
    body["timings"] = json!({"predicted_n":100,"predicted_ms":500});
    let response = openai_chat::decode_response(
        &model,
        &serde_json::to_vec(&body).unwrap(),
        Some(crate::types::AudioFormat::Wav),
    )
    .unwrap();
    assert_eq!(response.usage.output_tokens, 20);
    assert_eq!(response.inference.unwrap().server.unwrap().tokens, 100);
}

#[tokio::test]
async fn client_wrapper_counts_chunks_not_hidden_or_billed_tokens() {
    let response = chat(json!({"usage":{"prompt_tokens":10,"completion_tokens":150,"completion_tokens_details":{"reasoning_tokens":100}}})).await;
    let events = vec![
        Ok(StreamEvent::Started { response_id: None }),
        Ok(StreamEvent::TextStart { index: 0 }),
        Ok(StreamEvent::TextDelta {
            index: 0,
            delta: "answer".into(),
        }),
        Ok(StreamEvent::TextEnd { index: 0 }),
        Ok(StreamEvent::Finished(response)),
    ];
    let mut measured = measured_stream(
        crate::stream::guard(futures_util::stream::iter(events)),
        Instant::now(),
        ClientTimingScope::Request,
    );
    let mut final_metrics = None;
    while let Some(event) = measured.next().await {
        if let StreamEvent::Finished(response) = event.unwrap() {
            final_metrics = response.inference;
        }
    }
    let metrics = final_metrics.unwrap();
    let client = metrics.client.unwrap();
    assert_eq!(client.output_events, 1);
    assert_eq!(client.text_bytes, 6);
    assert_eq!(client.reported_output_tokens, 150);
    assert_eq!(client.output_interval_ns(), Some(0));
    assert!(client.end_to_end_tokens_per_second().is_some());
    assert_eq!(
        metrics.server_unavailable,
        Some(ServerTimingUnavailable::NotReported)
    );
}

#[tokio::test]
async fn failed_or_cancelled_stream_does_not_invent_a_success_sample() {
    let empty = futures_util::stream::iter([Ok(StreamEvent::Started { response_id: None })]);
    let mut stream = measured_stream(
        crate::stream::guard(empty),
        Instant::now(),
        ClientTimingScope::Request,
    );
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        StreamEvent::Started { .. }
    ));
    assert!(stream.next().await.unwrap().is_err());
    assert!(stream.next().await.is_none());
    let pending = futures_util::stream::pending();
    drop(measured_stream(
        crate::stream::guard(pending),
        Instant::now(),
        ClientTimingScope::Request,
    ));
}
