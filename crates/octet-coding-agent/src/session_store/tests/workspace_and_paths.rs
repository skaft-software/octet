//! How a workspace is turned into a store directory and how that directory is
//! later recognised again: the workspace key is stable and one-way, the
//! `.workspace` marker repairs a directory whose private content changed, and a
//! brand-new session path is always inside the store with the expected prefix
//! and extension.
//!
//! Separate from the metadata and deletion tests because it is the only group
//! that asserts the mapping from a workspace to a location on disk, which is
//! the invariant every other store test takes for granted.

use super::*;

#[test]
fn per_workspace_dirs_are_stable_and_distinct() {
    let root = tempfile::tempdir().unwrap();
    let workspace_a = tempfile::tempdir().unwrap();
    let workspace_b = tempfile::tempdir().unwrap();
    let first = SessionStore::new(root.path(), workspace_a.path());
    let second = SessionStore::new(root.path(), workspace_a.path());
    let other = SessionStore::new(root.path(), workspace_b.path());
    assert_eq!(first.dir(), second.dir());
    assert_ne!(first.dir(), other.dir());
    assert!(first.dir().starts_with(root.path()));
}

#[cfg(unix)]
#[test]
fn workspace_marker_skips_identical_private_content_but_repairs_changes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    store.write_workspace_marker().unwrap();
    let marker = store.dir().join(WORKSPACE_MARKER);
    let initial = std::fs::metadata(&marker).unwrap();
    store.write_workspace_marker().unwrap();
    assert_eq!(std::fs::metadata(&marker).unwrap().ino(), initial.ino());

    std::fs::write(&marker, b"stale\n").unwrap();
    store.write_workspace_marker().unwrap();
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        format!("{}\n", workspace.path().display()).as_bytes()
    );

    // A matching but non-private file must not bypass the secure writer.
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
    store.write_workspace_marker().unwrap();
    assert_eq!(
        std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
        0o600
    );

    std::fs::remove_file(&marker).unwrap();
    let outside = root.path().join("outside");
    std::fs::write(&outside, b"untouched").unwrap();
    std::os::unix::fs::symlink(&outside, &marker).unwrap();
    assert!(store.write_workspace_marker().is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
}

#[test]
fn list_all_discovers_marked_workspace_stores_and_message_counts() {
    let root = tempfile::tempdir().unwrap();
    let workspace_a = tempfile::tempdir().unwrap();
    let workspace_b = tempfile::tempdir().unwrap();
    let store_a = SessionStore::new(root.path(), workspace_a.path());
    let store_b = SessionStore::new(root.path(), workspace_b.path());
    std::fs::create_dir_all(store_a.dir()).unwrap();
    std::fs::create_dir_all(store_b.dir()).unwrap();
    store_a.write_workspace_marker().unwrap();
    store_b.write_workspace_marker().unwrap();

    let path_a = store_a.new_path("a");
    let mut session_a = Session::create(&path_a).unwrap();
    session_a
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("a".into())],
        })))
        .unwrap();
    drop(session_a);
    let path_b = store_b.new_path("b");
    let mut session_b = Session::create(&path_b).unwrap();
    session_b
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("b".into())],
        })))
        .unwrap();
    session_b
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("b2".into())],
        })))
        .unwrap();
    drop(session_b);

    let all = store_a.list_all();
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|meta| {
        meta.workspace.as_deref() == Some(workspace_a.path()) && meta.message_count == 1
    }));
    assert!(all.iter().any(|meta| {
        meta.workspace.as_deref() == Some(workspace_b.path()) && meta.message_count == 2
    }));
}

#[test]
fn new_path_is_inside_dir_with_jsonl_extension_and_prefix() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    let path = store.new_path("2026-07-12T14-30-05Z");
    assert!(path.starts_with(store.dir()));
    assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("jsonl"));
    assert!(path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("2026-07-12T14-30-05Z-")));
}
