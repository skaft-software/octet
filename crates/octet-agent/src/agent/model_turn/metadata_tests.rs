//! Metadata is the exact persisted assistant usage record, never the latest total.
use super::*;
use crate::compaction::{
    SessionOperationDecision, SessionOperationFuture, SessionOperationInvocation,
};

struct Capture(Arc<Mutex<Vec<serde_json::Value>>>);
struct Continue;
impl SessionOperationInvocation for Continue {
    fn take_future(&mut self) -> SessionOperationFuture {
        Box::pin(async { Ok(SessionOperationDecision::Continue) })
    }
}
impl SessionOperationHook for Capture {
    fn begin(
        &self,
        _: &Session,
        operation: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_value(operation).unwrap());
        Ok(Some(Box::new(Continue)))
    }
}
fn assistant(model: &str, content: Vec<AssistantPart>) -> AssistantMessage {
    AssistantMessage {
        model: octet_ai::ModelId(model.into()),
        protocol: Protocol::OpenAiResponses,
        content,
    }
}

#[tokio::test]
async fn committed_metadata_is_linked_by_entry_after_reload_and_overlapping_tool_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hooks: Vec<Arc<dyn SessionOperationHook>> = vec![Arc::new(Capture(seen.clone()))];
    let mut turns = ModelTurnHooks::new(&hooks, "run:metadata");
    let call = ToolCall {
        id: octet_ai::ToolCallId("held".into()),
        name: "probe".into(),
        arguments_json: "{}".into(),
        argument_error: None,
        async_execution: true,
    };
    let first_usage = Usage {
        input_tokens: 13,
        output_tokens: 11,
        cache_read_tokens: 7,
        cache_write_tokens: 5,
        cache_write_1h_tokens: 2,
        reasoning_tokens: 3,
        total_tokens: 36,
    };
    let first_cost = Cost {
        input: 101,
        output: 203,
        reasoning: 17,
        cache_read: 29,
        cache_write: 31,
        total: 383,
        total_picodollars_remainder: 400_000,
    };
    let first = session
        .append_assistant_turn(
            assistant("first-model", vec![AssistantPart::ToolCall(call.clone())]),
            octet_ai::EndpointId("first-endpoint".into()),
            octet_ai::ModelId("first-model".into()),
            first_usage,
            Some(first_cost),
            StopReason::ToolUse,
            None,
        )
        .unwrap();
    turns.committed(&session, 0, &first, &[call]);
    let second_usage = Usage {
        input_tokens: 2,
        output_tokens: 1,
        total_tokens: 3,
        ..Usage::default()
    };
    let second = session
        .append_assistant_turn(
            assistant("second-model", vec![AssistantPart::Text("second".into())]),
            octet_ai::EndpointId("second-endpoint".into()),
            octet_ai::ModelId("second-model".into()),
            second_usage,
            None,
            StopReason::MaxTokens,
            None,
        )
        .unwrap();
    turns.committed(&session, 1, &second, &[]);
    session
        .record_compaction_usage(
            octet_ai::EndpointId("unrelated-endpoint".into()),
            octet_ai::ModelId("unrelated-model".into()),
            Usage {
                input_tokens: 9999,
                total_tokens: 9999,
                ..Usage::default()
            },
            Some(Cost::default()),
        )
        .unwrap();
    // Read the durable ledger, not a response retained in memory.
    drop(session);
    let mut session = Session::open(&path).unwrap();
    let cancellation = CancellationToken::default();
    turns.settle(&mut session, &cancellation).await.unwrap();
    assert!(
        seen.lock().unwrap().is_empty(),
        "first tool is still pending"
    );
    let result = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: octet_ai::ToolCallId("held".into()),
                content: vec![ToolResultPart::Text("settled".into())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();
    turns.settle(&mut session, &cancellation).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for (event, id, model, usage, cost, stop) in [
        (
            &seen[0],
            &first,
            "first-model",
            first_usage,
            Some(first_cost),
            "tool_use",
        ),
        (
            &seen[1],
            &second,
            "second-model",
            second_usage,
            None,
            "max_tokens",
        ),
    ] {
        assert_eq!(
            event["assistant_metadata"],
            serde_json::json!({
                "assistant_entry_id": id,
                "model": model,
                "usage": usage,
                "cost": cost,
                "stop_reason": stop,
            })
        );
        assert_eq!(
            event["assistant_entry"]["id"],
            serde_json::to_value(id).unwrap()
        );
    }
    assert_eq!(
        seen[0]["tool_result_entries"][0]["id"],
        serde_json::to_value(result).unwrap()
    );
    assert_eq!(seen[1]["tool_result_entries"], serde_json::json!([]));
}

#[tokio::test]
async fn missing_or_legacy_assistant_accounting_never_borrows_another_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hooks: Vec<Arc<dyn SessionOperationHook>> = vec![Arc::new(Capture(seen.clone()))];
    let mut turns = ModelTurnHooks::new(&hooks, "run:legacy");
    for with_usage in [true, false] {
        let entry = session
            .append(EntryValue::Message(Message::Assistant(assistant(
                "legacy-model",
                vec![AssistantPart::Text("legacy".into())],
            ))))
            .unwrap();
        if with_usage {
            session
                .record_assistant_usage(
                    entry.clone(),
                    octet_ai::EndpointId("legacy-endpoint".into()),
                    octet_ai::ModelId("legacy-model".into()),
                    Usage {
                        input_tokens: 5,
                        total_tokens: 5,
                        ..Usage::default()
                    },
                    None,
                )
                .unwrap();
        }
        turns.committed(&session, u64::from(!with_usage), &entry, &[]);
        turns
            .settle(&mut session, &CancellationToken::default())
            .await
            .unwrap();
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["assistant_metadata"]["usage"]["input_tokens"], 5);
    assert_eq!(
        seen[0]["assistant_metadata"]["stop_reason"],
        serde_json::Value::Null
    );
    assert_eq!(
        seen[0]["assistant_metadata"]["cost"],
        serde_json::Value::Null
    );
    assert!(seen[1]
        .as_object()
        .unwrap()
        .contains_key("assistant_metadata"));
    assert_eq!(seen[1]["assistant_metadata"], serde_json::Value::Null);
}
