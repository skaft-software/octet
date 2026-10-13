//! The streaming search projection: an entry search that rescans and notifies
//! only on a real change, reconciliation that batches refreshes and removals,
//! in-place file edits detected without a directory mtime change, a large legacy
//! projection that stays searchable and cached, an old on-disk schema that has
//! to be rebuilt, an unreadable refresh that drops stale hits, and an oversized
//! catalog that still completes discovery and search.
//!
//! Separate from discovery because this is the one group that writes through
//! the projection while the catalog rows it reads stay fixed.

use super::*;

#[test]
fn entry_search_is_incremental_and_notifies_only_on_change() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let mut first = Session::create(store.dir().join("one.jsonl")).unwrap();
    first
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("alpha needle".into())],
        })))
        .unwrap();
    drop(first);
    let mut second = Session::create(store.dir().join("two.jsonl")).unwrap();
    second
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("beta needle".into())],
        })))
        .unwrap();
    drop(second);

    let scans = std::cell::Cell::new(0usize);
    let cold = store
        .search_entries_with("needle", 10, |path| {
            scans.set(scans.get() + 1);
            index_session_entries(path)
        })
        .unwrap();
    assert_eq!(
        scans.get(),
        2,
        "a cold index reads every session exactly once"
    );
    assert_eq!(cold.scanned_sessions, 2);
    assert!(cold.index_changed);
    assert_eq!(cold.hits.len(), 2);
    assert!(cold.hits.iter().any(|hit| hit.session_id == "one"));
    assert!(cold
        .hits
        .iter()
        .any(|hit| hit.text.contains("alpha needle")));

    let mut watcher = SessionSearchWatcher::default();
    assert!(
        watcher.observe(cold.revision),
        "the first observation is a change"
    );
    assert!(
        !watcher.observe(cold.revision),
        "an unchanged index is silent"
    );

    scans.set(0);
    let warm = store
        .search_entries_with("needle", 10, |path| {
            scans.set(scans.get() + 1);
            index_session_entries(path)
        })
        .unwrap();
    assert_eq!(
        scans.get(),
        0,
        "a warm index must not re-read any transcript"
    );
    assert!(!warm.index_changed);
    assert!(!watcher.observe(warm.revision));

    // Only the new/changed transcript is re-read.
    let mut third = Session::create(store.dir().join("three.jsonl")).unwrap();
    third
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("gamma needle".into())],
        })))
        .unwrap();
    drop(third);
    scans.set(0);
    let delta = store
        .search_entries_with("needle", 10, |path| {
            scans.set(scans.get() + 1);
            index_session_entries(path)
        })
        .unwrap();
    assert_eq!(scans.get(), 1, "only the changed session is re-read");
    assert!(delta.index_changed);
    assert!(
        watcher.observe(delta.revision),
        "the change fires the notification"
    );
    assert_eq!(delta.hits.len(), 3);
}

#[test]
fn entry_reconciliation_batches_refreshes_and_removals() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), root.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    for id in 0..65 {
        std::fs::write(store.dir().join(format!("s{id:03}.jsonl")), b"{}\n").unwrap();
    }
    let initial = store.entry_index_revision().unwrap();
    let cold = store
        .search_entries_with("needle", 100, |_| {
            Ok(vec![IndexedEntry {
                entry_id: "entry".into(),
                kind: IndexedEntryKind::User,
                text: "needle".into(),
            }])
        })
        .unwrap();
    assert_eq!(cold.scanned_sessions, 65);
    assert_eq!(cold.hits.len(), 65);
    assert_eq!(
        cold.revision - initial,
        3,
        "32-session batches, not one transaction per session"
    );
    let warm = store
        .search_entries_with("needle", 100, |_| panic!("unchanged transcript"))
        .unwrap();
    assert_eq!(warm.revision, cold.revision);
    for id in 0..65 {
        std::fs::remove_file(store.dir().join(format!("s{id:03}.jsonl"))).unwrap();
    }
    let empty = store.search_entries("needle", 100).unwrap();
    assert!(empty.hits.is_empty());
    assert_eq!(
        empty.revision - cold.revision,
        3,
        "stale removals are bounded too"
    );
}

