//! Branching and checkpoints: checkout, fork, and the ancestry rules that
//! decide which restore points survive replay.
//!
//! Part of the `session::tests` suite; the shared builders and
//! `use super::*;` preamble live in `session/tests.rs`.

use super::*;

#[test]
fn checkout_ancestor_and_continue_forms_branch_preserving_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut s = Session::create(&path).unwrap();
    let e1 = s.append(user("root")).unwrap();
    let e2 = s.append(assistant("branch-a")).unwrap();
    s.checkout(e1.clone()).unwrap();
    let e3 = s.append(assistant("branch-b")).unwrap();

    // The new entry forks from the ancestor, the old branch is intact.
    assert_eq!(s.entry(&e3).unwrap().parent, Some(e1.clone()));
    assert_eq!(s.entry(&e2).unwrap().parent, Some(e1));
    assert_eq!(s.entries().len(), 3);

    let ctx = s.context().unwrap();
    assert_eq!(ctx.len(), 2);
    assert_eq!(text_of(&ctx[1]), "branch-b");

    // Reopen: both branches still present, head on the new branch.
    drop(s);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.entries().len(), 3);
    assert_eq!(reopened.head(), Some(e3));
}

#[test]
fn checkout_root_and_continue_preserves_the_original_root_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut session = Session::create(&path).unwrap();
    let original_user = session.append(user("original prompt")).unwrap();
    let original_assistant = session.append(assistant("original answer")).unwrap();
    session.checkout_root().unwrap();
    let edited_user = session.append(user("edited prompt")).unwrap();

    assert_eq!(session.entry(&original_user).unwrap().parent, None);
    assert_eq!(
        session.entry(&original_assistant).unwrap().parent,
        Some(original_user.clone())
    );
    assert_eq!(session.entry(&edited_user).unwrap().parent, None);
    assert_eq!(session.entries().len(), 3);
    assert_eq!(session.context().unwrap().len(), 1);
    assert_eq!(
        text_of(&session.context().unwrap()[0]),
        "edited prompt",
        "the active branch starts at the edited root"
    );

    drop(session);
    let reopened = Session::open(path).unwrap();
    assert_eq!(reopened.entries().len(), 3);
    assert_eq!(reopened.head(), Some(edited_user));
    assert!(reopened.entry(&original_assistant).is_some());
}

#[test]
fn fork_to_copies_only_the_selected_committed_ancestor_chain() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.jsonl");
    let fork_path = dir.path().join("fork.jsonl");
    let mut source = Session::create(&source_path).unwrap();

    let root = source.append(user("root")).unwrap();
    let selected = source.append(assistant("selected answer")).unwrap();
    let later = source.append(user("later work")).unwrap();
    source.checkout(root.clone()).unwrap();
    let sibling = source.append(assistant("sibling answer")).unwrap();

    let mut fork = source.fork_to(&fork_path, Some(&selected)).unwrap();
    assert_eq!(fork.head(), Some(selected.clone()));
    assert_eq!(fork.entries().len(), 2);
    assert!(fork.entry(&root).is_some());
    assert!(fork.entry(&selected).is_some());
    assert!(fork.entry(&later).is_none());
    assert!(fork.entry(&sibling).is_none());
    let context = fork.context().unwrap();
    assert_eq!(context.len(), 2);
    assert_eq!(text_of(&context[0]), "root");
    assert_eq!(text_of(&context[1]), "selected answer");

    let continuation = fork.append(user("fork-only continuation")).unwrap();
    assert_eq!(
        fork.entry(&continuation).unwrap().parent,
        Some(selected.clone())
    );
    drop(fork);
    let reopened = Session::open(fork_path).unwrap();
    assert_eq!(reopened.head(), Some(continuation));
    assert_eq!(reopened.entries().len(), 3);

    assert_eq!(source.head(), Some(sibling));
    assert_eq!(source.entries().len(), 4);
}

