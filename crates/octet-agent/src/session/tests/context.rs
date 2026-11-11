//! Model-visible context reconstruction: compaction boundaries, active
//! skill snapshots and tool-result coalescing.
//!
//! Part of the `session::tests` suite; the shared builders and
//! `use super::*;` preamble live in `session/tests.rs`.

use super::*;

#[test]
fn config_entries_persist_but_are_not_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut s = Session::create(&path).unwrap();
    s.append(user("hi")).unwrap();
    s.append(EntryValue::Config {
        model: Some("claude".to_string()),
        reasoning: Some("high".to_string()),
        reasoning_mode: None,
    })
    .unwrap();
    s.append(assistant("hello")).unwrap();
    drop(s);

    let reopened = Session::open(&path).unwrap();
    assert!(matches!(
        reopened.entries()[1].value,
        EntryValue::ResponsesReasoning { .. }
            | EntryValue::ResponsesSteering { .. }
            | EntryValue::Config { .. }
    ));
    let ctx = reopened.context().unwrap();
    assert_eq!(ctx.len(), 2, "config entries are not model-visible");
}

#[test]
fn active_skills_keep_chronological_order_across_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    for id in ["a", "b"] {
        session
            .append(EntryValue::SkillActivated {
                descriptor: skill_descriptor(id),
                instructions_hash: format!("{id}-hash"),
                instructions: format!("{id}-instructions"),
            })
            .unwrap();
    }
    // Compaction caches the state before this entry: [a, b].
    let first_kept = session.append(user("keep this")).unwrap();
    session.compact("summary", first_kept).unwrap();
    session
        .append(EntryValue::SkillActivated {
            descriptor: skill_descriptor("c"),
            instructions_hash: "c-hash".to_string(),
            instructions: "c-instructions".to_string(),
        })
        .unwrap();

    let state = session
        .resolve_active_skills(&session.head().unwrap())
        .unwrap();
    let ids: Vec<_> = state
        .active_skills
        .iter()
        .map(|skill| skill.descriptor.id.as_str())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
}

#[test]
fn deactivating_latest_skill_activation_does_not_resurrect_an_older_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session
        .append(EntryValue::SkillActivated {
            descriptor: skill_descriptor("audit"),
            instructions_hash: "old-hash".to_string(),
            instructions: "old instructions".to_string(),
        })
        .unwrap();
    let latest = session
        .append(EntryValue::SkillActivated {
            descriptor: skill_descriptor("audit"),
            instructions_hash: "new-hash".to_string(),
            instructions: "new instructions".to_string(),
        })
        .unwrap();
    session
        .append(EntryValue::SkillDeactivated {
            activation_id: latest,
            skill_id: "audit".to_string(),
        })
        .unwrap();

    let state = session
        .resolve_active_skills(&session.head().unwrap())
        .unwrap();
    assert!(state.active_skills.is_empty());
}

#[test]
fn repeated_compaction_uses_only_the_nearest_skill_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    let activation_id = session
        .append(EntryValue::SkillActivated {
            descriptor: skill_descriptor("audit"),
            instructions_hash: "instructions-hash".into(),
            instructions: "audit instructions".into(),
        })
        .unwrap();
    session
        .append(EntryValue::SkillResourceRead {
            activation_id: activation_id.clone(),
            skill_id: "audit".into(),
            resource_path: "reference.txt".into(),
            start_line: None,
            line_count: None,
            content_hash: "resource-hash".into(),
            content: "resource content".into(),
        })
        .unwrap();
    session.append(user("old user")).unwrap();
    let old = session.append(assistant("old assistant")).unwrap();
    session.append(user("recent user")).unwrap();
    let recent = session.append(assistant("recent assistant")).unwrap();

    // Append both markers after the same completed history, matching
    // repeated provider rejection before another assistant can be added.
    session.compact("first summary", old).unwrap();
    session.compact("replacement summary", recent).unwrap();

    let state = session
        .resolve_active_skills(&session.head().unwrap())
        .unwrap();
    assert_eq!(state.active_skills.len(), 1);
    assert_eq!(state.skill_resources.len(), 1);
    assert_eq!(state.skill_resources[0].resource_path, "reference.txt");
}

#[test]
fn compaction_reconstruction_matches_design_example() {
    // Entries: E1, E2, E3, C(first_kept=E2), E5, E6 — context must be
    // [summary, E2, E3, E5, E6].
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(temp_path(&dir)).unwrap();
    let _e1 = s.append(user("E1")).unwrap();
    let e2 = s.append(assistant("E2")).unwrap();
    let _e3 = s.append(user("E3")).unwrap();
    let _c = s.compact("what came before", e2).unwrap();
    let _e5 = s.append(assistant("E5")).unwrap();
    let _e6 = s.append(user("E6")).unwrap();

    let ctx = s.context().unwrap();
    let texts: Vec<String> = ctx.iter().map(text_of).collect();
    assert_eq!(
        texts,
        vec![
            "[summary of earlier conversation]\nwhat came before".to_string(),
            "E2".to_string(),
            "E3".to_string(),
            "E5".to_string(),
            "E6".to_string(),
        ]
    );
}

