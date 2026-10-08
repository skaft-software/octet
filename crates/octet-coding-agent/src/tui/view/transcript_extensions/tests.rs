use super::*;
use octet_agent::session::{CustomMessage, CustomMessageContent};
use octet_agent::{AgentEvent, EntryId, ToolOutput, ToolProgress};

fn owner(generation: u64) -> ExtensionResourceOwner {
    ExtensionResourceOwner {
        session_id: "session".into(),
        extension_instance_id: "renderer".into(),
        process_generation: generation,
    }
}
fn message(display: bool) -> CustomMessage {
    CustomMessage {
        custom_type: "notice".into(),
        content: CustomMessageContent::Text("canonical text".into()),
        display,
        details: Some(json!({"original":42})),
    }
}
fn rows(text: &str) -> TranscriptRenderResponse {
    TranscriptRenderResponse {
        registered: true,
        lines: Some(vec![text.into()]),
        markdown: None,
        render_shell: None,
    }
}
fn rendered(shell: &InteractiveShell) -> String {
    let state = shell.state.borrow();
    let lines = state.rendered_transcript(state.size.0).join("\n");
    lines
}

#[test]
fn message_sources_stay_canonical_and_hidden_messages_never_render() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_transcript_render_owners(vec![owner(1)]);
    shell.append_custom_transcript_message(&EntryId("hidden".into()), &message(false), 100);
    assert!(shell.transcript_render_candidates().is_empty());
    shell.append_custom_transcript_message(&EntryId("durable-entry".into()), &message(true), 200);
    let candidate = shell.transcript_render_candidates().pop().unwrap();
    assert_eq!(candidate.source_id, "durable-entry");
    let TranscriptRenderContent::Message {
        message: source, ..
    } = &candidate.render
    else {
        panic!("message source")
    };
    assert_eq!(source["details"]["original"], 42);
    assert_eq!(source["timestamp"], 200);
    assert!(shell.accept_transcript_render(&candidate, owner(1), rows("\x1b[32mCUSTOM\x1b[0m")));
    assert!(rendered(&shell).contains("CUSTOM"));
    assert!(
        matches!(&shell.state.borrow().transcript[0], TranscriptBlock::Notice(text) if text.contains("canonical text"))
    );
    assert!(!shell.accept_transcript_render(&candidate, owner(2), rows("STALE")));
    shell.set_transcript_render_owners(vec![owner(2)]);
    assert!(!rendered(&shell).contains("CUSTOM"));
    assert!(rendered(&shell).contains("canonical text"));
}