#[test]
fn fork_to_stops_at_the_compaction_boundary_and_reroots_the_span() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.jsonl");
    let fork_path = dir.path().join("fork.jsonl");
    let mut source = Session::create(&source_path).unwrap();

    let root = source.append(user("root")).unwrap();
    let first_answer = source.append(assistant("first answer")).unwrap();
    let first_kept = source.append(user("second prompt")).unwrap();
    source.append(assistant("second answer")).unwrap();
    source.append(user("third prompt")).unwrap();
    source.append(assistant("third answer")).unwrap();
    let compaction = source
        .compact("summary of the replaced history", first_kept.clone())
        .unwrap();

    let fork = source.fork_to(&fork_path, Some(&compaction)).unwrap();
    assert_eq!(fork.head(), Some(compaction.clone()));
    // Only the retained span plus the boundary itself was copied.
    assert!(fork.entry(&root).is_none());
    assert!(fork.entry(&first_answer).is_none());
    assert!(fork.entry(&first_kept).is_some());
    assert_eq!(fork.entries().len(), 5);
    // The root-side entry of the retained span was detached from the
    // replaced history and re-rooted in the fork.
    assert_eq!(
        fork.entry(&first_kept).unwrap().parent,
        None,
        "retained span must be re-rooted"
    );

    // The fork replays exactly like the source: summary, then the span.
    let texts = |messages: &[Message]| messages.iter().map(text_of).collect::<Vec<_>>();
    let fork_context = fork.context().unwrap();
    assert_eq!(
        texts(&fork_context),
        texts(&source.context().unwrap()),
        "fork context must match the source context"
    );
    assert_eq!(
        texts(&fork_context),
        vec![
            "[summary of earlier conversation]\nsummary of the replaced history".to_string(),
            "second prompt".to_string(),
            "second answer".to_string(),
            "third prompt".to_string(),
            "third answer".to_string(),
        ]
    );

    // The re-rooted span survives a reopen: the file validates on its own.
    drop(fork);
    let reopened = Session::open(&fork_path).unwrap();
    assert_eq!(reopened.head(), Some(compaction));
    assert_eq!(reopened.entries().len(), 5);
    assert_eq!(
        reopened.entry(&first_kept).unwrap().parent,
        None,
        "re-rooting must be durable"
    );
}

#[test]
fn fork_to_stops_at_the_oldest_compaction_on_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.jsonl");
    let fork_path = dir.path().join("fork.jsonl");
    let mut source = Session::create(&source_path).unwrap();

    let root = source.append(user("root")).unwrap();
    source.append(assistant("first answer")).unwrap();
    let first_kept = source.append(user("second prompt")).unwrap();
    source.append(assistant("second answer")).unwrap();
    source.append(user("third prompt")).unwrap();
    let first_compaction = source.compact("first summary", first_kept.clone()).unwrap();
    let second_kept = source.append(user("fourth prompt")).unwrap();
    source.append(assistant("fourth answer")).unwrap();
    // A second, newer compaction whose boundary is the prompt appended
    // after the first one. Its replaced span subsumes the first boundary.
    let second_compaction = source
        .compact("second summary", second_kept.clone())
        .unwrap();

    let fork = source
        .fork_to(&fork_path, Some(&second_compaction))
        .unwrap();
    assert_eq!(fork.head(), Some(second_compaction));
    // The walk stops at the oldest boundary on the chain, so the first
    // compaction and its retained span are not copied: the newer summary
    // already subsumes them.
    assert!(fork.entry(&first_compaction).is_none());
    assert!(fork.entry(&first_kept).is_none());
    assert!(fork.entry(&root).is_none());
    assert_eq!(fork.entries().len(), 3);
    // The retained prompt's source-side parent is the first compaction,
    // which was not copied, so it must be re-rooted.
    assert_eq!(
        fork.entry(&second_kept).unwrap().parent,
        None,
        "retained span must be re-rooted"
    );
}

#[test]
fn fork_to_with_none_creates_an_empty_session() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.jsonl");
    let fork_path = dir.path().join("fork.jsonl");
    let mut source = Session::create(&source_path).unwrap();
    source.append(user("root")).unwrap();

    let mut fork = source.fork_to(&fork_path, None).unwrap();
    assert!(fork.head().is_none());
    assert!(fork.entries().is_empty());

    // The empty fork continues as a fresh root branch.
    let new_root = fork.append(user("fresh start")).unwrap();
    assert_eq!(
        fork.entry(&new_root).unwrap().parent,
        None,
        "first entry of an empty fork is a root"
    );
    drop(fork);
    let reopened = Session::open(&fork_path).unwrap();
    assert_eq!(reopened.entries().len(), 1);
    assert_eq!(reopened.head(), Some(new_root));
}

