//! Entry records: prompt metadata, colours, structured tool output and
//! durable extension entries / labels.
//!
//! Part of the `session::tests` suite; the shared builders and
//! `use super::*;` preamble live in `session/tests.rs`.

use super::*;

#[test]
fn custom_message_round_trips_typed_metadata_without_model_details() {
    let directory = tempfile::tempdir().unwrap();
    let path = temp_path(&directory);
    let mut session = Session::create(&path).unwrap();
    let custom = CustomMessage {
        custom_type: "job".into(),
        content: CustomMessageContent::Parts(vec![
            CustomMessagePart::Text {
                text: "first".into(),
            },
            CustomMessagePart::Text {
                text: "second".into(),
            },
        ]),
        display: false,
        details: Some(serde_json::json!({"private": "NOT_MODEL_CONTEXT"})),
    };
    let id = session.append_custom_message(custom.clone(), None).unwrap();
    session.checkpoint(id.clone()).unwrap();
    assert_eq!(session.context().unwrap().len(), 1);
    drop(session);
    let resumed = Session::open(&path).unwrap();
    assert_eq!(
        resumed
            .entry(&id)
            .unwrap()
            .metadata
            .as_ref()
            .unwrap()
            .custom_message
            .as_ref(),
        Some(&custom)
    );
    let context = resumed.context().unwrap();
    let encoded = serde_json::to_string(&context).unwrap();
    assert!(!encoded.contains("NOT_MODEL_CONTEXT"));
    assert!(!encoded.contains("custom_type"));
    let Message::User(user) = &context[0] else {
        panic!("custom content must project as user")
    };
    assert!(
        matches!(&user.content[..], [UserPart::Text(a), UserPart::Text(b)] if a == "first" && b == "second")
    );
}

#[test]
fn prompt_metadata_persists_safe_identity_and_exact_normalized_color() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let valid = session
        .append_with_metadata(
            user("valid"),
            Some(EntryMetadata {
                prompt_model: Some(ModelId("custom/model-a".into())),
                prompt_model_source: Some("  deepseek  ".into()),
                custom_message: None,
                prompt_color: Some("  #22AACC  ".into()),
                display_text: Some("visible\ndraft".into()),
                run_outcome: None,
                tool_output: None,
                tool_composition: None,
                tool_started_unix_ms: None,
                tool_finished_unix_ms: None,
                native_steering: None,
                local_synthetic_assistant: false,
                extension_metadata: Default::default(),
            }),
        )
        .unwrap();
    let invalid = session
        .append_with_metadata(
            user("invalid"),
            Some(EntryMetadata {
                prompt_model: Some(ModelId("model\u{1b}[31m".into())),
                prompt_model_source: Some("#2243e6".into()),
                custom_message: None,
                prompt_color: Some("rgb(1,2,3)\u{1b}".into()),
                display_text: Some("bad\u{1b}".into()),
                run_outcome: None,
                tool_output: None,
                tool_composition: None,
                tool_started_unix_ms: None,
                tool_finished_unix_ms: None,
                native_steering: None,
                local_synthetic_assistant: false,
                extension_metadata: Default::default(),
            }),
        )
        .unwrap();
    drop(session);

    let session = Session::open(&path).unwrap();
    assert_eq!(
        session.entry(&valid).unwrap().metadata,
        Some(EntryMetadata {
            prompt_model: Some(ModelId("custom/model-a".into())),
            prompt_model_source: Some("deepseek".into()),
            custom_message: None,
            prompt_color: Some("#22aacc".into()),
            display_text: Some("visible\ndraft".into()),
            run_outcome: None,
            tool_output: None,
            tool_composition: None,
            tool_started_unix_ms: None,
            tool_finished_unix_ms: None,
            native_steering: None,
            local_synthetic_assistant: false,
            extension_metadata: Default::default(),
        })
    );
    assert_eq!(session.entry(&invalid).unwrap().metadata, None);
    let persisted = std::fs::read_to_string(path).unwrap();
    assert!(!persisted.contains("#2243e6"));
    assert!(persisted.contains("#22aacc"));
    assert!(!persisted.contains("[31m"));
}

