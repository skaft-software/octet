//! Responses API replay: the sidecar projection, route switching and
//! cache invalidation.
//!
//! Part of the `session::tests` suite; the shared builders and
//! `use super::*;` preamble live in `session/tests.rs`.

use super::*;

#[test]
fn responses_replay_cache_advances_suffix_without_copying_settled_payloads() {
    for turns in [16, 64, 256] {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("replay.jsonl")).unwrap();
        let endpoint = EndpointId("responses".into());
        let model = ModelId("m".into());
        session
            .append(user(&"settled prefix".repeat(1024)))
            .unwrap();
        let first = session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap();
        let octet_ai::responses::ResponsesReplayItem::User(first_user) = &first[0] else {
            panic!("user")
        };
        let UserPart::Text(first_text) = &first_user.content[0] else {
            panic!("text")
        };
        let first_text_ptr = first_text.as_ptr();
        drop(first);
        for turn in 0..turns {
            session.append(user("new request")).unwrap();
            session
                .append_assistant_turn(
                    AssistantMessage {
                        content: vec![AssistantPart::Text("answer".into())],
                        model: model.clone(),
                        protocol: Protocol::OpenAiResponses,
                    },
                    endpoint.clone(),
                    model.clone(),
                    Usage::default(),
                    None,
                    StopReason::EndTurn,
                    Some(responses_output(&format!("output-{turn}"))),
                )
                .unwrap();
            let replay = session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .unwrap();
            let again = session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .unwrap();
            assert!(Arc::ptr_eq(&replay, &again));
            let octet_ai::responses::ResponsesReplayItem::User(first_user) = &replay[0] else {
                panic!("user")
            };
            let UserPart::Text(first_text) = &first_user.content[0] else {
                panic!("text")
            };
            assert_eq!(
                first_text.as_ptr(),
                first_text_ptr,
                "settled payload was cloned"
            );
        }
        assert_eq!(session.responses_replay_work.get(), (1, turns * 3));
        let replay = session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap();
        let full = session
            .rebuild_responses_replay(&endpoint, &model)
            .unwrap()
            .unwrap();
        assert_eq!(replay_debug_json(&replay), replay_debug_json(&full));
        // An externally retained snapshot remains immutable on append.
        session.append(user("after snapshot")).unwrap();
        let newer = session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap();
        assert_eq!(newer.len(), replay.len() + 1);
    }
}

#[test]
fn responses_replay_cache_invalidates_routes_branches_compactions_and_repairs_gaps() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("boundaries.jsonl");
    let mut session = Session::create(&path).unwrap();
    let endpoint = EndpointId("responses".into());
    let model = ModelId("m".into());
    let root = session.append(user("root")).unwrap();
    assert_eq!(
        session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    let assistant = session.append(responses_assistant("answer")).unwrap();
    assert!(session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .is_none());
    session
        .append_responses_turn(
            assistant,
            endpoint.clone(),
            model.clone(),
            responses_output("raw"),
        )
        .unwrap();
    assert_eq!(
        session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap()
            .len(),
        2
    );
    assert!(session
        .responses_replay_snapshot(&EndpointId("other".into()), &model)
        .unwrap()
        .is_none());
    session
        .append_responses_compaction(
            endpoint.clone(),
            model.clone(),
            responses_compact_output("native"),
        )
        .unwrap();
    let native = session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap();
    assert!(matches!(
        &native[0],
        octet_ai::responses::ResponsesReplayItem::Compacted(_)
    ));
    assert_eq!(native.len(), 1);
    let kept = session.append(user("kept")).unwrap();
    session.compact("local summary", kept).unwrap();
    let local = session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap();
    assert_eq!(
        replay_debug_json(&local),
        replay_debug_json(
            &session
                .rebuild_responses_replay(&endpoint, &model)
                .unwrap()
                .unwrap()
        )
    );
    session.checkout(root).unwrap();
    assert_eq!(
        session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    session.checkout_root().unwrap();
    assert!(session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap()
        .is_empty());
    drop(session);
    assert!(Session::open(&path)
        .unwrap()
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap()
        .is_empty());
}

#[test]
fn responses_replay_legacy_fallback_does_not_rescan_history_per_turn() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("legacy.jsonl")).unwrap();
    let endpoint = EndpointId("responses".into());
    let model = ModelId("m".into());
    session.append(user("root")).unwrap();
    session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap();
    session.append(responses_assistant("legacy gap")).unwrap();
    assert!(session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .is_none());
    for turn in 0..64 {
        session.append(user("new request")).unwrap();
        let assistant = session.append(responses_assistant("answer")).unwrap();
        session
            .append_responses_turn(
                assistant,
                endpoint.clone(),
                model.clone(),
                responses_output(&format!("{turn}")),
            )
            .unwrap();
        assert!(session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .is_none());
    }
    assert_eq!(session.responses_replay_work.get(), (1, 1 + 64 * 3));
}