#[test]
fn resumed_sources_keep_identity_private_data_and_tool_details_without_writing_session() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.jsonl");
    let mut session = octet_agent::Session::create(&path).unwrap();
    let visible = session.append_custom_message(message(true), None).unwrap();
    let hidden = session.append_custom_message(message(false), None).unwrap();
    let private = session
        .append_extension_entry("renderer", Some(7), "saved", json!({"private":9}))
        .unwrap();
    session
        .append_with_metadata(
            octet_agent::session::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::ToolResult(octet_ai::ToolResult {
                        tool_call_id: octet_ai::ToolCallId("resumed-call".into()),
                        content: vec![
                            octet_ai::ToolResultPart::Text("first".into()),
                            octet_ai::ToolResultPart::Text("second".into()),
                        ],
                        is_error: false,
                        added_tool_names: None,
                    })],
                },
            )),
            Some(octet_agent::session::EntryMetadata {
                tool_output: Some(
                    octet_agent::ToolOutputDetails::try_new(
                        None,
                        Some(json!({"pi_details":{"retained":11}})),
                    )
                    .unwrap(),
                ),
                ..Default::default()
            }),
        )
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.set_transcript_render_owners(vec![owner(1)]);
    super::super::transcript_hydration::append_hydrated_items(
        &mut shell.state.borrow_mut(),
        crate::hydrate::hydrate_transcript(&session).unwrap(),
    );
    let candidates = shell.transcript_render_candidates();
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == visible.0));
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.source_id == hidden.0));
    let entry = candidates
        .iter()
        .find(|candidate| candidate.source_id == private.0)
        .unwrap();
    assert_eq!(entry.namespace.as_deref(), Some("renderer"));
    let TranscriptRenderContent::Entry { entry, .. } = &entry.render else {
        panic!("entry source")
    };
    assert_eq!(entry["data"]["private"], 9);
    let result = candidates
        .iter()
        .find_map(|candidate| {
            if let TranscriptRenderContent::Tool { result, .. } = &candidate.render {
                result.as_ref()
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(result["content"].as_array().unwrap().len(), 2);
    assert_eq!(result["details"]["retained"], 11);
    for candidate in &candidates {
        assert!(shell.accept_transcript_render(candidate, owner(1), rows("presentation only")));
    }
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn resize_disclosure_revision_and_owner_fences_reject_old_frames() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_transcript_render_owners(vec![owner(1)]);
    shell.append_custom_transcript_message(&EntryId("durable".into()), &message(true), 100);
    let original = shell.transcript_render_candidates().pop().unwrap();
    shell.set_size(64, 30);
    assert!(!shell.accept_transcript_render(&original, owner(1), rows("OLD WIDTH")));
    let resized = shell.transcript_render_candidates().pop().unwrap();
    assert_eq!(resized.source_id, original.source_id);
    assert_ne!(resized.key.width, original.key.width);
    assert!(shell.accept_transcript_render(&resized, owner(1), rows("COLLAPSED")));
    assert!(rendered(&shell).contains("COLLAPSED"));
    shell.set_verbose_tools(true);
    assert!(!rendered(&shell).contains("COLLAPSED"));
    assert!(!shell.accept_transcript_render(&resized, owner(1), rows("OLD DISCLOSURE")));
    let expanded = shell.transcript_render_candidates().pop().unwrap();
    assert!(expanded.key.expanded);
    shell.state.borrow_mut().touch_block(0);
    assert!(!shell.accept_transcript_render(&expanded, owner(1), rows("OLD CONTENT")));
    let current = shell.transcript_render_candidates().pop().unwrap();
    assert!(shell.accept_transcript_render(&current, owner(1), rows("CURRENT")));
    shell.set_transcript_render_owners(Vec::new());
    assert!(!rendered(&shell).contains("CURRENT"));
    assert!(rendered(&shell).contains("canonical text"));
}

#[test]
fn private_entries_are_inert_and_keep_their_namespace_and_real_identity() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_transcript_render_owners(vec![owner(1)]);
    shell.append_private_transcript_entry("pi-one".into(), json!({"type":"custom","id":"entry-private","parentId":"before","customType":"saved","data":{"secret":7}}));
    assert!(!rendered(&shell).contains("secret"));
    let candidate = shell.transcript_render_candidates().pop().unwrap();
    assert_eq!(candidate.source_id, "entry-private");
    assert_eq!(candidate.namespace.as_deref(), Some("pi-one"));
    assert!(shell.accept_transcript_render(&candidate, owner(1), rows("OWN ENTRY")));
    assert!(rendered(&shell).contains("OWN ENTRY"));
    shell.set_transcript_render_owners(Vec::new());
    assert!(!rendered(&shell).contains("OWN ENTRY"));
    assert!(!rendered(&shell).contains("secret"));
}

#[test]
fn full_tool_partial_and_final_details_survive_without_animation_rpcs() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_transcript_render_owners(vec![owner(1)]);
    let id = octet_ai::ToolCallId("actual-tool-call".into());
    shell.on_agent_event(&AgentEvent::ToolStarted {
        id: id.clone(),
        name: "custom".into(),
        args: json!({"path":"file"}),
    });
    let output = ToolOutput::new("partial")
        .try_with_metadata(json!({"pi_details":{"progress":5}}))
        .unwrap();
    shell.on_agent_event(&AgentEvent::ToolProgress {
        id: id.clone(),
        progress: ToolProgress::PartialResult(Arc::new(output)),
    });
    let partial = shell
        .transcript_render_candidates()
        .into_iter()
        .find(|candidate| matches!(candidate.render, TranscriptRenderContent::Tool { .. }))
        .unwrap();
    let TranscriptRenderContent::Tool {
        tool_call_id,
        result,
        is_partial,
        ..
    } = &partial.render
    else {
        unreachable!()
    };
    assert_eq!(tool_call_id, "actual-tool-call");
    assert!(*is_partial);
    assert_eq!(result.as_ref().unwrap()["details"]["progress"], 5);
    shell.state.borrow_mut().advance_event_dot_animation_by(1);
    assert!(
        shell.transcript_key_current(&partial.key),
        "decoration is not content revision"
    );
    let final_output = ToolOutput::new("final")
        .try_with_metadata(json!({"pi_details":{"progress":10}}))
        .unwrap();
    shell.on_agent_event(&AgentEvent::ToolFinished {
        id,
        result: Ok(final_output),
        duration: std::time::Duration::from_millis(10),
    });
    assert!(!shell.transcript_key_current(&partial.key));
    let final_frame = shell
        .transcript_render_candidates()
        .into_iter()
        .find(|candidate| matches!(candidate.render, TranscriptRenderContent::Tool { .. }))
        .unwrap();
    let TranscriptRenderContent::Tool {
        result, is_partial, ..
    } = &final_frame.render
    else {
        unreachable!()
    };
    assert!(!is_partial);
    assert_eq!(result.as_ref().unwrap()["details"]["progress"], 10);
    assert_eq!(result.as_ref().unwrap()["content"][0]["text"], "final");
}

#[test]
fn transformed_markdown_is_only_presentation_and_negative_receipts_do_not_spin() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_transcript_render_owners(vec![owner(1)]);
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized("original Markdown".into()),
        )));
    let candidate = shell.transcript_render_candidates().pop().unwrap();
    assert!(shell.accept_transcript_render(
        &candidate,
        owner(1),
        TranscriptRenderResponse {
            registered: true,
            lines: None,
            markdown: Some("**TRANSFORMED**".into()),
            render_shell: None
        }
    ));
    assert!(rendered(&shell).contains("TRANSFORMED"));
    assert!(
        matches!(&shell.state.borrow().transcript[0], TranscriptBlock::Assistant(block) if block.text == "original Markdown")
    );
    assert!(shell.transcript_render_candidates().is_empty());
    shell.invalidate_transcript_renderer(&owner(1), Some(&candidate.source_id));
    let candidate = shell.transcript_render_candidates().pop().unwrap();
    assert!(shell.accept_transcript_render(
        &candidate,
        owner(1),
        TranscriptRenderResponse {
            registered: false,
            lines: None,
            markdown: None,
            render_shell: None
        }
    ));
    assert!(shell.transcript_render_candidates().is_empty());
    assert!(rendered(&shell).contains("original Markdown"));
}