#[test]
fn structured_tool_output_details_survive_reopen_without_entering_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let details = crate::tool::ToolOutputDetails::try_new(
        Some(serde_json::json!({
            "sources": [{"title": "Primary", "url": "https://example.test"}]
        })),
        Some(serde_json::json!({"cache": "miss", "elapsed_ms": 12})),
    )
    .unwrap();
    let id = session
        .append_with_metadata(
            EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("call-structured".into()),
                    content: vec![octet_ai::ToolResultPart::Text("Found one source.".into())],
                    is_error: false,
                    added_tool_names: None,
                })],
            })),
            Some(EntryMetadata {
                tool_output: Some(details.clone()),
                ..EntryMetadata::default()
            }),
        )
        .unwrap();
    let invalid_target = session
        .append_with_metadata(
            user("ordinary user message"),
            Some(EntryMetadata {
                tool_output: Some(details.clone()),
                ..EntryMetadata::default()
            }),
        )
        .unwrap();
    drop(session);

    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        reopened
            .entry(&id)
            .and_then(|entry| entry.metadata.as_ref())
            .and_then(|metadata| metadata.tool_output.as_ref()),
        Some(&details)
    );
    assert_eq!(reopened.entry(&invalid_target).unwrap().metadata, None);
    let context = reopened.context().unwrap();
    let Message::User(message) = &context[0] else {
        panic!("expected user tool-result message");
    };
    let UserPart::ToolResult(result) = &message.content[0] else {
        panic!("expected tool result");
    };
    assert_eq!(result.content.len(), 1);
    let persisted = std::fs::read_to_string(path).unwrap();
    assert!(persisted.contains("structured_content"));
    assert!(persisted.contains("elapsed_ms"));
}

#[test]
fn explicit_null_structured_tool_output_survives_session_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let details =
        crate::tool::ToolOutputDetails::try_new(Some(serde_json::Value::Null), None).unwrap();
    let id = session
        .append_with_metadata(
            EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("call-null".into()),
                    content: vec![octet_ai::ToolResultPart::Text("No value.".into())],
                    is_error: false,
                    added_tool_names: None,
                })],
            })),
            Some(EntryMetadata {
                tool_output: Some(details),
                ..EntryMetadata::default()
            }),
        )
        .unwrap();
    drop(session);

    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        reopened
            .entry(&id)
            .and_then(|entry| entry.metadata.as_ref())
            .and_then(|metadata| metadata.tool_output.as_ref())
            .and_then(crate::tool::ToolOutputDetails::structured_content),
        Some(&serde_json::Value::Null)
    );
    assert!(std::fs::read_to_string(path)
        .unwrap()
        .contains("\"structured_content\":null"));
}

#[test]
fn run_outcome_marker_is_durable_and_not_model_visible() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    session.append(user("question")).unwrap();
    session.append(assistant("answer")).unwrap();
    let outcome_id = session
        .append_run_outcome(SessionRunOutcome {
            status: SessionRunOutcomeStatus::Failed,
            message: Some("bounded failure".into()),
        })
        .unwrap();
    drop(session);

    let session = Session::open(&path).unwrap();
    let marker = session.entry(&outcome_id).expect("outcome marker");
    assert!(matches!(
        marker.value,
        EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        }
    ));
    assert_eq!(
        marker
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.run_outcome.as_ref()),
        Some(&SessionRunOutcome {
            status: SessionRunOutcomeStatus::Failed,
            message: Some("bounded failure".into()),
        })
    );
    assert_eq!(session.context().unwrap().len(), 2);
}

#[test]
fn prompt_colors_are_immutable_across_checkout_branch_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let first = session
        .append_with_metadata(
            user("first model"),
            Some(EntryMetadata {
                prompt_model: Some(ModelId("model-a".into())),
                prompt_color: Some("#123456".into()),
                ..EntryMetadata::default()
            }),
        )
        .unwrap();
    let abandoned = session.append(assistant("old branch")).unwrap();
    session.checkout(first.clone()).unwrap();
    let second = session
        .append_with_metadata(
            user("second model"),
            Some(EntryMetadata {
                prompt_model: Some(ModelId("model-b".into())),
                prompt_color: Some("#abcdef".into()),
                ..EntryMetadata::default()
            }),
        )
        .unwrap();
    assert_ne!(session.head(), Some(abandoned));
    drop(session);

    let session = Session::open(path).unwrap();
    assert_eq!(
        session
            .entry(&first)
            .and_then(|entry| entry.metadata.as_ref())
            .and_then(|metadata| metadata.prompt_color.as_deref()),
        Some("#123456")
    );
    assert_eq!(
        session
            .entry(&second)
            .and_then(|entry| entry.metadata.as_ref())
            .and_then(|metadata| metadata.prompt_color.as_deref()),
        Some("#abcdef")
    );
}