#[test]
fn successive_compactions_expose_only_the_newest_summary() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session.append(user("old user")).unwrap();
    session.append(assistant("old assistant")).unwrap();
    session.append(user("middle user")).unwrap();
    let middle = session.append(assistant("middle assistant")).unwrap();
    session.append(user("recent user")).unwrap();
    let recent = session.append(assistant("recent assistant")).unwrap();

    session
        .compact("first overlapping summary", middle)
        .unwrap();
    session
        .compact("replacement summary including prior history", recent)
        .unwrap();

    let texts: Vec<String> = session.context().unwrap().iter().map(text_of).collect();
    assert_eq!(
        texts,
        [
            "[summary of earlier conversation]\nreplacement summary including prior history",
            "recent assistant",
        ]
    );
    assert!(texts.iter().all(|text| !text.contains("first overlapping")));
}

#[test]
fn compact_rejects_non_ancestor_first_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(temp_path(&dir)).unwrap();
    let e1 = s.append(user("root")).unwrap();
    let e2 = s.append(assistant("side")).unwrap();
    s.checkout(e1).unwrap();
    let _e3 = s.append(assistant("main")).unwrap();
    // e2 is on the abandoned branch, not an ancestor of the head.
    let err = s.compact("s", e2).unwrap_err();
    assert!(matches!(err, SessionError::NotAncestor(_)));
}

#[test]
fn compaction_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut s = Session::create(&path).unwrap();
    let e1 = s.append(user("old")).unwrap();
    let e2 = s.append(user("kept")).unwrap();
    assert_eq!(e1.0, "001");
    s.compact("summary text", e2).unwrap();
    drop(s);

    let reopened = Session::open(&path).unwrap();
    let ctx = reopened.context().unwrap();
    let texts: Vec<String> = ctx.iter().map(text_of).collect();
    assert_eq!(
        texts,
        vec![
            "[summary of earlier conversation]\nsummary text".to_string(),
            "kept".to_string(),
        ]
    );
}

#[test]
fn tool_results_persist_individually_and_coalesce_in_context() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(temp_path(&dir)).unwrap();
    s.append(user("do things")).unwrap();
    s.append(assistant("calling tools")).unwrap();
    s.append(tool_result("call_1", "one")).unwrap();
    s.append(tool_result("call_2", "two")).unwrap();

    // Individual persistence: two separate entries on disk.
    assert_eq!(s.entries().len(), 4);

    // Coalesced reconstruction: one user message with both results.
    let ctx = s.context().unwrap();
    assert_eq!(ctx.len(), 3);
    match &ctx[2] {
        Message::User(u) => {
            assert_eq!(u.content.len(), 2);
            assert!(u
                .content
                .iter()
                .all(|p| matches!(p, UserPart::ToolResult(_))));
        }
        _ => panic!("expected coalesced user message"),
    }
}

#[test]
fn plain_user_text_does_not_coalesce_with_tool_results() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(temp_path(&dir)).unwrap();
    s.append(assistant("calling tool")).unwrap();
    s.append(tool_result("call_1", "one")).unwrap();
    s.append(user("interjection")).unwrap();
    let ctx = s.context().unwrap();
    assert_eq!(ctx.len(), 3);
}

#[test]
fn parallel_tool_results_stay_ahead_of_adjacent_media() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut s = Session::create(&path).unwrap();
    s.append(assistant("calling tools")).unwrap();
    s.append(EntryValue::Message(Message::User(UserMessage {
        content: vec![
            UserPart::ToolResult(ToolResult {
                tool_call_id: ToolCallId("call_1".into()),
                content: vec![],
                is_error: false,
                added_tool_names: None,
            }),
            UserPart::Media(octet_ai::Media::image_bytes(
                bytes::Bytes::from_static(b"first"),
                "image/png".parse().unwrap(),
            )),
        ],
    })))
    .unwrap();
    s.append(tool_result("call_2", "two")).unwrap();

    let assert_order = |context: Vec<Message>| {
        let Message::User(turn) = context.last().unwrap() else {
            panic!("expected a coalesced user turn");
        };
        assert_eq!(turn.content.len(), 3);
        assert!(matches!(
            &turn.content[0],
            UserPart::ToolResult(result) if result.tool_call_id.0 == "call_1"
        ));
        assert!(matches!(
            &turn.content[1],
            UserPart::ToolResult(result) if result.tool_call_id.0 == "call_2"
        ));
        assert!(matches!(&turn.content[2], UserPart::Media(_)));
    };
    assert_order(s.context().unwrap());

    drop(s);
    assert_order(Session::open(path).unwrap().context().unwrap());
}

