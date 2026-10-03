//! The summary path that reads a session transcript without materialising it:
//! listing is byte-for-byte read-only even with a torn tail, invalid UTF-8 is
//! accepted only in the unterminated tail and rejected inside a completed
//! record, a cross-branch checkpoint is refused, the Responses sidecar is
//! structurally validated and its output matches full compaction validation,
//! usage records that are not assistant turns are accepted, and a deferred run
//! round-trips through the lightweight mirror while a normal resume refuses it.
//!
//! Separate from listing because these tests constrain the parser itself, not
//! the list a caller sees.

use super::*;

#[test]
fn active_branch_title_uses_oldest_active_user_text() {
    use octet_agent::{EntryValue, Session};
    use octet_ai::{
        AssistantMessage, AssistantPart, Message, ModelId, Protocol, UserMessage, UserPart,
    };

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let root = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("active title".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("abandoned".into())],
            model: ModelId("m".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    session.checkout(root).unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("active".into())],
            model: ModelId("m".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    assert_eq!(active_branch_title(&session), "active title");
}

#[test]
fn title_normalization_is_bounded_and_unicode_aware() {
    assert_eq!(trim_title("  one\n\ttwo  "), "one two");
    assert_eq!(
        trim_title(&format!("{}   ", "é".repeat(60))),
        "é".repeat(60)
    );
    assert_eq!(
        trim_title(&format!("{} next", "é".repeat(60))),
        format!("{}…", "é".repeat(60))
    );
    assert_eq!(trim_title(&"a".repeat(61)), format!("{}…", "a".repeat(60)));
}

#[test]
fn listing_is_byte_for_byte_read_only_even_for_a_torn_tail() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("torn.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("durable title".into())],
        })))
        .unwrap();
    drop(session);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"type\":\"entry\"");
    std::fs::write(&path, &bytes).unwrap();

    assert_eq!(store.list().len(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn listing_accepts_invalid_utf8_only_in_the_unterminated_tail() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("utf8-tail.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("durable title".into())],
        })))
        .unwrap();
    drop(session);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"text\":\"");
    bytes.extend_from_slice(&[0xf0, 0x9f]);
    std::fs::write(&path, &bytes).unwrap();

    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "durable title");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn lightweight_summary_rejects_invalid_utf8_in_a_completed_record() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("utf8-corrupt.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("durable title".into())],
        })))
        .unwrap();
    drop(session);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(&[0xff, b'\n']);
    std::fs::write(&path, &bytes).unwrap();

    let error = summarize_session(&path).unwrap_err();
    assert!(error.to_string().contains("line 3"), "{error:#}");
    assert!(error.to_string().contains("invalid UTF-8"), "{error:#}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn lightweight_summary_rejects_a_malformed_completed_final_record() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("corrupt.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("durable title".into())],
        })))
        .unwrap();
    drop(session);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"type\":\"entry\"\n");
    std::fs::write(&path, &bytes).unwrap();

    let error = summarize_session(&path).unwrap_err();
    assert!(error.to_string().contains("line 3"), "{error:#}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn lightweight_summary_rejects_a_cross_branch_checkpoint() {
    use std::io::Write as _;

    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("cross-branch.jsonl");
    let mut session = Session::create(&path).unwrap();
    let root_entry = session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("root".into())],
        })))
        .unwrap();
    let abandoned_prompt = session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("abandoned".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("old answer".into())],
                model: octet_ai::ModelId("model".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    session.checkout(root_entry).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("active".into())],
        })))
        .unwrap();
    let active_head = session
        .append(EntryValue::Message(Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("new answer".into())],
                model: octet_ai::ModelId("model".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    drop(session);

    let record = octet_agent::SessionRecord::Checkpoint {
        prompt: abandoned_prompt,
        head: active_head,
        usage: None,
        run_cost_microdollars: None,
    };
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    serde_json::to_writer(&mut file, &record).unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);

    let error = summarize_session(&path).unwrap_err();
    assert!(error.to_string().contains("line 12"), "{error:#}");
    assert!(error.to_string().contains("not an ancestor"), "{error:#}");
}