#[test]
fn extension_entries_are_durable_and_never_model_visible() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let id = session
        .append_extension_entry(
            "octet.todo",
            Some(7),
            "todo.created",
            serde_json::json!({ "text": "ship wave 1" }),
        )
        .unwrap();
    // The payload rides the same non-context marker envelope as
    // `append_run_outcome`, so every provider projection skips it and
    // older readers can still replay the record.
    assert!(matches!(
        &session.entry(&id).unwrap().value,
        EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        }
    ));
    assert!(session.context().unwrap().is_empty());
    let prompt = session.append(user("hello")).unwrap();
    let context = session.context().unwrap();
    assert_eq!(context.len(), 1, "only the user message is model-visible");
    assert!(
        !format!("{context:?}").contains("ship wave 1"),
        "extension payload text must never reach provider context"
    );
    // Nothing before the prompt is model-visible: the extension marker is
    // skipped exactly like every other configuration marker.
    assert!(session.context_before(&prompt).unwrap().is_empty());
    // A marker that is itself the active head still adds no context, and
    // the prior message remains the only model-visible contribution.
    let second = session
        .append_extension_entry(
            "octet.todo",
            None,
            "todo.updated",
            serde_json::json!({ "text": "ship wave 1" }),
        )
        .unwrap();
    assert_eq!(session.head(), Some(second.clone()));
    assert_eq!(session.context().unwrap().len(), 1);
    assert_eq!(session.context_before(&second).unwrap().len(), 1);
    assert!(!format!("{:?}", session.context().unwrap()).contains("ship wave 1"));
}

#[test]
fn extension_entry_round_trips_through_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let id = {
        let mut session = Session::create(&path).unwrap();
        session
            .append_extension_entry(
                "octet.todo",
                Some(11),
                "todo.created",
                serde_json::json!({ "items": ["a", "b"] }),
            )
            .unwrap()
    };
    let session = Session::open(&path).unwrap();
    let entry = session
        .extension_entry(&id, "octet.todo")
        .expect("payload resolves");
    assert_eq!(entry.entry_type, "todo.created");
    assert_eq!(entry.data, serde_json::json!({ "items": ["a", "b"] }));
    let metadata = session.entry(&id).unwrap().metadata.as_ref().unwrap();
    let stored = &metadata.extension_metadata["octet.todo"];
    assert!(!stored.public);
    assert_eq!(stored.provenance.extension, "octet.todo");
    assert_eq!(stored.provenance.process_generation, Some(11));
    assert_eq!(
        stored.value,
        serde_json::json!({
            "entry_type": "todo.created",
            "data": { "items": ["a", "b"] },
        })
    );
    assert!(metadata.public_extension_metadata().is_empty());
    assert!(session.extension_entry(&id, "other.namespace").is_none());
    assert!(session.context().unwrap().is_empty());
}

#[test]
fn extension_entry_refuses_invalid_input_without_state_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let anchor = session.append(user("anchor")).unwrap();
    let durable_bytes = std::fs::metadata(&path).unwrap().len();
    let long_type = "t".repeat(MAX_EXTENSION_ENTRY_TYPE_BYTES + 1);
    let mut deep = serde_json::json!(1);
    for _ in 0..=MAX_EXTENSION_ENTRY_METADATA_DEPTH {
        deep = serde_json::json!({ "a": deep });
    }
    // 255 single-kilobyte strings plus their array is exactly the node
    // bound, but whose encoded form exceeds the namespace value bound.
    let oversize = serde_json::Value::Array(
        (0..255)
            .map(|_| serde_json::json!("x".repeat(1024)))
            .collect(),
    );
    let cases = vec![
        ("Bad.Namespace", "note", serde_json::json!(1)),
        ("", "note", serde_json::json!(1)),
        ("octet..todo", "note", serde_json::json!(1)),
        ("octet.todo", "", serde_json::json!(1)),
        ("octet.todo", long_type.as_str(), serde_json::json!(1)),
        ("octet.todo", "no\nte", serde_json::json!(1)),
        (
            "octet.todo",
            "note",
            serde_json::json!("x".repeat(MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES)),
        ),
        ("octet.todo", "note", deep),
        ("octet.todo", "note", oversize),
    ];
    for (namespace, entry_type, data) in cases {
        let error = session
            .append_extension_entry(namespace, None, entry_type, data)
            .expect_err("invalid extension entry must be refused");
        assert!(matches!(error, SessionError::Limit(_)), "{error}");
    }
    assert_eq!(session.head(), Some(anchor));
    assert_eq!(session.entries().len(), 1);
    assert_eq!(session.context().unwrap().len(), 1);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), durable_bytes);
    // A payload that fits the namespace value bound is retained verbatim.
    let accepted = serde_json::json!({ "note": "x".repeat(8 * 1024) });
    let id = session
        .append_extension_entry("octet.todo", None, "note", accepted.clone())
        .unwrap();
    assert_eq!(
        session.extension_entry(&id, "octet.todo").unwrap().data,
        accepted
    );
}