#[test]
fn completed_prompt_checkpoint_round_trips_and_restores_a_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let prompt = session.append(user("make a change")).unwrap();
    let completed = session.append(assistant("done")).unwrap();

    let checkpoint = session.checkpoint(prompt.clone()).unwrap();
    assert_eq!(checkpoint.head, completed);
    assert_eq!(session.head(), Some(completed.clone()));
    session.append(user("later branch")).unwrap();
    drop(session);

    let mut reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.checkpoints(), &[checkpoint]);
    reopened.restore_checkpoint(&prompt).unwrap();
    assert_eq!(reopened.head(), Some(completed.clone()));
    let branch = reopened.append(user("new branch")).unwrap();
    assert_eq!(reopened.entry(&branch).unwrap().parent, Some(completed));
    assert_eq!(
        text_of(reopened.context().unwrap().last().unwrap()),
        "new branch"
    );
}

#[test]
fn checkpoint_telemetry_round_trips_and_follows_the_active_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let first_prompt = session.append(user("first")).unwrap();
    session.append(assistant("first answer")).unwrap();
    let first_usage = Usage {
        input_tokens: 120,
        output_tokens: 30,
        total_tokens: 150,
        ..Usage::default()
    };
    let first = session
        .checkpoint_with_telemetry(first_prompt, Some(first_usage), Some(8_600))
        .unwrap();

    let second_prompt = session.append(user("second")).unwrap();
    session.append(assistant("second answer")).unwrap();
    session
        .checkpoint_with_telemetry(second_prompt, Some(Usage::default()), Some(0))
        .unwrap();
    session.checkout(first.head.clone()).unwrap();
    drop(session);

    let reopened = Session::open(path).unwrap();
    let active = reopened.latest_active_checkpoint().unwrap();
    assert_eq!(active, &first);
    assert_eq!(active.usage, Some(first_usage));
    assert_eq!(active.run_cost_microdollars, Some(8_600));
}

