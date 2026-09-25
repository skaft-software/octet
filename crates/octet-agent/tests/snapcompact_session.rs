//! Durable bitmap compaction and subsequent handoff coverage.

use octet_agent::compaction::{prepare_handoff, CompactionDetails};
use octet_agent::session::{EntryValue, Session, SnapcompactCheckpoint};
use octet_ai::{Media, Message, UserMessage, UserPart};

fn user(text: &str) -> EntryValue {
    EntryValue::Message(Message::User(UserMessage {
        content: vec![UserPart::Text(text.to_owned())],
    }))
}

#[test]
fn bitmap_checkpoint_replays_after_reopen_and_preserves_source_for_next_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bitmap.jsonl");
    let mut session = Session::create(&path).unwrap();
    session.append(user("original goal")).unwrap();
    let first_kept = session.append(user("kept turn")).unwrap();
    session.append(user("newest turn")).unwrap();
    let png = Media::image_bytes(
        bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nexample"),
        "image/png".parse().unwrap(),
    );
    let checkpoint = SnapcompactCheckpoint {
        source_text: "[User]: original goal\n".into(),
        frames: vec![png],
    };
    let preview = session
        .preview_compaction_context(&first_kept, "Read frames", &checkpoint)
        .unwrap();
    session
        .compact_snapcompact(
            "Read frames".into(),
            first_kept,
            CompactionDetails::default(),
            checkpoint,
        )
        .unwrap();
    assert!(session.has_snapcompact_context().unwrap());
    assert_eq!(
        serde_json::to_value(&preview).unwrap(),
        serde_json::to_value(session.context().unwrap()).unwrap()
    );
    drop(session);

    let mut session = Session::open(&path).unwrap();
    assert!(session.has_snapcompact_context().unwrap());
    assert_eq!(
        serde_json::to_value(&preview).unwrap(),
        serde_json::to_value(session.context().unwrap()).unwrap()
    );
    let next_kept = session.append(user("later turn")).unwrap();
    let prepared = prepare_handoff(&session, &next_kept).unwrap();
    assert_eq!(
        prepared.previous_summary.as_deref(),
        Some("[User]: original goal\n")
    );
}
