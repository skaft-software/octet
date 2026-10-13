//! The per-session metadata file and the durable delete pipeline: metadata
//! round-trips without rewriting the session, a delete stages into a recoverable
//! trash with a retention deadline, a permanent delete demands the trash
//! confirmation, an interrupted pre-commit delete is idempotently restored, and
//! a fork records its provenance as an atomic pair.
//!
//! Separate from the catalog tests because this is the group where the durable
//! write path itself is under test, not the projection that reads it back.

use super::*;

#[test]
fn catalog_metadata_round_trips_without_rewriting_the_session() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("metadata.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("original title".into())],
        })))
        .unwrap();
    drop(session);
    let session_bytes = std::fs::read(&path).unwrap();

    store
        .set_tags("metadata", vec!["work".into(), "active".into()])
        .unwrap();
    store.rename("metadata", "  Renamed session  ").unwrap();
    store.set_pinned("metadata", true).unwrap();
    store.set_archived("metadata", true).unwrap();

    let reopened = SessionStore::new(root.path(), workspace.path());
    let metadata = reopened.load_metadata("metadata").unwrap();
    assert_eq!(metadata.name.as_deref(), Some("Renamed session"));
    assert_eq!(metadata.tags, ["work", "active"]);
    assert!(metadata.pinned);
    assert!(metadata.archived);
    let listed = reopened.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Renamed session");
    assert!(listed[0].pinned);
    assert!(listed[0].archived);
    assert_eq!(std::fs::read(path).unwrap(), session_bytes);
}

#[test]
fn trash_lifecycle_is_recoverable_and_preserves_its_retention_deadline() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("lifecycle.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("recover me".into())],
        })))
        .unwrap();
    drop(session);
    store.set_pinned("lifecycle", true).unwrap();

    let trashed = store
        .set_lifecycle("lifecycle", SessionStorageLifecycle::Trash, 1_000)
        .unwrap();
    assert!(trashed.archived);
    assert!(!trashed.pinned);
    assert_eq!(trashed.trashed_at_ms, Some(1_000));
    assert_eq!(
        trashed.purge_after_ms,
        Some(1_000 + SESSION_TRASH_RETENTION_MS)
    );

    let repeated = store
        .set_lifecycle("lifecycle", SessionStorageLifecycle::Trash, 9_000)
        .unwrap();
    assert_eq!(repeated.trashed_at_ms, trashed.trashed_at_ms);
    assert_eq!(repeated.purge_after_ms, trashed.purge_after_ms);
    let listed = store.list();
    assert_eq!(listed[0].trashed_at_ms, Some(1_000));
    assert_eq!(
        listed[0].purge_after_ms,
        Some(1_000 + SESSION_TRASH_RETENTION_MS)
    );

    let restored = store
        .set_lifecycle("lifecycle", SessionStorageLifecycle::Active, 10_000)
        .unwrap();
    assert!(!restored.archived);
    assert_eq!(restored.trashed_at_ms, None);
    assert_eq!(restored.purge_after_ms, None);

    let archived = store
        .set_lifecycle("lifecycle", SessionStorageLifecycle::Archived, 11_000)
        .unwrap();
    assert!(archived.archived);
    assert_eq!(archived.trashed_at_ms, None);
    assert!(path.is_file());
}

#[test]
fn permanent_delete_requires_the_current_trash_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("delete-me.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("delete me".into())],
        })))
        .unwrap();
    drop(session);
    store
        .set_lifecycle("delete-me", SessionStorageLifecycle::Trash, 2_000)
        .unwrap();
    let metadata_path = store.metadata_path("delete-me").unwrap();

    let error = store.delete_permanently("delete-me", 1_999).unwrap_err();
    assert!(error.to_string().contains("confirmation is stale"));
    assert!(path.is_file());
    assert!(metadata_path.is_file());

    std::fs::write(
        store.dir().join(".delete-delete-me-deadbeefdeadbeef"),
        b"staged transcript",
    )
    .unwrap();
    std::fs::write(
        store
            .metadata_dir()
            .join(".delete-delete-me-deadbeefdeadbeef"),
        b"staged metadata",
    )
    .unwrap();
    store.delete_permanently("delete-me", 2_000).unwrap();
    store.finish_permanent_delete("delete-me").unwrap();
    assert!(!path.exists());
    assert!(!metadata_path.exists());
    assert!(store.path_by_id("delete-me").is_err());
}

#[test]
fn interrupted_pre_commit_delete_restores_metadata_idempotently() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("rollback-delete.jsonl");
    drop(Session::create(&path).unwrap());
    store.rename("rollback-delete", "Keep this name").unwrap();
    store
        .set_lifecycle("rollback-delete", SessionStorageLifecycle::Trash, 12_000)
        .unwrap();
    let metadata_path = store.metadata_path("rollback-delete").unwrap();
    let staged_metadata = store
        .metadata_dir()
        .join(".delete-rollback-delete-deadbeefdeadbeef");
    std::fs::rename(&metadata_path, &staged_metadata).unwrap();
    let staged_transcript = store.dir().join(".delete-rollback-delete-deadbeefdeadbeef");
    std::fs::write(&staged_transcript, b"stale staging file").unwrap();

    store.rollback_permanent_delete("rollback-delete").unwrap();
    store.rollback_permanent_delete("rollback-delete").unwrap();

    let metadata = store.load_metadata("rollback-delete").unwrap();
    assert_eq!(metadata.name.as_deref(), Some("Keep this name"));
    assert_eq!(metadata.trashed_at_ms, Some(12_000));
    assert!(path.is_file());
    assert!(metadata_path.is_file());
    assert!(!staged_metadata.exists());
    assert!(!staged_transcript.exists());
}

#[test]
fn fork_provenance_round_trips_as_an_atomic_pair() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("fork.jsonl");
    let mut session = Session::create(path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("fork".into())],
        })))
        .unwrap();
    drop(session);

    let metadata = store
        .set_fork_provenance("fork", "source-session", "0042")
        .unwrap();
    assert_eq!(
        metadata.forked_from_session_id.as_deref(),
        Some("source-session")
    );
    assert_eq!(metadata.forked_from_entry_id.as_deref(), Some("0042"));
    let listed = store.list();
    assert_eq!(
        listed[0].forked_from_session_id.as_deref(),
        Some("source-session")
    );
    assert_eq!(listed[0].forked_from_entry_id.as_deref(), Some("0042"));

    let invalid = SessionUserMetadata {
        forked_from_session_id: Some("source-session".into()),
        ..SessionUserMetadata::default()
    };
    assert!(store.save_metadata("fork", &invalid).is_err());
}
