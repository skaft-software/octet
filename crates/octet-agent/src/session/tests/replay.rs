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
            .rebuild_responses_replay(&endpoint, &model, &mut Default::default())
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
                .rebuild_responses_replay(&endpoint, &model, &mut Default::default())
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

fn screenshot() -> octet_ai::Media {
    octet_ai::Media::image_bytes(
        bytes::Bytes::from_static(b"screenshot"),
        "image/png".parse().unwrap(),
    )
}

fn screenshot_result(call_id: &str) -> EntryValue {
    EntryValue::Message(Message::User(UserMessage {
        content: vec![UserPart::ToolResult(ToolResult {
            tool_call_id: ToolCallId(call_id.to_owned()),
            content: vec![octet_ai::ToolResultPart::Media(screenshot())],
            is_error: false,
            added_tool_names: None,
            tool_name: None,
        })],
    }))
}

#[test]
fn replay_screenshot_retention_work_is_independent_of_settled_history() {
    for turns in [16, 64, 256] {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("images.jsonl")).unwrap();
        let endpoint = EndpointId("responses".into());
        let model = ModelId("m".into());
        session.append(user("settled prefix")).unwrap();
        session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap();
        crate::session::replay::REPLAY_IMAGE_WORK.with(|work| work.set(0));
        for turn in 0..turns {
            session
                .append_assistant_turn(
                    AssistantMessage {
                        content: vec![],
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
            session
                .append(screenshot_result(&format!("call-{turn}")))
                .unwrap();
            session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap();
        }
        let work = crate::session::replay::REPLAY_IMAGE_WORK.with(|work| work.replace(0));
        assert!(
            work <= turns * (2 + MODEL_VISIBLE_TOOL_IMAGE_LIMIT),
            "{turns} screenshot turns visited {work} replay items; only the suffix and retained image window may be revisited"
        );
        let replay = session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap();
        let full = session
            .rebuild_responses_replay(&endpoint, &model, &mut Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(replay_debug_json(&replay), replay_debug_json(&full));
    }
}

#[test]
fn replay_image_window_preserves_adjacent_media_and_immutable_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("mixed.jsonl")).unwrap();
    let endpoint = EndpointId("responses".into());
    let model = ModelId("m".into());
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            // A prompt attachment never consumes the tool image budget.
            content: vec![
                UserPart::Text("prompt".into()),
                UserPart::Media(screenshot()),
            ],
        })))
        .unwrap();
    let EntryValue::Message(Message::User(mut mixed)) = screenshot_result("mixed") else {
        unreachable!()
    };
    let UserPart::ToolResult(result) = &mut mixed.content[0] else {
        unreachable!()
    };
    result.content = vec![octet_ai::ToolResultPart::Media(screenshot()); 6];
    mixed
        .content
        .extend([UserPart::Media(screenshot()), UserPart::Media(screenshot())]);
    session
        .append(EntryValue::Message(Message::User(mixed)))
        .unwrap();
    let snapshot = session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap();
    let original = replay_debug_json(&snapshot);
    assert_eq!(
        serde_json::to_string(&original)
            .unwrap()
            .matches(TOOL_IMAGE_PLACEHOLDER)
            .count(),
        4
    );
    for index in 0..3 {
        session
            .append(screenshot_result(&format!("later-{index}")))
            .unwrap();
        session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap();
    }
    let replay = session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap();
    assert_eq!(
        replay_debug_json(&snapshot),
        original,
        "retained snapshots are immutable"
    );
    let octet_ai::ResponsesReplayItem::User(prompt) = &replay[0] else {
        unreachable!()
    };
    assert!(matches!(prompt.content[1], UserPart::Media(_)));
    let octet_ai::ResponsesReplayItem::User(mixed) = &replay[1] else {
        unreachable!()
    };
    assert_eq!(
        mixed.content.len(),
        2,
        "only the newest adjacent screenshot remains"
    );
    assert!(matches!(mixed.content[1], UserPart::Media(_)));
    let UserPart::ToolResult(result) = &mixed.content[0] else {
        unreachable!()
    };
    assert_eq!(result.content.len(), 7);
    assert!(result.content.iter().all(|part| matches!(part,
        octet_ai::ToolResultPart::Text(text) if text == TOOL_IMAGE_PLACEHOLDER)));
    let full = session
        .rebuild_responses_replay(&endpoint, &model, &mut Default::default())
        .unwrap()
        .unwrap();
    assert_eq!(replay_debug_json(&replay), replay_debug_json(&full));
}

