//! What happens when the warm catalog is wrong: a refresh after a session has
//! been mutated, a cold catalog that still accepts labels and tool-invocation
//! records, a corrupt or newer-schema catalog that falls back without
//! downgrading or touching transcripts, and the removal of rows whose transcript
//! has gone. Every test here asserts that the transcripts are left alone.
//!
//! Separate from the discovery group because these are the tests that a
//! plausible-but-wrong cache must be talked out of.

use super::*;

#[test]
fn indexed_entries_keep_only_user_and_assistant_text() {
    let record = serde_json::json!({
        "type": "entry",
        "id": "e1",
        "value": {"type": "message", "Assistant": {"content": [
            {"Text": "visible answer"},
            {"Reasoning": {"text": "hidden needle"}},
            {"ToolCall": {"name": "bash", "arguments": {"command": "secret needle"}}}
        ]}}
    });
    let entry = indexed_entry_from_record(&record).unwrap();
    assert_eq!(entry.kind, IndexedEntryKind::Assistant);
    assert!(entry.text.contains("visible answer"));
    assert!(!entry.text.contains("hidden needle"));
    assert!(!entry.text.contains("secret needle"));

    let user = serde_json::json!({
        "type": "entry",
        "id": "e2",
        "value": {"type": "message", "User": {"content": [
            {"Text": "user needle"},
            {"Media": {"mime": "image/png"}}
        ]}}
    });
    let entry = indexed_entry_from_record(&user).unwrap();
    assert_eq!(entry.kind, IndexedEntryKind::User);
    assert_eq!(entry.text, "user needle");
}

#[test]
fn open_session_refresh_keeps_mutated_transcripts_warm() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("active.jsonl");
    let mut session = Session::create(path).unwrap();
    let branch_root = session
        .append(EntryValue::Message(Message::Assistant(
            octet_ai::AssistantMessage {
                content: Vec::new(),
                model: ModelId("model".into()),
                protocol: Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("first branch".into())],
        })))
        .unwrap();
    assert_eq!(store.list()[0].title, "first branch");

    session.checkout(branch_root).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("second branch".into())],
        })))
        .unwrap();
    store.refresh_catalog_for_open_session(&session).unwrap();
    drop(session);

    let scans = std::cell::Cell::new(0);
    let listed = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 0);
    assert_eq!(listed[0].title, "second branch");
}

#[test]
fn cold_catalog_accepts_labels_and_tool_invocation_records() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("with-metadata.jsonl");
    let mut session = Session::create(&path).unwrap();
    let prompt = session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("retained title".into())],
        })))
        .unwrap();
    session.set_entry_label(&prompt, "checkpoint").unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(
            octet_ai::AssistantMessage {
                model: ModelId("model".into()),
                protocol: Protocol::OpenAiResponses,
                content: vec![octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                    id: octet_ai::ToolCallId("call".into()),
                    name: "test".into(),
                    arguments_json: "{}".into(),
                    argument_error: None,
                    async_execution: false,
                })],
            },
        )))
        .unwrap();
    session
        .tool_invocation(0)
        .unwrap()
        .set_memo("progress", serde_json::json!(true))
        .unwrap();
    drop(session);
    assert!(Session::open_read_only(&path).is_ok());
    assert!(summarize_catalog_session(&path).is_ok());
    assert_eq!(store.list()[0].title, "retained title");
}

#[test]
fn corrupt_catalog_falls_back_without_touching_transcripts() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("authoritative.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("authoritative".into())],
        })))
        .unwrap();
    drop(session);
    let authoritative_bytes = std::fs::read(&path).unwrap();
    assert_eq!(store.list().len(), 1);
    std::fs::write(SessionCatalog::path(store.dir()), b"not a sqlite database").unwrap();

    let scans = std::cell::Cell::new(0);
    let listed = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 1);
    assert_eq!(listed[0].title, "authoritative");
    assert_eq!(std::fs::read(&path).unwrap(), authoritative_bytes);

    scans.set(0);
    let rebuilt = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 0);
    assert_eq!(rebuilt[0].title, "authoritative");
    assert_eq!(std::fs::read(path).unwrap(), authoritative_bytes);
}

#[test]
fn catalog_removes_rows_for_missing_transcripts() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("removed.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("remove me".into())],
        })))
        .unwrap();
    drop(session);
    assert_eq!(store.list().len(), 1);
    assert!(SessionCatalog::open(store.dir())
        .unwrap()
        .load()
        .unwrap()
        .contains_key("removed"));

    std::fs::remove_file(path).unwrap();
    assert!(store.list().is_empty());
    assert!(!SessionCatalog::open(store.dir())
        .unwrap()
        .load()
        .unwrap()
        .contains_key("removed"));
}

#[test]
fn newer_catalog_schema_falls_back_without_downgrading() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("future.jsonl");
    let mut session = Session::create(path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("future compatible".into())],
        })))
        .unwrap();
    drop(session);
    assert_eq!(store.list().len(), 1);
    let catalog_path = SessionCatalog::path(store.dir());
    let connection = rusqlite::Connection::open(&catalog_path).unwrap();
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);
    let future_catalog_bytes = std::fs::read(&catalog_path).unwrap();

    let scans = std::cell::Cell::new(0);
    let listed = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 1);
    assert_eq!(listed[0].title, "future compatible");
    assert_eq!(std::fs::read(&catalog_path).unwrap(), future_catalog_bytes);
    let connection = rusqlite::Connection::open(catalog_path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 99);
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "delete");
}