#[test]
fn lightweight_summary_validates_responses_sidecar_structure() {
    use std::io::Write as _;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad-responses-turn.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("title".into())],
        })))
        .unwrap();
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("answer".into())],
                model: ModelId("model-a".into()),
                protocol: Protocol::OpenAiResponses,
            },
        )))
        .unwrap();
    drop(session);

    let malformed = serde_json::json!({
        "type": "entry",
        "id": "999",
        "parent": assistant,
        "value": {
            "type": "responses_turn",
            "assistant": assistant,
            "endpoint": "responses",
            "model": "model-b",
            "output": [{"type": "message"}]
        }
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    serde_json::to_writer(&mut file, &malformed).unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);

    let error = summarize_session(&path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is not a direct sidecar of assistant"),
        "{error:#}"
    );

    let compact_path = directory.path().join("bad-responses-compact.jsonl");
    let mut session = Session::create(&compact_path).unwrap();
    let first = session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("compact title".into())],
        })))
        .unwrap();
    let second = session
        .append(EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    drop(session);
    let malformed = serde_json::json!({
        "type": "entry",
        "id": "999",
        "parent": second,
        "value": {
            "type": "responses_compaction",
            "endpoint": "responses",
            "model": "model-a",
            "covered_through": first,
            "output": [{"type": "compaction"}]
        }
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&compact_path)
        .unwrap();
    serde_json::to_writer(&mut file, &malformed).unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);
    let error = summarize_session(&compact_path).unwrap_err();
    assert!(
        error.to_string().contains("is not a direct checkpoint"),
        "{error:#}"
    );
}

#[test]
fn lightweight_responses_output_matches_full_compaction_validation() {
    let cases = [
        serde_json::json!([]),
        serde_json::json!([{"type": "message", "content": "ignored"}]),
        serde_json::json!([{"type": "compaction"}]),
        serde_json::json!([{"type": "compaction", "encrypted_content": ""}]),
        serde_json::json!([{"type": "compaction", "encrypted_content": 42}]),
        serde_json::json!([{"type": "compaction", "encrypted_content": "opaque"}]),
        serde_json::json!([
            {"type": "message", "future": {"large": [1, 2, 3]}},
            {"type": "compaction", "encrypted_content": "opaque"}
        ]),
        serde_json::json!([
            {"type": "compaction", "encrypted_content": "one"},
            {"type": "compaction", "encrypted_content": "two"}
        ]),
    ];

    for value in cases {
        let summary: SummaryResponsesOutput = serde_json::from_value(value.clone()).unwrap();
        let full: octet_ai::ResponsesOutput = serde_json::from_value(value).unwrap();
        assert_eq!(summary.is_empty(), full.is_empty());
        assert_eq!(summary.has_valid_compaction(), full.has_valid_compaction());
    }
}

#[test]
fn lightweight_summary_accepts_non_assistant_usage_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("usage-kinds.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("usage title".into())],
        })))
        .unwrap();
    session
        .record_rejected_responses_turn_usage(
            octet_ai::EndpointId("responses".into()),
            ModelId("model".into()),
            octet_ai::Usage::default(),
            None,
        )
        .unwrap();
    session
        .record_terminal_gate_usage(
            octet_ai::EndpointId("responses".into()),
            ModelId("model".into()),
            octet_ai::Usage::default(),
            None,
            None,
        )
        .unwrap();
    drop(session);

    assert_eq!(
        summarize_session(&path).unwrap().title.as_deref(),
        Some("usage title")
    );
}

/// One durably parked deferred run, written through the session's own
/// deferred-run store so the record lands in the transcript exactly as a
/// real suspension would. The open session is returned so a test can drive
/// the next durable change through the same store.
fn park_deferred_run(path: &Path) -> (Session, DeferredRunRecord) {
    use octet_agent::tools::deferred::{
        DeferredHandle, DeferredResponseDeclaration, DeferredStopReason, DeferredSuspendDecision,
        ModelIdentity,
    };

    let mut session = Session::create(path).unwrap();
    let source = session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("parked prompt".into())],
        })))
        .unwrap();
    let identity = ModelIdentity::new("provider", "model");
    let declaration = DeferredResponseDeclaration {
        stop_reason: DeferredStopReason::Deferred,
        api: "anthropic_messages".into(),
        handle: Some(DeferredHandle::new(
            "provider",
            "model",
            "anthropic_messages",
            "resp-1",
        )),
    };
    let store = session.deferred_run_store();
    assert!(matches!(
        store
            .suspend(&identity, "op-1", &source.0, declaration)
            .unwrap(),
        DeferredSuspendDecision::Suspended(_)
    ));
    let record = store.record("op-1").expect("the suspension is durable");
    (session, record)
}