#[test]
fn replay_image_state_rebuilds_across_branch_route_compaction_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("boundaries-images.jsonl");
    let mut session = Session::create(&path).unwrap();
    let endpoint = EndpointId("responses".into());
    let model = ModelId("m".into());
    let root = session.append(user("root")).unwrap();
    session.append_assistant_turn(
        AssistantMessage { content: vec![], model: model.clone(), protocol: Protocol::OpenAiResponses },
        endpoint.clone(), model.clone(), Usage::default(), None, StopReason::EndTurn,
        Some(octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(
            serde_json::json!({"type":"computer_call", "call_id":"reused", "action":{"type":"screenshot"}})
        ).unwrap()])),
    ).unwrap();
    // Protect the result even when the call and result arrive in separate suffixes.
    session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap();
    session.append(screenshot_result("reused")).unwrap();
    for index in 0..6 {
        session
            .append(screenshot_result(&format!("old-{index}")))
            .unwrap();
        session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap();
    }
    let replay = session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap();
    let octet_ai::ResponsesReplayItem::User(native) = &replay[2] else {
        unreachable!()
    };
    let UserPart::ToolResult(result) = &native.content[0] else {
        unreachable!()
    };
    assert!(matches!(
        result.content[0],
        octet_ai::ToolResultPart::Media(_)
    ));
    // Abandon the native call. Its ID must not protect an ordinary result on a new branch.
    session.checkout(root).unwrap();
    let kept = session.append(screenshot_result("reused")).unwrap();
    session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap();
    for index in 0..6 {
        session
            .append(screenshot_result(&format!("new-{index}")))
            .unwrap();
        session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap();
    }
    for endpoint in [&endpoint, &EndpointId("another-route".into())] {
        let replay = session
            .responses_replay_snapshot(endpoint, &model)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_string(&replay_debug_json(&replay))
                .unwrap()
                .matches(TOOL_IMAGE_PLACEHOLDER)
                .count(),
            3
        );
    }
    session.compact("summary", kept.clone()).unwrap();
    session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap();
    session
        .append(screenshot_result("after-compaction"))
        .unwrap();
    let replay = session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap();
    let full = session
        .rebuild_responses_replay(&endpoint, &model, &mut Default::default())
        .unwrap()
        .unwrap();
    assert_eq!(replay_debug_json(&replay), replay_debug_json(&full));
    drop(session);
    let mut session = Session::open(&path).unwrap();
    let reopened = session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap();
    assert_eq!(replay_debug_json(&replay), replay_debug_json(&reopened));
    // Checkout must restore images omitted from the later projection, not reuse its window.
    session.checkout(kept).unwrap();
    let replay = session
        .responses_replay_snapshot(&endpoint, &model)
        .unwrap()
        .unwrap();
    assert_eq!(replay.len(), 2);
    assert!(!serde_json::to_string(&replay_debug_json(&replay))
        .unwrap()
        .contains(TOOL_IMAGE_PLACEHOLDER));
}

#[test]
fn replay_bounds_function_screenshots_but_preserves_native_computer_results() {
    for incremental in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("images.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        let endpoint = model.endpoint.id.clone();
        let model_id = model.spec.id.clone();
        session.append(user("drive")).unwrap();
        for index in 0..7 {
            let native = index == 0;
            let call_id = format!("call_{index}");
            let output = octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(
                if native {
                    serde_json::json!({"type":"computer_call", "call_id":call_id, "action":{"type":"screenshot"}, "unknown":"preserved"})
                } else {
                    serde_json::json!({"type":"function_call", "call_id":call_id, "name":"capture", "arguments":"{}", "unknown":"preserved"})
                }
            ).unwrap()]);
            session
                .append_assistant_turn(
                    AssistantMessage {
                        content: vec![],
                        model: model_id.clone(),
                        protocol: Protocol::OpenAiResponses,
                    },
                    endpoint.clone(),
                    model_id.clone(),
                    Usage::default(),
                    None,
                    StopReason::EndTurn,
                    Some(output),
                )
                .unwrap();
            session
                .append(EntryValue::Message(Message::User(UserMessage {
                    content: vec![UserPart::ToolResult(ToolResult {
                        tool_call_id: ToolCallId(call_id),
                        content: vec![
                            octet_ai::ToolResultPart::Text(format!("result {index}")),
                            octet_ai::ToolResultPart::Media(octet_ai::Media::image_bytes(
                                bytes::Bytes::from(format!("image-{index}")),
                                "image/png".parse().unwrap(),
                            )),
                        ],
                        is_error: false,
                        added_tool_names: None,
                        tool_name: None,
                    })],
                })))
                .unwrap();
            if incremental {
                session
                    .responses_replay_snapshot(&endpoint, &model_id)
                    .unwrap();
            }
        }
        let replay = session
            .responses_replay_snapshot(&endpoint, &model_id)
            .unwrap()
            .unwrap();
        let input = octet_ai::responses::encode_responses_replay(&model, None, &replay).unwrap();
        let items: Vec<_> = input.items().iter().map(|item| item.as_json()).collect();
        let function_outputs: Vec<_> = items
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .collect();
        assert_eq!(function_outputs.len(), 6);
        let encoded = serde_json::to_string(&function_outputs).unwrap();
        assert_eq!(encoded.matches("input_image").count(), 4);
        assert_eq!(encoded.matches(TOOL_IMAGE_PLACEHOLDER).count(), 2);
        let computer = items
            .iter()
            .find(|item| item["type"] == "computer_call_output")
            .unwrap();
        assert!(computer["output"]["image_url"].as_str().is_some());
        assert_eq!(
            items
                .iter()
                .filter(|item| item["unknown"] == "preserved")
                .count(),
            7
        );
        let full = session
            .rebuild_responses_replay(&endpoint, &model_id, &mut Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(replay_debug_json(&replay), replay_debug_json(&full));
    }
}
