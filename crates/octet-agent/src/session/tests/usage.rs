//! Usage accounting: per-operation records, picodollar carry, session
//! totals, delegated runs and the uncertain-usage ledger.
//!
//! Part of the `session::tests` suite; the shared builders and
//! `use super::*;` preamble live in `session/tests.rs`.

use super::*;

#[test]
fn usage_uncertainty_serializes_without_fictional_usage_or_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    assert!(!session.has_uncertain_usage());
    assert_eq!(
        session.usage_uncertainty_exposure(),
        Some(UsageUncertaintyBound {
            tokens: 0,
            cost_microdollars: Some(0)
        })
    );
    record_unknown_attempt(&mut session).unwrap();
    let bytes = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&bytes).unwrap(),
        serde_json::json!({
            "type": "usage_uncertainty",
            "record": {
                "endpoint": "codex",
                "model": "openai/gpt-5.4",
                "operation": "assistant_turn"
            }
        })
    );
    let record: SessionRecord = serde_json::from_str(&bytes).unwrap();
    let SessionRecord::UsageUncertainty {
        record,
        bound: None,
    } = record
    else {
        panic!("expected uncertainty, not known usage");
    };
    assert_eq!(session.usage_uncertainty_records(), &[record]);
    assert!(session.head().is_none());
    assert!(session.entries().is_empty());
    assert!(session.usage_records().is_empty());
    assert!(session.context().unwrap().is_empty());
    let reopened = Session::open_read_only(&path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert_eq!(reopened.usage_uncertainty_exposure(), None);
    assert_eq!(
        reopened.usage_uncertainty_records(),
        session.usage_uncertainty_records()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn bounded_uncertainty_sums_and_survives_replay_without_changing_known_totals() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    for (tokens, cost_microdollars) in [(45, Some(12)), (9, Some(3))] {
        session
            .record_usage_uncertainty_with_bound(
                EndpointId("codex".into()),
                ModelId("model".into()),
                "assistant_turn",
                Some(UsageUncertaintyBound {
                    tokens,
                    cost_microdollars,
                }),
            )
            .unwrap();
    }
    let exposure = UsageUncertaintyBound {
        tokens: 54,
        cost_microdollars: Some(15),
    };
    assert_eq!(session.usage_uncertainty_exposure(), Some(exposure));
    assert_eq!(session.total_cost_microdollars(), 0);
    let bytes = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(bytes.lines().next().unwrap()).unwrap()["bound"],
        serde_json::json!({"tokens":45,"cost_microdollars":12})
    );
    drop(session);
    let mut reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.usage_uncertainty_exposure(), Some(exposure));
    reopened.checkout_root().unwrap();
    assert_eq!(reopened.usage_uncertainty_exposure(), Some(exposure));
    reopened
        .record_usage_uncertainty_with_bound(
            EndpointId("codex".into()),
            ModelId("model".into()),
            "assistant_turn",
            Some(UsageUncertaintyBound {
                tokens: 4,
                cost_microdollars: None,
            }),
        )
        .unwrap();
    assert_eq!(
        reopened.usage_uncertainty_exposure(),
        Some(UsageUncertaintyBound {
            tokens: 58,
            cost_microdollars: None,
        })
    );
}

#[test]
fn old_reader_accepts_new_bound_as_unknown_sibling_field() {
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OldRecord {
        UsageUncertainty { record: UsageUncertaintyRecord },
    }
    let json = serde_json::json!({
        "type": "usage_uncertainty",
        "record": { "endpoint": "codex", "model": "model", "operation": "assistant_turn" },
        "bound": { "tokens": 4096, "cost_microdollars": 7 }
    });
    let OldRecord::UsageUncertainty { record } = serde_json::from_value(json).unwrap();
    assert_eq!(record.operation, "assistant_turn");
}