#[test]
fn reopening_rejects_a_semantically_invalid_compact_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let covered = session.append(user("prompt")).unwrap();
    drop(session);

    let malformed = serde_json::json!({
        "type": "entry",
        "id": "999",
        "parent": covered,
        "value": {
            "type": "responses_compaction",
            "endpoint": "responses",
            "model": "m",
            "covered_through": covered,
            "output": [{"type": "message", "id": "not-compacted"}]
        }
    });
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    serde_json::to_writer(&mut file, &malformed).unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);

    let error = Session::open(&path).unwrap_err();
    assert!(matches!(error, SessionError::Corrupt { .. }), "{error}");
    assert!(error.to_string().contains("direct checkpoint"), "{error}");
}

#[test]
fn native_responses_compaction_is_a_branch_local_replay_base() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = EndpointId("responses".into());
    let model = ModelId("m".into());
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session.append(user("old prompt")).unwrap();
    let assistant = session.append(responses_assistant("old answer")).unwrap();
    let covered = session
        .append_responses_turn(
            assistant,
            endpoint.clone(),
            model.clone(),
            responses_output("old_raw"),
        )
        .unwrap();
    session
        .append_responses_compaction(
            endpoint.clone(),
            model.clone(),
            responses_compact_output("compact_raw"),
        )
        .unwrap();
    session.append(user("after compact")).unwrap();

    let replay = session
        .responses_replay_items(&endpoint, &model)
        .unwrap()
        .unwrap();
    assert_eq!(replay.len(), 2);
    let octet_ai::responses::ResponsesReplayItem::Compacted(output) = &replay[0] else {
        panic!("native compact output must be the replay base");
    };
    assert_eq!(output.items()[1].as_json()["id"], "compact_raw");

    // Checking out the covered head abandons the checkpoint. The new
    // sibling reconstructs from canonical messages plus the turn sidecar.
    session.checkout(covered).unwrap();
    session.append(user("sibling")).unwrap();
    let sibling = session
        .responses_replay_items(&endpoint, &model)
        .unwrap()
        .unwrap();
    assert_eq!(sibling.len(), 3);
    let octet_ai::responses::ResponsesReplayItem::Output(output) = &sibling[1] else {
        panic!("ordinary turn output must be restored on the sibling");
    };
    assert_eq!(output.items()[0].as_json()["id"], "old_raw");
}

#[test]
fn a_lent_context_that_never_comes_back_is_rebuilt_from_the_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let endpoint = EndpointId("openai".into());
    let model = ModelId("gpt-5.4".into());

    for turn in 0..5 {
        let prompt_id = session.append(user(&format!("user turn {turn}"))).unwrap();
        let lent = session.take_context_for_request().unwrap();
        assert_eq!(lent.len(), turn * 2 + 1);
        // A request that fails before it is encoded never hands the history
        // back; the session must not wait for it or keep a stale copy.
        drop(lent);
        session.reclaim_lent_context();
        assert_eq!(session.context_ref().unwrap().len(), turn * 2 + 1);

        session
            .append_assistant_turn(
                octet_ai::AssistantMessage {
                    content: vec![AssistantPart::Text(format!("assistant turn {turn}"))],
                    model: model.clone(),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
                endpoint.clone(),
                model.clone(),
                Usage {
                    input_tokens: 100 + turn as u64 * 20,
                    output_tokens: 10,
                    total_tokens: 110 + turn as u64 * 20,
                    ..Usage::default()
                },
                Some(Cost {
                    input: 10,
                    output: 5,
                    reasoning: 0,
                    cache_read: 0,
                    cache_write: 0,
                    total: 15,
                    total_picodollars_remainder: 0,
                }),
                StopReason::EndTurn,
                None,
            )
            .unwrap();
        session
            .checkpoint_with_telemetry(
                prompt_id,
                Some(Usage {
                    input_tokens: 100,
                    output_tokens: 10,
                    total_tokens: 110,
                    ..Usage::default()
                }),
                Some(15),
            )
            .unwrap();
    }

    assert_eq!(session.context_clone_count.get(), 0);
    // The first lend builds the context; after that every lent context was
    // lost and rebuilt on the next access, once per turn. Appends extended
    // the rebuilt cache in place.
    assert_eq!(session.context_reconstruct_count.get(), 6);
    assert_eq!(session.active_branch_activity_counts(), (5, 0));
    assert!(session.has_priced_usage());
    assert!(!session.has_unpriced_usage());

    let reopened = Session::open(&path).unwrap();
    assert!(reopened.is_linear_chain());
    assert_eq!(reopened.active_branch_activity_counts(), (5, 0));
    assert!(reopened.has_priced_usage());
    assert!(!reopened.has_unpriced_usage());
    assert_eq!(
        reopened.latest_active_checkpoint().map(|cp| &cp.head),
        reopened.head_ref()
    );
}