#[test]
fn extension_entry_node_budget_includes_the_durable_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let accepted = serde_json::json!(vec![false; 253]);
    let id = session
        .append_extension_entry("octet.todo", None, "note", accepted.clone())
        .unwrap();
    for count in [254, 255, 256] {
        assert!(session
            .append_extension_entry(
                "octet.todo",
                None,
                "note",
                serde_json::json!(vec![false; count])
            )
            .is_err());
    }
    assert_eq!(
        session.extension_entry(&id, "octet.todo").unwrap().data,
        accepted
    );
    drop(session);
    let session = Session::open(&path).unwrap();
    assert_eq!(
        session.extension_entry(&id, "octet.todo").unwrap().data,
        accepted
    );
}

#[test]
fn entry_labels_are_replaceable_durable_and_clearable() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let first;
    let second;
    {
        let mut session = Session::create(&path).unwrap();
        first = session.append(user("one")).unwrap();
        second = session.append(assistant("two")).unwrap();
        session.set_entry_label(&first, "planning").unwrap();
        session.set_entry_label(&second, "answer").unwrap();
        session.set_entry_label(&first, "replanned").unwrap();
        assert_eq!(session.entry_label(&first), Some("replanned"));
        assert_eq!(session.entry_label(&second), Some("answer"));
        assert_eq!(session.entry_labels().len(), 2);
        assert!(session.entry_labels().len() <= session.entries().len());
        // Labels never move the head or change model-visible context.
        assert_eq!(session.head(), Some(second.clone()));
        assert_eq!(session.context().unwrap().len(), 2);
    }
    let mut session = Session::open(&path).unwrap();
    assert_eq!(session.entry_label(&first), Some("replanned"));
    assert_eq!(session.entry_label(&second), Some("answer"));
    session.set_entry_label(&first, "").unwrap();
    assert_eq!(session.entry_label(&first), None);
    assert!(session.entry_labels().contains_key(&second));
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.entry_label(&first), None);
    assert_eq!(reopened.entry_label(&second), Some("answer"));
}

#[test]
fn entry_label_refuses_unknown_entry_and_invalid_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let entry = session.append(user("one")).unwrap();
    let durable_bytes = std::fs::metadata(&path).unwrap().len();
    let unknown = EntryId("999".into());
    let error = session.set_entry_label(&unknown, "ghost").unwrap_err();
    assert!(matches!(error, SessionError::UnknownEntry(id) if id == unknown));
    let error = session
        .set_entry_label(&entry, &"x".repeat(MAX_ENTRY_LABEL_BYTES + 1))
        .unwrap_err();
    assert!(matches!(error, SessionError::Limit(_)), "{error}");
    let error = session.set_entry_label(&entry, "two\nlines").unwrap_err();
    assert!(matches!(error, SessionError::Limit(_)), "{error}");
    assert_eq!(session.entry_label(&entry), None);
    assert!(session.entry_labels().is_empty());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), durable_bytes);
    // The exact byte bound is accepted without control characters.
    let label = "y".repeat(MAX_ENTRY_LABEL_BYTES);
    session.set_entry_label(&entry, &label).unwrap();
    assert_eq!(session.entry_label(&entry), Some(label.as_str()));
}