#[test]
fn latest_active_assistant_usage_is_per_request_and_branch_aware() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    let root = session.append(user("root")).unwrap();
    let abandoned = session.append(assistant("abandoned")).unwrap();
    session
        .record_assistant_usage(
            abandoned,
            EndpointId("provider".into()),
            ModelId("m".into()),
            Usage {
                total_tokens: 900,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    session.checkout(root).unwrap();
    session.append(user("active")).unwrap();
    let active = session.append(assistant("active answer")).unwrap();
    session
        .record_assistant_usage(
            active,
            EndpointId("provider".into()),
            ModelId("m".into()),
            Usage {
                total_tokens: 100,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    session
        .record_compaction_usage(
            EndpointId("provider".into()),
            ModelId("m".into()),
            Usage {
                total_tokens: 500,
                ..Usage::default()
            },
            None,
        )
        .unwrap();

    assert_eq!(
        session
            .latest_active_assistant_usage()
            .unwrap()
            .usage
            .total_tokens,
        100
    );
}

#[test]
fn per_operation_usage_round_trips_for_turns_and_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    session.append(user("first")).unwrap();
    let assistant = session.append(assistant("answer")).unwrap();
    let turn_usage = Usage {
        input_tokens: 50,
        cache_read_tokens: 100,
        output_tokens: 20,
        total_tokens: 170,
        ..Usage::default()
    };
    let turn_cost = Cost {
        total: 42,
        total_picodollars_remainder: 600_000,
        ..Cost::default()
    };
    session
        .record_assistant_usage(
            assistant.clone(),
            EndpointId("provider".to_string()),
            ModelId("m".to_string()),
            turn_usage,
            Some(turn_cost),
        )
        .unwrap();
    let compaction_usage = Usage {
        input_tokens: 75,
        output_tokens: 10,
        total_tokens: 85,
        ..Usage::default()
    };
    session
        .record_compaction_usage(
            EndpointId("provider".to_string()),
            ModelId("m".to_string()),
            compaction_usage,
            Some(Cost {
                total_picodollars_remainder: 600_000,
                ..Cost::default()
            }),
        )
        .unwrap();
    let expected = session.usage_records().to_vec();
    assert_eq!(expected[0].cost, Some(turn_cost));
    assert_eq!(expected[0].cost_microdollars, Some(42));
    assert_eq!(expected[0].session_cost_microdollars, Some(42));
    assert_eq!(
        expected[0].session_cost_picodollars_remainder,
        Some(600_000)
    );
    assert_eq!(expected[1].session_cost_microdollars, Some(43));
    assert_eq!(
        expected[1].session_cost_picodollars_remainder,
        Some(200_000)
    );
    assert!(expected[0].completed_at_unix_ms.is_some());
    assert_eq!(session.total_cost_microdollars(), 43);
    assert_eq!(session.total_cost_picodollars_remainder(), 200_000);
    drop(session);

    let reopened = Session::open(path).unwrap();
    assert_eq!(reopened.usage_records(), expected);
    assert_eq!(reopened.total_cost_microdollars(), 43);
    assert_eq!(reopened.total_cost_picodollars_remainder(), 200_000);
}

#[test]
fn usage_totals_fold_tool_turns_and_summaries_and_preserve_uncertainty() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    session.append(user("prompt")).unwrap();
    let assistant = session.append(assistant("answer")).unwrap();
    session
        .record_assistant_usage(
            assistant,
            EndpointId("provider".into()),
            ModelId("m".into()),
            Usage {
                input_tokens: 100,
                cache_read_tokens: 50,
                cache_write_tokens: 30,
                cache_write_1h_tokens: 25,
                output_tokens: 20,
                reasoning_tokens: 5,
                total_tokens: 200,
            },
            None,
        )
        .unwrap();
    session
        .record_compaction_usage(
            EndpointId("provider".into()),
            ModelId("m".into()),
            Usage {
                input_tokens: 75,
                output_tokens: 10,
                total_tokens: 85,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    session
        .record_delegated_agent_usage(DelegatedUsage {
            agent_id: "child".into(),
            turn_count: 1,
            tool_call_count: 2,
            endpoint: EndpointId("provider".into()),
            model: ModelId("m".into()),
            usage: Usage {
                total_tokens: 40,
                ..Usage::default()
            },
            cost: None,
        })
        .unwrap();

    let totals = crate::telemetry::schema::UsageTotals::from_records(session.usage_records());
    assert_eq!(totals.assistant_records, 1);
    assert_eq!(totals.summary_records, 1);
    assert_eq!(totals.delegated_records, 1);
    assert_eq!(totals.total_tokens, 200 + 85 + 40);
    assert_eq!(totals.own_context_total_tokens, 200 + 85);
    assert_eq!(totals.cache_write_tokens, 30);
    assert_eq!(totals.cache_write_1h_tokens, 25);
    assert_eq!(totals.cache_hit_rate(), Some(50.0 / 255.0));

    // Known usage is only a subtotal: durable uncertainty is preserved and
    // never rewritten as fabricated zero usage.
    assert!(!session.has_uncertain_usage());
    record_unknown_attempt(&mut session).unwrap();
    assert!(session.has_uncertain_usage());
    assert_eq!(
        crate::telemetry::schema::UsageTotals::from_records(session.usage_records()),
        totals,
        "recording uncertainty must not fabricate or alter known totals"
    );
    let durable = session.usage_records().to_vec();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert_eq!(reopened.usage_records(), durable);
}

#[test]
fn delegated_usage_is_durable_and_contributes_exact_session_cost() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let usage = Usage {
        input_tokens: 1_000,
        cache_read_tokens: 250,
        output_tokens: 80,
        reasoning_tokens: 20,
        total_tokens: 1_330,
        ..Usage::default()
    };
    let cost = Cost {
        input: 10,
        output: 4,
        reasoning: 1,
        cache_read: 1,
        total: 15,
        total_picodollars_remainder: 750_000,
        ..Cost::default()
    };
    session
        .record_delegated_agent_usage(DelegatedUsage {
            agent_id: "agent-1".into(),
            turn_count: 3,
            tool_call_count: 7,
            endpoint: EndpointId("provider".into()),
            model: ModelId("worker-model".into()),
            usage,
            cost: Some(cost),
        })
        .unwrap();
    assert_eq!(session.total_cost_microdollars(), 15);
    assert_eq!(session.total_cost_picodollars_remainder(), 750_000);
    assert!(matches!(
        &session.usage_records()[0].kind,
        UsageRecordKind::DelegatedAgent {
            agent_id,
            turn_count: 3,
            tool_call_count: 7,
        } if agent_id == "agent-1"
    ));
    drop(session);

    let reopened = Session::open(path).unwrap();
    assert_eq!(reopened.total_cost_microdollars(), 15);
    assert_eq!(reopened.usage_records()[0].usage, usage);
    assert_eq!(reopened.usage_records()[0].cost, Some(cost));
}

#[test]
fn checkpoint_rejects_non_user_and_non_ancestor_entries() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    let root = session.append(user("root")).unwrap();
    let old_prompt = session.append(user("old branch")).unwrap();
    let assistant_entry = session.append(assistant("done")).unwrap();
    assert!(matches!(
        session.checkpoint(assistant_entry),
        Err(SessionError::UnknownEntry(_))
    ));
    session.checkout(root).unwrap();
    session.append(user("new branch")).unwrap();
    assert!(matches!(
        session.checkpoint(old_prompt),
        Err(SessionError::NotAncestor(_))
    ));
}

#[test]
fn replay_rejects_a_checkpoint_whose_prompt_is_on_another_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let root = session.append(user("root")).unwrap();
    let abandoned_prompt = session.append(user("abandoned prompt")).unwrap();
    session.append(assistant("abandoned answer")).unwrap();
    session.checkout(root).unwrap();
    session.append(user("active prompt")).unwrap();
    let active_head = session.append(assistant("active answer")).unwrap();
    drop(session);

    let mut bytes = Vec::new();
    write_json_line(
        &mut bytes,
        &SessionRecord::Checkpoint {
            prompt: abandoned_prompt,
            head: active_head,
            usage: None,
            run_cost_microdollars: None,
        },
    )
    .unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();

    let error = Session::open(&path).unwrap_err();
    assert!(
        matches!(error, SessionError::Corrupt { line: 12, .. }),
        "{error}"
    );
    assert!(error.to_string().contains("not an ancestor"), "{error}");
}