/// Every change that rewrites earlier context gives the rewritten messages
/// new stamps, so nothing kept for them (request encoding, context metrics)
/// can be reused; a plain append keeps the stamps of the unchanged prefix.
#[test]
fn context_stamps_change_exactly_with_the_messages() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    let stamps = |session: &Session| {
        let length = session.context_ref().unwrap().len();
        let stamps = session.context_stamps.borrow().clone();
        assert_eq!(stamps.len(), length);
        stamps
    };
    let shares_none =
        |before: &[u64], after: &[u64]| after.iter().all(|stamp| !before.contains(stamp));

    session.append(user("question")).unwrap();
    let first = session.append(assistant("answer")).unwrap();
    let before = stamps(&session);
    session.append(user("follow-up")).unwrap();
    let after = stamps(&session);
    assert_eq!(after[..2], before[..], "an append keeps the prefix");
    assert!(shares_none(&before, &after[2..]));

    // Two tool results merge into one context message: it is restamped.
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![
                AssistantPart::ToolCall(octet_ai::ToolCall {
                    async_execution: false,
                    id: ToolCallId("a".into()),
                    name: "read".into(),
                    arguments_json: "{}".into(),
                    argument_error: None,
                }),
                AssistantPart::ToolCall(octet_ai::ToolCall {
                    async_execution: false,
                    id: ToolCallId("b".into()),
                    name: "read".into(),
                    arguments_json: "{}".into(),
                    argument_error: None,
                }),
            ],
            model: ModelId("m".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    session.append(tool_result("a", "one")).unwrap();
    let before = stamps(&session);
    session.append(tool_result("b", "two")).unwrap();
    let after = stamps(&session);
    assert_eq!(after.len(), before.len());
    assert_eq!(after[..before.len() - 1], before[..before.len() - 1]);
    assert_ne!(after.last(), before.last());

    let check_rebuilt = |session: &mut Session, change: &dyn Fn(&mut Session)| {
        let before = stamps(session);
        change(session);
        let after = stamps(session);
        assert!(shares_none(&before, &after), "{before:?} -> {after:?}");
    };
    check_rebuilt(&mut session, &|session| {
        session.checkout(first.clone()).unwrap();
    });
    check_rebuilt(&mut session, &|session| {
        session
            .branch_with_summary(Some(first.clone()), "summary".into(), Default::default())
            .unwrap();
    });
    check_rebuilt(&mut session, &|session| {
        session
            .append(EntryValue::SkillActivated {
                descriptor: skill_descriptor("skill"),
                instructions_hash: "hash".into(),
                instructions: "instructions".into(),
            })
            .unwrap();
    });
    check_rebuilt(&mut session, &|session| {
        let kept = session.append(user("kept")).unwrap();
        session.compact("summary", kept).unwrap();
    });
    // A lent context that never came back is rebuilt with new stamps too.
    check_rebuilt(&mut session, &|session| {
        drop(session.take_context_for_request().unwrap());
        session.reclaim_lent_context();
    });
}

/// Cached skill state carried across appends must match a fresh resolution,
/// including across a compaction whose skill snapshot is empty: the walk stops
/// at that compaction, so skills activated before its kept range end there.
#[test]
fn cached_skill_state_matches_a_fresh_resolution_across_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::SkillActivated {
            descriptor: skill_descriptor("a"),
            instructions_hash: "a-hash".to_string(),
            instructions: "a-instructions".to_string(),
        })
        .unwrap();
    session.append(user("before")).unwrap();
    let head = session.head().unwrap();
    assert_eq!(
        session
            .resolve_active_skills(&head)
            .unwrap()
            .active_skills
            .len(),
        1
    );
    let kept = session.append(user("kept")).unwrap();
    session
        .append(EntryValue::Compaction {
            summary: "summary".to_string(),
            snapcompact: None,
            first_kept: kept,
            active_skills: Vec::new(),
            skill_resources: Vec::new(),
            details: Default::default(),
        })
        .unwrap();
    session.append(user("after")).unwrap();
    let head = session.head().unwrap();
    let cached = session.resolve_active_skills(&head).unwrap();
    let reopened = Session::open_read_only(&path).unwrap();
    let fresh = reopened.resolve_active_skills(&head).unwrap();
    assert_eq!(
        serde_json::to_value(&cached).unwrap(),
        serde_json::to_value(&fresh).unwrap()
    );
    assert!(fresh.active_skills.is_empty());
}