#[test]
fn search_reconciles_existing_file_edits_without_directory_mtime_changes() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), root.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("existing.jsonl");
    let record = |text: &str| {
        format!("{{\"type\":\"entry\",\"id\":\"e\",\"value\":{{\"type\":\"message\",\"User\":{{\"content\":[{{\"Text\":\"{text}\"}}]}}}}}}\n")
    };
    std::fs::write(&path, record("old needle")).unwrap();
    assert_eq!(
        store.search_entries("old needle", 10).unwrap().hits.len(),
        1
    );
    let directory_mtime = store.dir().metadata().unwrap().modified().unwrap();
    let previous = path.metadata().unwrap().modified().unwrap();
    std::fs::write(&path, record("new needle")).unwrap();
    // No clock-resolution/timing assumption in the invalidation regression.
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(previous + std::time::Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        store.dir().metadata().unwrap().modified().unwrap(),
        directory_mtime
    );
    let refreshed = store.search_entries("new needle", 10).unwrap();
    assert_eq!(refreshed.scanned_sessions, 1);
    assert_eq!(refreshed.hits.len(), 1);
    assert!(store
        .search_entries("old needle", 10)
        .unwrap()
        .hits
        .is_empty());
}

#[test]
fn large_legacy_projection_stays_searchable_and_cached() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), root.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("legacy.jsonl");
    let id = "long-legacy-id".repeat(100);
    let record = serde_json::json!({"type":"entry", "id":id, "value":{"type":"message", "User":{"content":[
        {"Media":{"data":"A".repeat(2 * 1024 * 1024)}}, {"Text":"retained needle"}
    ]}}});
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let cold = store.search_entries("needle", 10).unwrap();
    assert_eq!(cold.hits.len(), 1);
    assert_eq!(cold.hits[0].entry_id, id);
    assert_eq!(cold.hits[0].text, "retained needle");
    let warm = store
        .search_entries_with("needle", 10, |_| {
            panic!("warm legacy projection must stay cached")
        })
        .unwrap();
    assert_eq!(warm.hits, cold.hits);
    assert_eq!(warm.scanned_sessions, 0);
}

#[test]
fn old_search_schema_rebuilds_and_unreadable_refresh_removes_stale_hits() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("search.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("migration needle".into())],
        })))
        .unwrap();
    drop(session);
    assert_eq!(store.search_entries("needle", 10).unwrap().hits.len(), 1);
    let connection = rusqlite::Connection::open(SessionCatalog::path(store.dir())).unwrap();
    connection
        .execute_batch("DROP TABLE indexed_entry_grams; PRAGMA user_version = 4;")
        .unwrap();
    drop(connection);
    let rebuilt = store.search_entries("needle", 10).unwrap();
    assert_eq!(rebuilt.scanned_sessions, 1);
    assert_eq!(rebuilt.hits.len(), 1);
    std::fs::write(&path, "changed and temporarily unreadable").unwrap();
    let refreshed = store
        .search_entries_with("needle", 10, |_| anyhow::bail!("unreadable"))
        .unwrap();
    assert!(refreshed.hits.is_empty());
    assert!(refreshed.index_changed);
    let retry = store
        .search_entries_with("needle", 10, |_| anyhow::bail!("unreadable"))
        .unwrap();
    assert!(retry.hits.is_empty());
    assert!(!retry.index_changed);
}

#[test]
fn oversized_catalog_keeps_warm_discovery_and_search_complete() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let mut session = Session::create(store.dir().join("oldest.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("retained needle".into())],
        })))
        .unwrap();
    drop(session);
    assert_eq!(store.list().len(), 1);
    assert_eq!(store.search_entries("needle", 10).unwrap().hits.len(), 1);
    let connection = rusqlite::Connection::open(SessionCatalog::path(store.dir())).unwrap();
    connection.execute_batch("CREATE TABLE padding (data BLOB); INSERT INTO padding VALUES (zeroblob(65 * 1024 * 1024)); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(connection);
    let discovered = store.discover_with_summarizer(store.candidates(), false, |_| {
        panic!("warm oversized catalog must not rescan transcripts")
    });
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].title, "retained needle");
    let search = store
        .search_entries_with("needle", 10, |_| {
            panic!("warm oversized index must not rescan transcripts")
        })
        .unwrap();
    assert_eq!(search.hits.len(), 1);
    assert_eq!(search.scanned_sessions, 0);
}
