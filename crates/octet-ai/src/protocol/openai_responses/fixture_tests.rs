//! Unit tests for `crate::protocol::openai_responses`.
//!
//! Covers offline fixture-matrix replay of the stream decoder.
//!
//! Extracted from `openai_responses.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::openai_responses`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::{decode_stream_event, COMPUTER_TOOL_NAME, MAX_COMPUTER_ACTION_BYTES};
use crate::error::{AiError, StreamProtocolError};
use crate::protocol::harness;
use crate::stream::StreamEvent;
use crate::types::{
    AssistantPart, Protocol, ReasoningStateKind, StopReason, ToolCallArgumentError, ToolDef,
};

macro_rules! fx {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/openai_responses/",
            $name
        ))
    };
}

async fn run(name: &[u8], chunk: usize) -> Result<Vec<StreamEvent>, AiError> {
    let model = harness::model(Protocol::OpenAiResponses, None);
    harness::drive(&model, decode_stream_event, name, chunk).await
}

fn text_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn plain_text() {
    let events = run(fx!("plain_text.sse"), 0).await.unwrap();
    assert_eq!(text_of(&events), "Hello world");
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
}

#[tokio::test]
async fn plain_text_identical_across_byte_boundaries() {
    let data = fx!("plain_text.sse");
    let base = format!("{:?}", run(data, 0).await.unwrap());
    for chunk in 1..=data.len() {
        assert_eq!(
            format!("{:?}", run(data, chunk).await.unwrap()),
            base,
            "chunk {chunk}"
        );
    }
}

#[tokio::test]
async fn encrypted_reasoning_state_preserved() {
    let events = run(fx!("reasoning_encrypted.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let reasoning = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::Reasoning(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(reasoning.text.as_deref(), Some("Let me reason carefully."));
    match &reasoning.state.as_ref().unwrap().kind {
        ReasoningStateKind::OpenAiReasoning {
            item_id,
            encrypted_content,
        } => {
            assert_eq!(item_id.as_deref(), Some("rs_1"));
            assert_eq!(encrypted_content.as_deref(), Some("RU5DUllQVEVE"));
        }
        other => panic!("expected OpenAiReasoning, got {other:?}"),
    }
    assert_eq!(text_of(&events), "Answer: 42");
    assert_eq!(resp.usage.reasoning_tokens, 18);
}

#[tokio::test]
async fn terminal_encrypted_reasoning_backfills_missing_item_payload() {
    // Azure OpenAI / xAI omit `encrypted_content` from `output_item.done`
    // and provide it only on `response.completed`. The stored reasoning
    // state must be enriched from the terminal output for replay.
    let events = run(fx!("reasoning_backfill.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let reasoning = resp
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::Reasoning(reasoning) => Some(reasoning),
            _ => None,
        })
        .expect("reasoning part");
    assert_eq!(reasoning.text.as_deref(), Some("Trace it"));
    match &reasoning.state.as_ref().unwrap().kind {
        ReasoningStateKind::OpenAiReasoning {
            item_id,
            encrypted_content,
        } => {
            assert_eq!(item_id.as_deref(), Some("rs_backfill"));
            assert_eq!(
                encrypted_content.as_deref(),
                Some("VEVSTUlOQUxfRU5DUllQVEVE")
            );
        }
        other => panic!("expected OpenAiReasoning, got {other:?}"),
    }
    assert_eq!(text_of(&events), "done");
}

#[tokio::test]
async fn reasoning_summary_deltas_stream_and_preserve_state() {
    let events = run(fx!("reasoning_summary.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let reasoning = resp
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::Reasoning(reasoning) => Some(reasoning),
            _ => None,
        })
        .expect("reasoning summary must be surfaced");
    assert_eq!(reasoning.text.as_deref(), Some("Planning briefly."));
    match &reasoning.state.as_ref().unwrap().kind {
        ReasoningStateKind::OpenAiReasoning {
            item_id,
            encrypted_content,
        } => {
            assert_eq!(item_id.as_deref(), Some("rs_summary"));
            assert_eq!(
                encrypted_content.as_deref(),
                Some("RU5DUllQVEVEX1NVTU1BUlk=")
            );
        }
        other => panic!("expected OpenAiReasoning, got {other:?}"),
    }
    assert_eq!(text_of(&events), "DONE");
}

#[tokio::test]
async fn tool_call_uses_call_id_and_tool_use_stop() {
    let events = run(fx!("tool_call.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    let tc = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::ToolCall(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(tc.id.0, "call_1");
    assert_eq!(tc.name, "grep");
    assert_eq!(
        tc.arguments_value().unwrap(),
        serde_json::json!({"pattern":"foo"})
    );
}

#[tokio::test]
async fn schema_mismatch_is_marked_before_tool_call_end() {
    let model = harness::model(Protocol::OpenAiResponses, None);
    let tools = [ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "grep".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"pattern": {"type": "integer"}},
            "required": ["pattern"],
            "additionalProperties": false,
        }),
    }];
    let events =
        harness::drive_with_tools(&model, decode_stream_event, fx!("tool_call.sse"), 0, &tools)
            .await
            .unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolCallEnd {
            argument_error: Some(ToolCallArgumentError::SchemaMismatch),
            ..
        }
    )));
    let call = harness::finished(&events)
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("schema-rejected call is retained");
    assert_eq!(call.id.0, "call_1");
    assert_eq!(call.arguments_json, r#"{"pattern":"foo"}"#);
    assert_eq!(
        call.argument_error,
        Some(ToolCallArgumentError::SchemaMismatch)
    );
}