#[test]
fn usage_uncertainty_survives_success_checkpoint_checkout_and_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let prompt = session.append(user("first prompt")).unwrap();
    let completed = session.append(assistant("first completion")).unwrap();
    session.add_cost(17).unwrap();
    let checkpoint = session.checkpoint(prompt.clone()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let context = serde_json::to_value(&*session.context().unwrap()).unwrap();
    record_unknown_attempt(&mut session).unwrap();
    record_unknown_attempt(&mut session).unwrap();
    assert!(std::fs::read(&path).unwrap().starts_with(&before));
    assert_eq!(
        serde_json::to_value(&*session.context().unwrap()).unwrap(),
        context
    );
    assert_eq!(session.head(), Some(completed));
    assert!(session.usage_records().is_empty());
    // A later completed response does not erase earlier accepted exposure.
    let later = session.append(assistant("replacement completed")).unwrap();
    session
        .record_assistant_usage(
            later.clone(),
            EndpointId("codex".into()),
            ModelId("m".into()),
            Usage {
                total_tokens: 31,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    session.checkpoint(prompt.clone()).unwrap();
    session.compact("summary", later).unwrap();
    assert!(session.has_uncertain_usage());
    session.checkout(checkpoint.head).unwrap();
    assert!(session.has_uncertain_usage());
    session.restore_checkpoint(&prompt).unwrap();
    assert!(session.has_uncertain_usage());
    session.checkout_root().unwrap();
    assert!(session.has_uncertain_usage());
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(reopened.head().is_none());
    assert!(reopened.has_uncertain_usage());
    assert_eq!(reopened.usage_uncertainty_records().len(), 2);
    assert_eq!(reopened.usage_records().len(), 1);
    assert_eq!(reopened.usage_records()[0].usage.total_tokens, 31);
    assert_eq!(reopened.total_cost_microdollars(), 17);
    assert_eq!(reopened.total_cost_picodollars_remainder(), 0);
}

#[test]
fn usage_uncertainty_legacy_sessions_remain_certain() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let prompt = session.append(user("legacy prompt")).unwrap();
    session.append(assistant("legacy completion")).unwrap();
    session.checkpoint(prompt).unwrap();
    drop(session);
    let reopened = Session::open(path).unwrap();
    assert!(!reopened.has_uncertain_usage());
    assert!(reopened.usage_uncertainty_records().is_empty());
}

#[test]
fn usage_uncertainty_fork_projection_starts_independent_accounting() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut source = Session::create(&path).unwrap();
    let head = source.append(user("forkable prompt")).unwrap();
    source.add_cost(17).unwrap();
    record_unknown_attempt(&mut source).unwrap();
    let fork_path = dir.path().join("fork.jsonl");
    let fork = source.fork_to(&fork_path, Some(&head)).unwrap();
    assert_eq!(fork.head(), Some(head));
    assert!(!fork.has_uncertain_usage());
    assert_eq!(fork.total_cost_microdollars(), 0);
    assert!(fork.usage_records().is_empty());
    assert!(source.has_uncertain_usage());
    assert!(Session::open_read_only(&path)
        .unwrap()
        .has_uncertain_usage());
    assert!(!Session::open_read_only(&fork_path)
        .unwrap()
        .has_uncertain_usage());
}

#[test]
fn usage_uncertainty_append_failures_leave_memory_and_disk_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut writer = Session::create(&path).unwrap();
    let mut stale = Session::open(&path).unwrap();
    writer.append(user("concurrent writer")).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        record_unknown_attempt(&mut stale),
        Err(SessionError::ConcurrentModification)
    ));
    assert!(!stale.has_uncertain_usage());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut read_only = Session::open_read_only(&path).unwrap();
    assert!(record_unknown_attempt(&mut read_only).is_err());
    assert!(!read_only.has_uncertain_usage());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    writer.writer.state.lock().unwrap().records = MAX_SESSION_RECORDS;
    assert!(matches!(
        record_unknown_attempt(&mut writer),
        Err(SessionError::Limit(_))
    ));
    assert!(!writer.has_uncertain_usage());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn usage_uncertainty_identifiers_are_bounded_and_validated_on_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    for invalid in [
        "".to_string(),
        "x".repeat(129),
        "https://host/path".into(),
        "secret?token=value".into(),
        "Authorization: secret".into(),
        "line\nfeed".into(),
    ] {
        for field in 0..3 {
            let mut ids = [
                "codex".to_string(),
                "m".to_string(),
                "assistant_turn".to_string(),
            ];
            ids[field] = invalid.clone();
            let error = session
                .record_usage_uncertainty(
                    EndpointId(ids[0].clone()),
                    ModelId(ids[1].clone()),
                    ids[2].clone(),
                )
                .unwrap_err();
            assert!(matches!(error, SessionError::Limit(_)));
            assert!(!session.has_uncertain_usage());
            assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
            let record = serde_json::json!({"type":"usage_uncertainty", "record": {
                "endpoint": ids[0], "model": ids[1], "operation": ids[2]
            }});
            std::fs::write(&path, format!("{record}\n")).unwrap();
            assert!(matches!(
                Session::open_read_only(&path),
                Err(SessionError::Corrupt { line: 1, .. })
            ));
            std::fs::write(&path, "").unwrap();
        }
    }
    let with_payload = serde_json::json!({"type":"usage_uncertainty", "record": {
        "endpoint":"codex", "model":"m", "operation":"assistant_turn", "body":"forbidden"
    }});
    assert!(serde_json::from_value::<SessionRecord>(with_payload).is_err());
    session
        .record_usage_uncertainty(
            EndpointId("x".repeat(128)),
            ModelId("x".repeat(128)),
            "x".repeat(128),
        )
        .unwrap();
    assert!(Session::open(&path).unwrap().has_uncertain_usage());
}

#[test]
fn usage_uncertainty_survives_repair_of_a_later_torn_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    record_unknown_attempt(&mut session).unwrap();
    drop(session);
    let before = std::fs::read(&path).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"entry\"")
        .unwrap();
    let reopened = Session::open(&path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert_eq!(reopened.usage_uncertainty_records().len(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
