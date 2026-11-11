//! The bitmap-compaction verdict is on the per-request compaction-policy
//! path, so it must not re-walk the whole branch every time. Appends keep or
//! install the cached verdict, checkouts and branch summaries invalidate it,
//! and a re-resolution stops at the newest compaction. The test-only walk
//! counter pins those properties without any timing assertion.

use super::*;

fn abs_walk(session: &Session) -> usize {
    session.snapcompact_walk_entries.get()
}

fn snapshot_checkpoint() -> SnapcompactCheckpoint {
    SnapcompactCheckpoint {
        source_text: "[User]: original goal\n".into(),
        frames: vec![octet_ai::Media::image_bytes(
            bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nexample"),
            "image/png".parse().unwrap(),
        )],
    }
}

#[test]
fn append_only_sessions_reuse_the_cached_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session.append(user("first")).unwrap();
    session.append(assistant("answer")).unwrap();
    session.append(user("second")).unwrap();

    // The first resolution walks the whole head chain (three non-compaction
    // entries), and every later call — with or without an intervening append
    // — must resolve without walking again.
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), 3);
    session.append(user("third")).unwrap();
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), 3, "append must keep the cached verdict");
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), 3, "cached calls must not walk");
}

#[test]
fn compaction_appends_install_their_flag_and_checkouts_invalidate() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session.append(user("goal")).unwrap();
    let kept = session.append(user("kept turn")).unwrap();
    session.append(user("newest turn")).unwrap();
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), 3);
    let before_cache_resolved = abs_walk(&session);

    // A text compaction is the new newest compaction and installs its own
    // (false) verdict; following appends keep it.
    session.compact("summary", kept.clone()).unwrap();
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(
        abs_walk(&session),
        before_cache_resolved,
        "compaction append must install the verdict without walking"
    );
    session.append(user("after summary")).unwrap();
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), before_cache_resolved);

    // A bitmap compaction flips the installed verdict the same way.
    session
        .compact_snapcompact(
            "Read frames".into(),
            kept.clone(),
            crate::compaction::CompactionDetails::default(),
            snapshot_checkpoint(),
        )
        .unwrap();
    assert!(session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), before_cache_resolved);

    // Re-branching below the compactions invalidates the verdict; the next
    // resolution walks only that branch's remaining entries.
    session.checkout(kept.clone()).unwrap();
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), before_cache_resolved + 2);
    // Appends on the re-branched chain keep the recomputed verdict cached.
    session.append(user("sibling")).unwrap();
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), before_cache_resolved + 2);
}

#[test]
fn reopen_resolves_lazily_for_the_restored_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    {
        let mut session = Session::create(&path).unwrap();
        session.append(user("goal")).unwrap();
        let kept = session.append(user("kept turn")).unwrap();
        session
            .compact_snapcompact(
                "Read frames".into(),
                kept,
                crate::compaction::CompactionDetails::default(),
                snapshot_checkpoint(),
            )
            .unwrap();
        session.append(user("newest turn")).unwrap();
        session.checkout_root().unwrap();
    }
    let session = Session::open(&path).unwrap();
    // The durable root head selects no entries, so there is no compaction.
    assert!(!session.has_snapcompact_context().unwrap());
    assert_eq!(abs_walk(&session), 0);
}