#[tokio::test]
async fn terminal_output_is_authoritative_over_added_item_skeleton() {
    let stream = br#"data: {"type":"response.created","response":{"id":"resp_raw"}}

data: {"type":"response.output_item.added","output_index":0,"item":{"id":"fc_skeleton","type":"function_call","call_id":"call_skeleton","name":"exec","arguments":"{}"}}

data: {"type":"response.completed","response":{"output":[{"type":"function_call","id":"fc_terminal","call_id":"call_terminal","name":"exec","arguments":"{\"command\":\"pwd\"}","phase":"commentary","unknown":{"kept":true}}]}}

"#;
    let events = run(stream, 0).await.unwrap();
    let response = harness::finished(&events);
    let output = response
        .responses_output
        .as_ref()
        .expect("terminal response output must be retained");
    assert_eq!(output.items().len(), 1);
    assert_eq!(output.items()[0].as_json()["id"], "fc_terminal");
    assert_eq!(output.items()[0].as_json()["call_id"], "call_terminal");
    assert_eq!(output.items()[0].as_json()["phase"], "commentary");
    assert_eq!(output.items()[0].as_json()["unknown"]["kept"], true);
    assert_ne!(output.items()[0].as_json()["id"], "fc_skeleton");
}

#[tokio::test]
async fn inline_tool_arguments_close_at_response_completion() {
    let events = run(fx!("inline_tool_arguments.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    let tc = resp
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(tool) => Some(tool),
            _ => None,
        })
        .expect("inline function call must be preserved");
    assert_eq!(
        tc.arguments_value().unwrap(),
        serde_json::json!({"pattern": "foo"})
    );
}

#[tokio::test]
async fn done_event_arguments_are_recovered_when_no_deltas_arrive() {
    let events = run(fx!("done_tool_arguments.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    let tc = resp
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(tool) => Some(tool),
            _ => None,
        })
        .expect("terminal function-call event must preserve the tool call");
    assert_eq!(tc.name, "exec");
    assert_eq!(
        tc.arguments_value().unwrap(),
        serde_json::json!({"command":"pwd"})
    );
}

