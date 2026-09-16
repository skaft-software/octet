//! Real JSONL persistence tests, not an in-memory durability substitute.

use octet_agent::{EntryValue, Session};
use octet_ai::{
    AssistantMessage, AssistantPart, Message, ModelId, Protocol, ToolCall, ToolCallId, ToolResult,
    ToolResultPart, UserMessage, UserPart,
};
use serde_json::json;

fn call(session: &mut Session) {
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId("reused-provider-id".into()),
                name: "read".into(),
                arguments_json: "{}".into(),
                argument_error: None,
            })],
            model: ModelId("test".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
}

fn result(session: &mut Session) -> Result<(), octet_agent::SessionError> {
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: ToolCallId("reused-provider-id".into()),
                content: vec![ToolResultPart::Text("complete".into())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .map(|_| ())
}

#[test]
fn memos_and_checkpoints_survive_reopen_then_settle_with_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    call(&mut session);
    let handle = session.tool_invocation(0).unwrap();
    let scope = handle.scope().clone();
    handle.set_memo("step/read", json!({"answer": 42})).unwrap();
    handle
        .replace_partial_output("bounded partial observation")
        .unwrap();
    assert_eq!(session.context().unwrap().len(), 1);
    assert_eq!(session.total_cost_microdollars(), 0);
    assert!(session.usage_records().is_empty());
    drop(session);
    assert!(handle.set_memo("late", json!(true)).is_err());

    let mut reopened = Session::open(&path).unwrap();
    let replay = reopened.tool_invocation(0).unwrap();
    assert_eq!(replay.scope(), &scope);
    assert_eq!(
        replay.partial_output().unwrap().as_deref(),
        Some("bounded partial observation")
    );
    let value: serde_json::Value = replay
        .replay_step("step/read", || panic!("committed memo must skip effect"))
        .unwrap();
    assert_eq!(value, json!({"answer": 42}));
    replay.clear_partial_output().unwrap();
    assert!(replay.partial_output().unwrap().is_none());
    assert!(replay.get_memo("step/read").unwrap().is_some());
    result(&mut reopened).unwrap();
    assert!(replay.get_memo("step/read").is_err());
    assert!(replay.replace_partial_output("late").is_err());
    assert!(reopened.tool_invocation(0).is_err());
    drop(reopened);
    assert!(Session::open(path).unwrap().tool_invocation(0).is_err());
}

#[test]
fn provider_call_id_reuse_does_not_alias_invocations() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    call(&mut session);
    let first = session.tool_invocation(0).unwrap();
    first.set_memo("step", json!("old")).unwrap();
    result(&mut session).unwrap();
    call(&mut session);
    let second = session.tool_invocation(0).unwrap();
    assert_ne!(first.scope(), second.scope());
    assert_eq!(second.get_memo("step").unwrap(), None);
    assert!(first.set_memo("step", json!("zombie")).is_err());
}

#[test]
fn memo_and_session_appends_share_one_stale_writer_fence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    call(&mut session);
    let handle = session.tool_invocation(0).unwrap();
    let mut competing = Session::open(&path).unwrap();
    handle.set_memo("step", json!(1)).unwrap();
    assert!(matches!(
        result(&mut competing),
        Err(octet_agent::SessionError::ConcurrentModification)
    ));
    result(&mut session).unwrap();
    assert!(handle.set_memo("step", json!(2)).is_err());
}

#[test]
fn read_only_invocation_handle_cannot_persist_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    call(&mut session);
    session
        .tool_invocation(0)
        .unwrap()
        .set_memo("step", json!(1))
        .unwrap();
    drop(session);
    let read_only = Session::open_read_only(&path).unwrap();
    let handle = read_only.tool_invocation(0).unwrap();
    assert_eq!(handle.get_memo("step").unwrap(), Some(json!(1)));
    assert!(handle.set_memo("step", json!(2)).is_err());
    assert_eq!(handle.get_memo("step").unwrap(), Some(json!(1)));
}

#[test]
fn failed_result_append_does_not_delete_memos() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    call(&mut session);
    let handle = session.tool_invocation(0).unwrap();
    handle.set_memo("step", json!(1)).unwrap();
    let mut competing = Session::open(&path).unwrap();
    competing
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("another writer".into())],
        })))
        .unwrap();
    assert!(result(&mut session).is_err());
    assert_eq!(handle.get_memo("step").unwrap(), Some(json!(1)));
    assert!(handle.set_memo("step", json!(2)).is_err());
}

#[test]
fn read_only_snapshot_of_a_writable_descriptor_cannot_gain_write_authority() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let descriptor = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .append(true)
        .open(&path)
        .unwrap();
    let snapshot_descriptor = descriptor.try_clone().unwrap();
    let mut owner = Session::create_with_file(&path, descriptor).unwrap();
    call(&mut owner);
    owner
        .tool_invocation(0)
        .unwrap()
        .set_memo("step", json!(1))
        .unwrap();
    let mut snapshot = Session::open_read_only_with_file(&path, snapshot_descriptor).unwrap();
    let handle = snapshot.tool_invocation(0).unwrap();
    assert_eq!(handle.get_memo("step").unwrap(), Some(json!(1)));
    assert!(handle.set_memo("step", json!(2)).is_err());
    assert!(result(&mut snapshot).is_err());
    assert_eq!(handle.get_memo("step").unwrap(), Some(json!(1)));
    result(&mut owner).unwrap();
}

#[test]
fn checkout_and_reopen_cannot_reissue_a_completed_invocation_identity() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkout.jsonl");
    let mut session = Session::create(&path).unwrap();
    call(&mut session);
    let assistant = session.head().unwrap();
    let handle = session.tool_invocation(0).unwrap();
    handle.set_memo("effect", json!("settled")).unwrap();
    result(&mut session).unwrap();
    let result_head = session.head().unwrap();
    let executions = AtomicUsize::new(0);
    session.checkout(assistant.clone()).unwrap();
    assert!(session.tool_invocation(0).is_err());
    assert!(handle
        .replay_step("effect", || executions.fetch_add(1, Ordering::SeqCst))
        .is_err());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    drop(session);
    let mut reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.head(), Some(assistant.clone()));
    if let Ok(reissued) = reopened.tool_invocation(0) {
        let _ = reissued.replay_step("effect", || executions.fetch_add(1, Ordering::SeqCst));
        panic!("a completed identity must never reopen after checkout");
    }
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(
        reopened.entry(&result_head).is_some(),
        "prior outcome is immutable"
    );
    // A fresh assistant, even with the same provider call ID, is distinct.
    call(&mut reopened);
    let fresh = reopened.tool_invocation(0).unwrap();
    assert_ne!(fresh.scope(), handle.scope());
    assert!(fresh.get_memo("effect").unwrap().is_none());
}
