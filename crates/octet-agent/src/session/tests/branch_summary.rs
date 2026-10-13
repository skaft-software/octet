use super::*;
use crate::compaction::{prepare_handoff, serialize_conversation, CompactionDetails};

#[test]
fn summary_navigation_is_one_synced_entry_and_head_and_preserves_accounting() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let root = session.append(user("shared root")).unwrap();
    let abandoned = session.append(assistant("abandoned work")).unwrap();
    session
        .record_compaction_usage(
            EndpointId("local".into()),
            ModelId("m".into()),
            Usage::default(),
            Some(Cost {
                total: 7,
                ..Cost::default()
            }),
        )
        .unwrap();
    let before = std::fs::read(&path).unwrap().len();
    let summary = session
        .branch_with_summary(
            Some(root.clone()),
            "carried work".into(),
            CompactionDetails {
                read_files: vec!["read.rs".into()],
                modified_files: vec!["changed.rs".into()],
            },
        )
        .unwrap();
    let bytes = std::fs::read_to_string(&path).unwrap();
    let records = bytes[before..]
        .lines()
        .map(|line| serde_json::from_str::<SessionRecord>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        matches!(records.as_slice(), [SessionRecord::Entry(entry), SessionRecord::Head { id, .. }]
        if entry.id == summary && entry.parent == Some(root.clone()) && id == &summary)
    );
    assert!(matches!(&session.entry(&summary).unwrap().value,
        EntryValue::BranchSummary { from_entry, details, .. }
        if from_entry == &abandoned && details.modified_files == vec!["changed.rs"]));
    assert_eq!(session.total_cost_microdollars(), 7);
    assert_eq!(session.usage_records().len(), 1);
    assert!(session.entry(&abandoned).is_some());
    let context = session.context().unwrap();
    assert_eq!(context.len(), 2);
    assert_eq!(text_of(&context[1]), "The following is a summary of a branch that this conversation came back from:\n\n<summary>\ncarried work</summary>");
    session.append(user("next request")).unwrap();
    assert_eq!(session.context().unwrap().len(), 3);
    let kept = session.head().unwrap();
    let preparation = prepare_handoff(&session, &kept).unwrap();
    assert!(serialize_conversation(&preparation.messages).contains("carried work"));
    assert_eq!(preparation.details.modified_files, vec!["changed.rs"]);
    let fork = session
        .fork_to(dir.path().join("fork.jsonl"), Some(&summary))
        .unwrap();
    assert!(
        fork.entry(&abandoned).is_none(),
        "provenance does not copy an abandoned branch"
    );
    assert_eq!(
        fork.context()
            .unwrap()
            .iter()
            .map(text_of)
            .collect::<Vec<_>>(),
        context.iter().map(text_of).collect::<Vec<_>>()
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.total_cost_microdollars(), 7);
    assert_eq!(reopened.usage_records().len(), 1);
    assert_eq!(reopened.head(), Some(kept));
    assert!(serialize_conversation(&reopened.context().unwrap()).contains("carried work"));
}

#[test]
fn root_summary_replays_canonically_and_does_not_inherit_sibling_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let endpoint = EndpointId("responses".into());
    let model = ModelId("m".into());
    session.append(user("source root")).unwrap();
    let old = session
        .append(responses_assistant("opaque old work"))
        .unwrap();
    session
        .append_responses_turn(
            old,
            endpoint.clone(),
            model.clone(),
            responses_output("old"),
        )
        .unwrap();
    session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap();
    let summary = session
        .branch_with_summary(None, "root handoff".into(), CompactionDetails::default())
        .unwrap();
    assert_eq!(session.entry(&summary).unwrap().parent, None);
    let canonical = session.context().unwrap();
    let replay = session
        .responses_replay_items(&endpoint, &model)
        .unwrap()
        .unwrap();
    assert_eq!(replay.len(), 1);
    let octet_ai::ResponsesReplayItem::User(user) = &replay[0] else {
        panic!("summary is canonical user input")
    };
    assert_eq!(
        text_of(&Message::User(user.clone())),
        text_of(&canonical[0])
    );
    let fork = session
        .fork_to(dir.path().join("root-fork.jsonl"), Some(&summary))
        .unwrap();
    assert_eq!(fork.entries().len(), 1);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.head(), Some(summary));
    assert_eq!(
        text_of(&reopened.context().unwrap()[0]),
        text_of(&canonical[0])
    );
}

#[test]
fn failed_or_invalid_summary_never_checkouts_or_consumes_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let root = session.append(user("root")).unwrap();
    let leaf = session.append(assistant("leaf")).unwrap();
    let before = std::fs::read(&path).unwrap();
    for summary in [
        String::new(),
        " \n\t".into(),
        "bad\0summary".into(),
        "x".repeat(crate::compaction::MAX_COMPACTION_HANDOFF_BYTES + 1),
    ] {
        assert!(session
            .branch_with_summary(Some(root.clone()), summary, CompactionDetails::default())
            .is_err());
    }
    assert!(session
        .branch_with_summary(
            Some(EntryId("missing".into())),
            "valid".into(),
            CompactionDetails::default()
        )
        .is_err());
    session.fail_next_append();
    assert!(session
        .branch_with_summary(
            Some(root.clone()),
            "valid".into(),
            CompactionDetails::default()
        )
        .is_err());
    assert_eq!(session.head(), Some(leaf));
    assert_eq!(session.entries().len(), 2);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        session
            .branch_with_summary(Some(root), "valid".into(), CompactionDetails::default())
            .unwrap(),
        EntryId("003".into())
    );
}

#[test]
fn completed_malformed_summary_is_corruption_and_torn_head_keeps_old_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let old = session.append(user("old")).unwrap();
    session
        .branch_with_summary(None, "valid summary".into(), CompactionDetails::default())
        .unwrap();
    drop(session);
    let bytes = std::fs::read_to_string(&path).unwrap();
    let lines = bytes.lines().collect::<Vec<_>>();
    // An entry without its final head is inert; there is no intermediate target checkout.
    std::fs::write(
        &path,
        format!(
            "{}\n{{\"type\":\"head\",",
            lines[..lines.len() - 1].join("\n")
        ),
    )
    .unwrap();
    let recovered = Session::open(&path).unwrap();
    assert_eq!(recovered.head(), Some(old));
    drop(recovered);
    std::fs::write(&path, bytes.replace("valid summary", " ")).unwrap();
    assert!(matches!(
        Session::open(&path),
        Err(SessionError::Corrupt { .. })
    ));
}

#[test]
fn stale_writer_cannot_publish_summary_or_move_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let old = session.append(user("old")).unwrap();
    let mut other = Session::open(&path).unwrap();
    other.append(user("concurrent")).unwrap();
    assert!(matches!(
        session.branch_with_summary(None, "valid".into(), CompactionDetails::default()),
        Err(SessionError::ConcurrentModification)
    ));
    assert_eq!(session.head(), Some(old));
    assert_eq!(session.entries().len(), 1);
}
