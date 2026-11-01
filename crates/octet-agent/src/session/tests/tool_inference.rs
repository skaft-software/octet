use super::*;

fn start(session: &mut Session, parent: &str) -> u64 {
    session
        .start_tool_inference(
            parent.into(),
            EndpointId("advisor-route".into()),
            ModelId("advisor-model".into()),
        )
        .unwrap()
}

fn provider_usage() -> Usage {
    Usage {
        input_tokens: 17,
        output_tokens: 5,
        total_tokens: 22,
        ..Usage::default()
    }
}

fn provider_cost() -> Cost {
    Cost {
        input: 2,
        output: 1,
        total: 3,
        total_picodollars_remainder: 600_001,
        ..Cost::default()
    }
}

#[test]
fn native_tool_inference_usage_preserves_exact_route_cost_and_picodollar_carry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let head = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("consult the advisor".into())],
        })))
        .unwrap();
    let context = serde_json::to_value(&*session.context().unwrap()).unwrap();
    for parent in ["tool-1", "tool-2"] {
        let attempt = start(&mut session, parent);
        session
            .record_tool_inference_usage(attempt, provider_usage(), Some(provider_cost()))
            .unwrap();
        assert!(session.tool_inference_attempts.is_empty());
        let record = session.usage_records().last().unwrap();
        assert_eq!(
            record.kind,
            UsageRecordKind::ToolInference {
                attempt,
                parent: parent.into(),
            }
        );
        assert_eq!(record.endpoint, Some(EndpointId("advisor-route".into())));
        assert_eq!(record.model, Some(ModelId("advisor-model".into())));
        assert_eq!(record.usage, provider_usage());
        assert_eq!(record.cost, Some(provider_cost()));
        assert!(record.completed_at_unix_ms.is_some());
        assert_eq!(record.stop_reason, None);
    }
    assert_eq!(session.total_cost_microdollars(), 7);
    assert_eq!(session.total_cost_picodollars_remainder(), 200_002);
    assert_eq!(session.head(), Some(head.clone()));
    assert_eq!(session.entries().len(), 1);
    assert_eq!(
        serde_json::to_value(&*session.context().unwrap()).unwrap(),
        context
    );
    assert!(!session.has_uncertain_usage());
    let records: Vec<SessionRecord> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 6);
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(r, SessionRecord::Head { .. }))
            .count(),
        1
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.total_cost_microdollars(), 7);
    assert_eq!(reopened.total_cost_picodollars_remainder(), 200_002);
    assert_eq!(reopened.usage_records().len(), 2);
    assert_eq!(reopened.head(), Some(head));
    assert!(!reopened.has_uncertain_usage());
    assert!(reopened.tool_inference_attempts.is_empty());
}