#[test]
fn responses_replay_survives_restart_and_uses_only_the_active_branch() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let endpoint = EndpointId("responses".into());
    let model = ModelId("m".into());
    let mut session = Session::create(&path).unwrap();
    let root = session.append(user("root")).unwrap();
    let abandoned_assistant = session.append(responses_assistant("old")).unwrap();
    session
        .append_responses_turn(
            abandoned_assistant,
            endpoint.clone(),
            model.clone(),
            responses_output("old_raw"),
        )
        .unwrap();

    session.checkout(root).unwrap();
    let active_assistant = session.append(responses_assistant("new")).unwrap();
    session
        .append_responses_turn(
            active_assistant,
            endpoint.clone(),
            model.clone(),
            responses_output("new_raw"),
        )
        .unwrap();
    session.append(user("follow up")).unwrap();
    drop(session);

    let reopened = Session::open(&path).unwrap();
    let replay = reopened
        .responses_replay_items(&endpoint, &model)
        .unwrap()
        .expect("the active branch is fully reconstructible");
    assert_eq!(replay.len(), 3);
    let octet_ai::responses::ResponsesReplayItem::Output(output) = &replay[1] else {
        panic!("assistant must be represented by raw output");
    };
    assert_eq!(output.items()[0].as_json()["id"], "new_raw");
    assert!(serde_json::to_string(&replay_debug_json(&replay))
        .unwrap()
        .contains("follow up"));
    assert!(!serde_json::to_string(&replay_debug_json(&replay))
        .unwrap()
        .contains("old_raw"));
}

#[test]
fn responses_replay_falls_back_on_route_mismatch_and_legacy_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let mut legacy = Session::create(dir.path().join("legacy.jsonl")).unwrap();
    legacy.append(user("prompt")).unwrap();
    legacy.append(responses_assistant("legacy answer")).unwrap();
    assert!(legacy
        .responses_replay_items(&EndpointId("responses".into()), &ModelId("m".into()))
        .unwrap()
        .is_none());

    let mut mismatch = Session::create(dir.path().join("mismatch.jsonl")).unwrap();
    mismatch.append(user("prompt")).unwrap();
    let assistant = mismatch
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("answer".into())],
            model: ModelId("other-model".into()),
            protocol: Protocol::OpenAiResponses,
        })))
        .unwrap();
    mismatch
        .append_responses_turn(
            assistant,
            EndpointId("other-endpoint".into()),
            ModelId("other-model".into()),
            responses_output("raw"),
        )
        .unwrap();
    assert!(mismatch
        .responses_replay_items(&EndpointId("responses".into()), &ModelId("m".into()))
        .unwrap()
        .is_none());
}