#[test]
fn deferred_run_records_round_trip_through_the_lightweight_mirror() {
    use octet_agent::tools::deferred::{
        DeferredResumeIntent, DeferredResumeStart, DeferredRunState,
    };

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("deferred.jsonl");
    let (session, parked) = park_deferred_run(&path);

    // The parked leaf survives the mirror with its operation identity, grade
    // and provider handle intact.
    let mirrored = summarize_session(&path).unwrap();
    assert_eq!(mirrored.deferred_run_records, vec![parked.clone()]);
    let record = &mirrored.deferred_run_records[0];
    assert_eq!(record.operation_id, "op-1");
    assert_eq!(record.state_label(), "suspended");
    assert_eq!(record.generation, 0);
    let leaf = record.leaf().expect("a parked record keeps its leaf");
    assert_eq!(leaf.poll, 0);
    assert_eq!(leaf.handle.id, "resp-1");
    assert_eq!(leaf.response_api, "anthropic_messages");

    // A permitted poll replaces the leaf under a bumped generation before the
    // provider runs; the mirror must keep the last authoritative state and
    // never the abandoned one.
    let DeferredResumeStart::Admitted(poll) = session
        .deferred_run_store()
        .begin_pass("op-1", "pass-1", DeferredResumeIntent::Poll, 0)
        .unwrap()
    else {
        panic!("the first permitted poll must be admitted");
    };
    drop(session);

    let mirrored = summarize_session(&path).unwrap();
    assert_eq!(
        mirrored.deferred_run_records,
        vec![poll.effect_pending.clone()]
    );
    assert_eq!(
        mirrored.deferred_run_records[0].state_label(),
        "effect_pending"
    );
    assert!(matches!(
        mirrored.deferred_run_records[0].state,
        DeferredRunState::EffectPending { .. }
    ));
    assert_eq!(mirrored.deferred_run_records[0].generation, 1);

    // A reopened session replays the same replaceable state, so the mirror
    // and the authoritative store agree after a restart.
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.deferred_runs(), mirrored.deferred_run_records);
}

#[test]
fn lightweight_mirror_refuses_deferred_records_normal_resume_rejects() {
    let directory = tempfile::tempdir().unwrap();

    // A record replaying a generation the store already holds is refused by
    // the durable store on reopen; the mirror must refuse it too.
    let stale_path = directory.path().join("stale-deferred.jsonl");
    let (session, parked) = park_deferred_run(&stale_path);
    drop(session);
    append_session_record(
        &stale_path,
        &octet_agent::SessionRecord::DeferredRun {
            record: parked.clone(),
        },
    );
    let error = summarize_session(&stale_path).unwrap_err();
    assert!(
        error.to_string().contains("generation regressed"),
        "{error:#}"
    );

    // A terminal tombstone is authoritative: no later record may follow it.
    let terminal_path = directory.path().join("terminal-deferred.jsonl");
    let (session, parked) = park_deferred_run(&terminal_path);
    drop(session);
    append_session_record(
        &terminal_path,
        &octet_agent::SessionRecord::DeferredRun {
            record: DeferredRunRecord::cancelled("op-1", parked.generation + 1),
        },
    );
    append_session_record(
        &terminal_path,
        &octet_agent::SessionRecord::DeferredRun {
            record: parked.clone(),
        },
    );
    let error = summarize_session(&terminal_path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("terminal deferred record may not be followed"),
        "{error:#}"
    );
}

/// Append one already-built session record byte-for-byte, the way a torn or
/// hostile transcript would carry it.
fn append_session_record(path: &Path, record: &octet_agent::SessionRecord) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}