#[test]
fn native_tool_inference_crashed_pending_attempt_remains_unknown_without_writing_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let attempt = start(&mut session, "tool-1");
    assert!(session.has_uncertain_usage());
    assert_eq!(session.usage_uncertainty_exposure(), None);
    assert!(session.usage_uncertainty_records().is_empty());
    let before = std::fs::read(&path).unwrap();
    drop(session);
    for _ in 0..2 {
        let reopened = Session::open_read_only(&path).unwrap();
        assert!(reopened.tool_inference_attempts.contains_key(&attempt));
        assert!(reopened.usage_uncertainty_records().is_empty());
        assert!(reopened.has_uncertain_usage());
        assert_eq!(reopened.usage_uncertainty_exposure(), None);
        assert!(reopened.usage_records().is_empty());
        assert_eq!(reopened.total_cost_microdollars(), 0);
        assert!(reopened.head().is_none());
        assert!(reopened.context().unwrap().is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let mut reopened = Session::open(&path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert_eq!(start(&mut reopened, "tool-2"), attempt + 1);
}

#[test]
fn native_tool_inference_abandoned_attempt_stays_unknown_during_later_live_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let abandoned = start(&mut session, "abandoned-tool");
    let completed = start(&mut session, "completed-tool");
    session
        .record_tool_inference_usage(completed, provider_usage(), Some(provider_cost()))
        .unwrap();
    assert!(session.has_uncertain_usage());
    assert_eq!(session.usage_uncertainty_exposure(), None);
    assert!(session.tool_inference_attempts.contains_key(&abandoned));
    assert!(!session.tool_inference_attempts.contains_key(&completed));
    assert_eq!(session.usage_records().len(), 1);
    assert_eq!(session.total_cost_microdollars(), 3);
    assert_eq!(session.total_cost_picodollars_remainder(), 600_001);
    let reopened = Session::open_read_only(&path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert_eq!(reopened.usage_uncertainty_exposure(), None);
    assert_eq!(reopened.usage_records(), session.usage_records());
    assert_eq!(reopened.tool_inference_attempts.len(), 1);
}

#[test]
fn native_tool_inference_known_usage_is_its_atomic_terminal_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let attempt = start(&mut session, "tool-1");
    session
        .record_tool_inference_usage(attempt, provider_usage(), Some(provider_cost()))
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    drop(session);
    let reopened = Session::open_read_only(&path).unwrap();
    assert!(!reopened.has_uncertain_usage());
    assert!(reopened.tool_inference_attempts.is_empty());
    assert!(reopened.usage_uncertainty_records().is_empty());
    assert_eq!(reopened.usage_records().len(), 1);
    assert_eq!(
        reopened.usage_uncertainty_exposure(),
        Some(UsageUncertaintyBound {
            tokens: 0,
            cost_microdollars: Some(0),
        })
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn native_tool_inference_settled_undispatched_attempt_does_not_invent_usage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let attempt = start(&mut session, "tool-1");
    session.settle_tool_inference(attempt).unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(reopened.tool_inference_attempts.is_empty());
    assert!(reopened.usage_records().is_empty());
    assert!(reopened.usage_uncertainty_records().is_empty());
    assert!(!reopened.has_uncertain_usage());
    assert_eq!(reopened.total_cost_microdollars(), 0);
    assert!(reopened.entries().is_empty());
    assert!(reopened.head().is_none());
}

#[test]
fn native_tool_inference_unknown_dispatched_failure_keeps_bound_after_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let attempt = start(&mut session, "tool-1");
    let bound = UsageUncertaintyBound {
        tokens: 120,
        cost_microdollars: Some(7),
    };
    session
        .record_usage_uncertainty_with_bound(
            EndpointId("advisor-route".into()),
            ModelId("advisor-model".into()),
            "tool_inference",
            Some(bound),
        )
        .unwrap();
    session.settle_tool_inference(attempt).unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert!(reopened.tool_inference_attempts.is_empty());
    assert_eq!(reopened.usage_uncertainty_records().len(), 1);
    assert_eq!(reopened.usage_uncertainty_exposure(), Some(bound));
    assert!(reopened.usage_records().is_empty());
}

#[test]
fn native_tool_inference_failed_start_and_terminal_appends_do_not_mutate_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    session.fail_next_append();
    assert!(session
        .start_tool_inference(
            "tool-1".into(),
            EndpointId("route".into()),
            ModelId("model".into())
        )
        .is_err());
    assert_eq!(session.next_tool_inference_attempt, 0);
    assert!(session.tool_inference_attempts.is_empty());
    assert!(std::fs::read(&path).unwrap().is_empty());
    let attempt = start(&mut session, "tool-1");
    let before = std::fs::read(&path).unwrap();
    session.fail_next_append();
    assert!(session
        .record_tool_inference_usage(attempt, provider_usage(), Some(provider_cost()))
        .is_err());
    assert!(session.tool_inference_attempts.contains_key(&attempt));
    assert!(session.usage_records().is_empty());
    assert_eq!(session.total_cost_microdollars(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    session.fail_next_append();
    assert!(session.settle_tool_inference(attempt).is_err());
    assert!(session.tool_inference_attempts.contains_key(&attempt));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn native_tool_inference_replay_rejects_duplicate_usage_and_route_mismatch() {
    for mutate in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut session = Session::create(&path).unwrap();
        let attempt = start(&mut session, "tool-1");
        session
            .record_tool_inference_usage(attempt, provider_usage(), Some(provider_cost()))
            .unwrap();
        drop(session);
        let before = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<serde_json::Value> = before
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if mutate {
            lines[1]["record"]["endpoint"] = serde_json::json!("different-route");
        } else {
            lines.push(lines[1].clone());
        }
        let corrupt = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(&path, &corrupt).unwrap();
        assert!(matches!(
            Session::open(&path),
            Err(SessionError::Corrupt { .. })
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
    }
}