#[test]
fn responses_model_switch_uses_canonical_history_and_fresh_reasoning() {
    use octet_ai::{ReasoningConfig, ReasoningEffort};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("switch.jsonl");
    let endpoint = EndpointId("codex".into());
    let astra = ModelId("gpt-6-astra".into());
    let luna = ModelId("gpt-6-luna".into());
    let low = ReasoningConfig::Effort(ReasoningEffort::Low);
    let high = ReasoningConfig::Effort(ReasoningEffort::High);
    let mut session = Session::create(&path).unwrap();
    session.append(user("first prompt")).unwrap();
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("astra answer".into())],
            model: astra.clone(),
            protocol: Protocol::OpenAiResponses,
        })))
        .unwrap();
    session
        .append_responses_turn(
            assistant,
            endpoint.clone(),
            astra.clone(),
            responses_output("astra opaque"),
        )
        .unwrap();
    session
        .append(EntryValue::ResponsesReasoning {
            endpoint: endpoint.clone(),
            model: astra.clone(),
            baseline: low.clone(),
            update: Some(octet_ai::ResponsesConfigurationUpdate {
                reasoning: high.clone(),
            }),
        })
        .unwrap();
    session
        .append(EntryValue::Config {
            model: Some(luna.0.clone()),
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    session.append(user("second prompt")).unwrap();

    for session in [&session, &Session::open(&path).unwrap()] {
        assert!(session
            .responses_replay_snapshot(&endpoint, &luna)
            .unwrap()
            .is_none());
        assert_eq!(session.responses_reasoning(&endpoint, &luna).unwrap(), None);
        assert_eq!(
            session
                .context()
                .unwrap()
                .iter()
                .map(text_of)
                .collect::<Vec<_>>(),
            ["first prompt", "astra answer", "second prompt"]
        );
    }
    // Returning to Astra after Luna has responded must not revive Astra's
    // prior reasoning pin or replay either route's opaque output.
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("luna answer".into())],
            model: luna.clone(),
            protocol: Protocol::OpenAiResponses,
        })))
        .unwrap();
    session
        .append_responses_turn(
            assistant,
            endpoint.clone(),
            luna.clone(),
            responses_output("luna opaque"),
        )
        .unwrap();
    session
        .append(EntryValue::ResponsesReasoning {
            endpoint: endpoint.clone(),
            model: luna.clone(),
            baseline: low.clone(),
            update: None,
        })
        .unwrap();
    assert_eq!(
        session.responses_reasoning(&endpoint, &luna).unwrap(),
        Some((low, ReasoningConfig::Effort(ReasoningEffort::Low)))
    );
    assert_eq!(
        session.responses_reasoning(&endpoint, &astra).unwrap(),
        None
    );
    assert!(session
        .responses_replay_snapshot(&endpoint, &astra)
        .unwrap()
        .is_none());
}

#[test]
fn responses_sidecars_reject_non_authoritative_output() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session.append(user("prompt")).unwrap();
    let assistant = session.append(responses_assistant("answer")).unwrap();
    let error = session
        .append_responses_turn(
            assistant,
            EndpointId("responses".into()),
            ModelId("m".into()),
            octet_ai::ResponsesOutput::default(),
        )
        .unwrap_err();
    assert!(matches!(error, SessionError::InvalidResponsesSidecar(_)));

    let error = session
        .append_responses_compaction(
            EndpointId("responses".into()),
            ModelId("m".into()),
            responses_output("not-a-compaction"),
        )
        .unwrap_err();
    assert!(matches!(error, SessionError::InvalidResponsesSidecar(_)));
}

fn replay_debug_json(
    replay: &[octet_ai::responses::ResponsesReplayItem],
) -> Vec<serde_json::Value> {
    replay
        .iter()
        .map(|item| match item {
            octet_ai::responses::ResponsesReplayItem::User(user) => {
                serde_json::to_value(user).unwrap()
            }
            octet_ai::responses::ResponsesReplayItem::LocalAssistant(assistant) => {
                serde_json::to_value(assistant).unwrap()
            }
            octet_ai::responses::ResponsesReplayItem::Output(output) => {
                serde_json::to_value(output).unwrap()
            }
            octet_ai::responses::ResponsesReplayItem::ConfigurationUpdate(update) => {
                serde_json::to_value(update).unwrap()
            }
            octet_ai::responses::ResponsesReplayItem::Compacted(output) => {
                serde_json::to_value(output).unwrap()
            }
        })
        .collect()
}