#[tokio::test]
async fn parallel_tool_calls() {
    let events = run(fx!("parallel_tool_calls.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let calls: Vec<_> = resp
        .message
        .content
        .iter()
        .filter_map(|p| match p {
            AssistantPart::ToolCall(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id.0, "call_a");
    assert_eq!(calls[1].id.0, "call_b");
}

#[tokio::test]
async fn malformed_tool_json_is_decode_error() {
    let err = run(fx!("malformed_tool_json.sse"), 0).await.unwrap_err();
    assert!(matches!(err, AiError::Decode(_)), "got {err:?}");
}

#[tokio::test]
async fn incomplete_maps_to_max_tokens() {
    let events = run(fx!("incomplete_max_tokens.sse"), 0).await.unwrap();
    let response = harness::finished(&events);
    assert_eq!(response.stop_reason, StopReason::MaxTokens);
    let output = response
        .responses_output
        .as_ref()
        .expect("incomplete terminal output must be retained for exact replay");
    assert_eq!(output.items()[0].as_json()["id"], "msg_terminal_partial");
    assert_eq!(output.items()[0].as_json()["unknown"]["kept"], "verbatim");
}

#[tokio::test]
async fn out_of_scope_event_is_ignored() {
    let events = run(fx!("ignored_event.sse"), 0).await.unwrap();
    assert_eq!(text_of(&events), "ok");
    assert!(matches!(events.last(), Some(StreamEvent::Finished(_))));
}

#[tokio::test]
async fn failed_terminal_without_prose_preserves_policy_veto() {
    for error in [
        serde_json::json!({"code":"cyber_policy"}),
        serde_json::json!({"type":"invalid_prompt", "message":null}),
        serde_json::Value::Null,
    ] {
        let permanent = !error.is_null();
        let terminal = serde_json::json!({"type":"response.failed","response":{"error":error}});
        let wire = format!("data: {terminal}\n\n");
        let AiError::ResponsesFailed(provider) = run(wire.as_bytes(), 0).await.unwrap_err() else {
            panic!("lost terminal provenance or policy veto");
        };
        assert_eq!(provider.is_permanent(), permanent);
    }
}

#[tokio::test]
async fn failed_terminals_preserve_unknown_and_permanent_code_kind() {
    for (code, permanent) in [
        ("unknown_failure", false),
        ("overloaded_error", false),
        ("server_is_overloaded", true),
        ("slow_down", true),
        ("invalid_prompt", true),
        ("bio_policy", true),
        ("cyber_policy", true),
        ("misalignment_policy_violation", true),
        ("context_length_exceeded", true),
        ("insufficient_quota", true),
        ("usage_not_included", true),
        ("authentication_error", true),
    ] {
        for field in ["code", "type"] {
            let mut error = serde_json::json!({"message":"retry transient server_error"});
            error[field] = serde_json::json!(code);
            let terminal = serde_json::json!({"type":"response.failed","response":{"error":error}});
            let wire = format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\"}}}}\n\ndata: {terminal}\n\n");
            let AiError::ResponsesFailed(provider) = run(wire.as_bytes(), 0).await.unwrap_err()
            else {
                panic!("lost response.failed provenance");
            };
            assert_eq!(provider.is_permanent(), permanent, "{field}={code}");
            assert_eq!(
                if field == "code" {
                    provider.code
                } else {
                    provider.kind
                }
                .as_deref(),
                Some(code)
            );
        }
    }
}

#[tokio::test]
async fn incomplete_unknown_reason_is_distinct_from_successful_known_terminals() {
    for (reason, stop) in [
        (
            "upstream_disconnect",
            StopReason::Other("upstream_disconnect".into()),
        ),
        ("content_filter", StopReason::Refusal),
        ("max_output_tokens", StopReason::MaxTokens),
    ] {
        let wire = String::from_utf8(fx!("incomplete_max_tokens.sse").to_vec())
            .unwrap()
            .replace("max_output_tokens", reason);
        let events = run(wire.as_bytes(), 0).await.unwrap();
        assert_eq!(harness::finished(&events).stop_reason, stop);
        assert_eq!(text_of(&events), "partial");
    }
}

#[tokio::test]
async fn response_failed_retains_terminal_provenance() {
    let err = run(fx!("response_failed.sse"), 0).await.unwrap_err();
    match err {
        AiError::ResponsesFailed(p) => {
            assert_eq!(p.code.as_deref(), Some("server_error"));
            assert_eq!(p.message, "boom");
        }
        other => panic!("expected ResponsesFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn premature_eof() {
    let err = run(fx!("premature_eof.sse"), 0).await.unwrap_err();
    assert!(
        matches!(
            err,
            AiError::StreamProtocol(StreamProtocolError::PrematureEof)
        ),
        "got {err:?}"
    );
}

// f9: a documented top-level `error` event is surfaced as `Provider`, not
// swallowed by `#[serde(other)]` into a `PrematureEof`.
#[tokio::test]
async fn top_level_error_event_becomes_provider_error() {
    let err = run(fx!("stream_error.sse"), 0).await.unwrap_err();
    match err {
        AiError::Provider(p) => {
            assert_eq!(p.code.as_deref(), Some("ERR_SOMETHING"));
            assert_eq!(p.message, "Something went wrong");
        }
        other => panic!("expected Provider, got {other:?}"),
    }
}

#[tokio::test]
async fn codex_nested_error_event_becomes_provider_error() {
    let err = run(fx!("codex_nested_stream_error.sse"), 1)
        .await
        .unwrap_err();
    match err {
        AiError::Provider(provider) => {
            assert_eq!(provider.code.as_deref(), Some("upstream_failure"));
            assert_eq!(provider.message, "Nested Codex stream failure");
        }
        other => panic!("expected Provider, got {other:?}"),
    }
}

#[tokio::test]
async fn codex_nested_error_accepts_observed_nullable_code() {
    let err = run(fx!("codex_nested_nullable_error.sse"), 1)
        .await
        .unwrap_err();
    match err {
        AiError::Provider(provider) => {
            assert_eq!(provider.code, None);
            assert_eq!(provider.kind.as_deref(), Some("server_error"));
            assert_eq!(provider.message, "The upstream provider ended the request");
        }
        other => panic!("expected Provider, got {other:?}"),
    }
}

#[tokio::test]
async fn codex_nested_error_still_requires_a_string_message() {
    let err = run(fx!("codex_nested_error_missing_message.sse"), 1)
        .await
        .unwrap_err();
    assert!(matches!(err, AiError::Decode(_)), "got {err:?}");
    assert!(
        err.to_string()
            .contains("invalid OpenAI Responses `error` event"),
        "got {err}"
    );
}

// f5: opaque reasoning with no visible delta must still surface a reasoning
// part carrying the item_id/encrypted_content (else it is silently dropped).
#[tokio::test]
async fn opaque_reasoning_without_text_is_preserved() {
    let events = run(fx!("reasoning_opaque_no_text.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let reasoning = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::Reasoning(r) => Some(r),
            _ => None,
        })
        .expect("opaque reasoning part must be present");
    assert_eq!(reasoning.text, None, "opaque reasoning carries no text");
    match &reasoning.state.as_ref().unwrap().kind {
        ReasoningStateKind::OpenAiReasoning {
            item_id,
            encrypted_content,
        } => {
            assert_eq!(item_id.as_deref(), Some("rs_9"));
            assert_eq!(encrypted_content.as_deref(), Some("T1BBUVVF"));
        }
        other => panic!("expected OpenAiReasoning, got {other:?}"),
    }
    assert_eq!(text_of(&events), "Answer");
}

// f2: a completed response without a `usage` object still decodes; usage
// falls back to the default and no `Usage` event is emitted.
#[tokio::test]
async fn completed_without_usage_defaults() {
    let events = run(fx!("completed_no_usage.sse"), 0).await.unwrap();
    assert!(
        !events.iter().any(|e| matches!(e, StreamEvent::Usage(_))),
        "no Usage event should be emitted when usage is absent"
    );
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage, crate::types::Usage::default());
    assert_eq!(text_of(&events), "hi");
}

// --- Responses computer use (roadmap #388): wire protocol only ---

fn computer_call_of(resp: &crate::types::Response) -> &crate::types::ToolCall {
    resp.message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("response must contain one computer tool call")
}

#[tokio::test]
async fn computer_call_round_trips_action_call_id_and_safety_checks() {
    let events = run(fx!("computer_call.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    let call = computer_call_of(resp);
    assert_eq!(call.id.0, "call_comp_1");
    assert_eq!(call.name, COMPUTER_TOOL_NAME);
    // The action is bounded into one canonical argument object and the
    // provider's pending safety checks ride along with it.
    assert_eq!(
        call.arguments_value().unwrap(),
        serde_json::json!({
            "action": {"type": "click", "button": "left", "x": 120, "y": 340},
            "pending_safety_checks": [{
                "id": "sc_1",
                "code": "malicious_instruction",
                "message": "Possible prompt injection"
            }],
        })
    );
    // The authoritative terminal item stays available for opaque replay.
    let output = resp.responses_output.as_ref().unwrap();
    assert_eq!(output.items().len(), 1);
    assert_eq!(output.items()[0].as_json()["type"], "computer_call");
    assert_eq!(output.items()[0].as_json()["call_id"], "call_comp_1");
}

#[tokio::test]
async fn computer_call_decodes_identically_across_byte_boundaries() {
    let data = fx!("computer_call.sse");
    let base = format!("{:?}", run(data, 0).await.unwrap());
    for chunk in [1, 3, 17] {
        assert_eq!(format!("{:?}", run(data, chunk).await.unwrap()), base);
    }
}

#[tokio::test]
async fn unsupported_computer_action_fails_closed() {
    let data = br#"data: {"type":"response.created","response":{"id":"resp_x"}}

data: {"type":"response.output_item.added","output_index":0,"item":{"id":"cc_1","type":"computer_call","call_id":"call_c1","action":{"type":"shell_exec","command":"rm -rf /"}}}

data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}

"#;
    let error = run(data, 0).await.unwrap_err();
    assert!(
        format!("{error}").contains("unsupported OpenAI Responses computer action `shell_exec`"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn computer_call_without_an_action_fails_closed() {
    // No action in `output_item.added` and no `output_item.done` at all:
    // the terminal check must refuse an actionless computer call.
    let data = br#"data: {"type":"response.created","response":{"id":"resp_x"}}

data: {"type":"response.output_item.added","output_index":0,"item":{"id":"cc_1","type":"computer_call","call_id":"call_c1"}}

data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}

"#;
    let error = run(data, 0).await.unwrap_err();
    assert!(
        format!("{error}").contains("unsupported OpenAI Responses computer action `missing`"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn oversized_computer_action_fails_closed() {
    let text = "a".repeat(MAX_COMPUTER_ACTION_BYTES + 1);
    let data = format!(
        "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_x\"}}}}\n\n\
         data: {{\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{{\"id\":\"cc_1\",\"type\":\"computer_call\",\"call_id\":\"call_c1\",\"action\":{{\"type\":\"type\",\"text\":\"{text}\"}}}}}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n\n"
    );
    let error = run(data.as_bytes(), 0).await.unwrap_err();
    assert!(
        format!("{error}").contains("over the 16384 byte bound"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn terminal_computer_call_action_is_used_when_added_omits_it() {
    let data = br#"data: {"type":"response.created","response":{"id":"resp_x"}}

data: {"type":"response.output_item.added","output_index":0,"item":{"id":"cc_1","type":"computer_call","call_id":"call_c1"}}

data: {"type":"response.output_item.done","output_index":0,"item":{"id":"cc_1","type":"computer_call","call_id":"call_c1","action":{"type":"screenshot"}}}

data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}

"#;
    let events = run(data, 0).await.unwrap();
    let resp = harness::finished(&events);
    assert_eq!(
        computer_call_of(resp).arguments_value().unwrap(),
        serde_json::json!({"action": {"type": "screenshot"}})
    );
}

#[tokio::test]
async fn computer_safety_checks_must_be_an_array() {
    for checks in [
        serde_json::json!({"id":"check"}),
        serde_json::json!("check"),
    ] {
        let item = serde_json::json!({
            "type":"computer_call", "id":"cc_1", "call_id":"call_c1",
            "action":{"type":"screenshot"}, "pending_safety_checks":checks,
        });
        let data = format!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_x\"}}}}\n\n\
             data: {{\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{item}}}\n\n"
        );
        let error = run(data.as_bytes(), 0).await.unwrap_err();
        assert!(
            error.to_string().contains("safety checks must be an array"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn terminal_computer_payload_changes_fail_before_executable_completion() {
    for terminal in [
        serde_json::json!({"action":{"type":"wait"}}),
        serde_json::json!({"pending_safety_checks":[{"id":"late-check"}]}),
        serde_json::json!({"pending_safety_checks":{"id":"malformed-check"}}),
    ] {
        let mut item = terminal;
        item["type"] = serde_json::json!("computer_call");
        item["id"] = serde_json::json!("cc_1");
        item["call_id"] = serde_json::json!("call_c1");
        let data = format!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_x\"}}}}\n\n\
             data: {{\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{{\"type\":\"computer_call\",\"id\":\"cc_1\",\"call_id\":\"call_c1\",\"action\":{{\"type\":\"screenshot\"}}}}}}\n\n\
             data: {{\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{item}}}\n\n\
             data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n\n"
        );
        let error = run(data.as_bytes(), 1).await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("changed after publication")
                || message.contains("safety checks must be an array"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn replayed_terminal_action_is_not_appended_twice() {
    // A duplicated action (added + done) must not concatenate two payloads:
    // the action is already asserted exactly once, with the safety checks,
    // in `computer_call_round_trips_action_call_id_and_safety_checks`.
    let events = run(fx!("computer_call.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let raw = computer_call_of(resp).arguments_json.clone();
    assert_eq!(raw.matches("\"type\":\"click\"").count(), 1, "{raw}");
}