#[test]
fn replay_validates_many_checkpoints_with_one_linear_ancestry_index() {
    const ENTRY_COUNT: u64 = 4_096;
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut bytes = Vec::new();
    let mut parent = None;
    let root = EntryId("001".into());
    for number in 1..=ENTRY_COUNT {
        let id = EntryId(format!("{number:03}"));
        let entry = Entry {
            id: id.clone(),
            parent: parent.clone(),
            metadata: None,
            timestamp_unix_ms: None,
            value: if number == 1 {
                user("checkpoint root")
            } else {
                EntryValue::Config {
                    model: None,
                    reasoning: None,
                    reasoning_mode: None,
                }
            },
        };
        write_json_line(&mut bytes, &SessionRecordRef::Entry(&entry)).unwrap();
        parent = Some(id);
    }
    let head = parent.unwrap();
    let total_cost = 0u64;
    let remainder = 0u32;
    write_json_line(
        &mut bytes,
        &SessionRecordRef::Head {
            id: &head,
            total_cost_microdollars: &total_cost,
            total_cost_picodollars_remainder: &remainder,
        },
    )
    .unwrap();
    let usage = None;
    let run_cost = None;
    for _ in 0..ENTRY_COUNT {
        write_json_line(
            &mut bytes,
            &SessionRecordRef::Checkpoint {
                prompt: &root,
                head: &head,
                usage: &usage,
                run_cost_microdollars: &run_cost,
            },
        )
        .unwrap();
    }
    std::fs::write(&path, bytes).unwrap();

    let session = Session::open_read_only(path).unwrap();
    assert_eq!(session.entries().len(), ENTRY_COUNT as usize);
    assert_eq!(session.checkpoints().len(), ENTRY_COUNT as usize);
}

#[test]
fn checkout_of_unknown_entry_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(temp_path(&dir)).unwrap();
    s.append(user("x")).unwrap();
    let err = s.checkout(EntryId("999".to_string())).unwrap_err();
    assert!(matches!(err, SessionError::UnknownEntry(_)));
}

#[test]
fn malformed_parent_reference_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let entry = r#"{"type":"entry","id":"001","parent":"000","value":{"type":"config","model":null,"reasoning":null}}"#;
    std::fs::write(&path, format!("{entry}\n{entry}\n")).unwrap();
    let err = Session::open(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::Corrupt { line: 1, .. }),
        "{err}"
    );
}
