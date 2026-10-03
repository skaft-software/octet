//! Projecting a live run, and a finished one, into the graphical transcript.
//! Tool titles, commands, test-result counts, search metadata, and progress
//! events are all redacted, bounded, and frozen at the moment the tool ended;
//! the run record then has to rehydrate all of it with the same ids and
//! timestamps after a restart, including a legacy session with no record at all.

use super::*;
use octet_agent::EntryMetadata;
use octet_ai::{AssistantMessage, Protocol, UserMessage};

use super::test_support::*;

#[test]
fn run_outcome_is_committed_live_and_replayed_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("outcome-replay.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("question".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("answer".into())],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    let known_entries = session.entries().len();
    let marker_id = session
        .append_run_outcome(SessionRunOutcome {
            status: SessionRunOutcomeStatus::Completed,
            message: None,
        })
        .unwrap();
    let session_id = SessionId::new("outcome-replay").unwrap();
    let mut projection = ProjectionState::new(known_entries);
    let live = project_new_entries(
        &session,
        directory.path(),
        &mut projection,
        Some(&RunId::new("run-1-1").unwrap()),
        None,
        None,
        &session_id,
    )
    .unwrap();
    let live_outcome = live
        .iter()
        .find(|item| matches!(&item.payload, ItemPayload::RunOutcome { .. }))
        .expect("live committed outcome");
    assert_eq!(live_outcome.lifecycle, ItemLifecycle::Committed);
    assert_eq!(
        live_outcome.durable_entry_id.as_ref().map(|id| id.as_str()),
        Some(marker_id.0.as_str())
    );
    let stable_item_id = live_outcome.id.clone();
    drop(session);

    let reopened = Session::open_read_only(&path).unwrap();
    let seed = seed_from_session(
        &reopened,
        session_id,
        SessionSeedOptions {
            workspace: directory.path(),
            project_id: None,
            model: ModelSelection {
                provider: "test".into(),
                model: "test-model".into(),
                reasoning: "off".into(),
            },
            authority: AuthorityProfile::FullAccess,
            generation: 2,
            meta: None,
            attachment_store: None,
            resource_store: None,
        },
    )
    .unwrap();
    let replayed = seed
        .snapshot
        .items
        .iter()
        .find(|item| matches!(&item.payload, ItemPayload::RunOutcome { .. }))
        .expect("replayed committed outcome");
    assert_eq!(replayed.id, stable_item_id);
    assert!(matches!(
        &replayed.payload,
        ItemPayload::RunOutcome {
            outcome: octet_serve_backend::RunOutcome::Completed,
            message: None,
            ..
        }
    ));
}

#[test]
fn synthetic_failed_turn_marker_is_hidden_live_and_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed-turn-marker.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("question".into())],
        })))
        .unwrap();
    let marker = "The previous assistant turn failed before completion. Do not continue that request unless the user asks again.";
    let marker_id = session
        .append_with_metadata(
            EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text(marker.into())],
                model: ModelId("test-model".into()),
                protocol: Protocol::AnthropicMessages,
            })),
            Some(EntryMetadata {
                local_synthetic_assistant: true,
                ..EntryMetadata::default()
            }),
        )
        .unwrap();
    let diagnostic = "provider=custom/e2e model=e2e-model phase=connection";
    session
        .append_run_outcome(SessionRunOutcome {
            status: SessionRunOutcomeStatus::Failed,
            message: Some(diagnostic.into()),
        })
        .unwrap();

    let session_id = SessionId::new("failed-turn-marker").unwrap();
    let mut projection = ProjectionState::new(0);
    let live = project_new_entries(
        &session,
        directory.path(),
        &mut projection,
        Some(&RunId::new("run-1-1").unwrap()),
        None,
        None,
        &session_id,
    )
    .unwrap();
    assert_eq!(projection.known_entries, session.entries().len());
    assert!(!live
        .iter()
        .any(|item| matches!(item.payload, ItemPayload::AssistantMessage { .. })));
    assert!(live.iter().any(|item| matches!(
        &item.payload,
        ItemPayload::RunOutcome {
            outcome: octet_serve_backend::RunOutcome::Failed,
            message: Some(message),
            ..
        } if message == diagnostic
    )));
    assert!(!serde_json::to_string(&live).unwrap().contains(marker));

    let branches = branch_graph(&session).unwrap();
    let marker_branch = branches
        .entries
        .iter()
        .find(|entry| entry.entry_id.as_str() == marker_id.0)
        .expect("synthetic marker remains as a structural branch node");
    assert_eq!(marker_branch.kind, SessionBranchEntryKind::Internal);
    assert!(!marker_branch.checkoutable);
    assert_eq!(marker_branch.label, "Internal session state");
    assert!(!serde_json::to_string(&branches).unwrap().contains(marker));
    branches.validate().unwrap();
    drop(session);

    let reopened = Session::open_read_only(&path).unwrap();
    let seed = seed_from_session(
        &reopened,
        session_id,
        SessionSeedOptions {
            workspace: directory.path(),
            project_id: None,
            model: ModelSelection {
                provider: "test".into(),
                model: "test-model".into(),
                reasoning: "off".into(),
            },
            authority: AuthorityProfile::FullAccess,
            generation: 2,
            meta: None,
            attachment_store: None,
            resource_store: None,
        },
    )
    .unwrap();
    let public_snapshot = serde_json::to_string(&seed.snapshot).unwrap();
    assert!(!public_snapshot.contains(marker));
    assert!(public_snapshot.contains(diagnostic));
    assert!(!seed
        .snapshot
        .items
        .iter()
        .any(|item| matches!(item.payload, ItemPayload::AssistantMessage { .. })));
}

#[test]
fn historical_projection_sanitizes_single_line_labels_and_tool_titles() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("hostile-historical-text.jsonl");
    let mut session = Session::create(&path).unwrap();
    let hostile_label = format!(
        "label\t\u{1b}\u{7}\u{202e}{}",
        " long historical branch text".repeat(24)
    );
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(hostile_label)],
        })))
        .unwrap();
    let hostile_command = format!(
        "echo\t\u{1b}\u{7}\u{202e}{}",
        " long historical command".repeat(32)
    );
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(octet_ai::ToolCall {
                async_execution: false,
                id: ToolCallId("call-hostile-historical-text".into()),
                name: "bash".into(),
                arguments_json: serde_json::to_string(&serde_json::json!({
                    "command": hostile_command,
                }))
                .unwrap(),
                argument_error: None,
            })],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();

    let seed = seed_from_session(
        &session,
        SessionId::new("hostile-historical-text").unwrap(),
        SessionSeedOptions {
            workspace: workspace.path(),
            project_id: None,
            model: ModelSelection {
                provider: "test".into(),
                model: "test-model".into(),
                reasoning: "off".into(),
            },
            authority: AuthorityProfile::FullAccess,
            generation: 1,
            meta: None,
            attachment_store: None,
            resource_store: None,
        },
    )
    .unwrap();

    seed.validate().unwrap();
    let label = seed
        .snapshot
        .branches
        .entries
        .iter()
        .find(|entry| entry.kind == SessionBranchEntryKind::UserMessage)
        .map(|entry| entry.label.as_str())
        .unwrap();
    let title = seed
        .snapshot
        .items
        .iter()
        .find_map(|item| match &item.payload {
            ItemPayload::ToolCall(activity) => Some(activity.title.as_str()),
            _ => None,
        })
        .unwrap();
    for projected in [label, title] {
        assert!(projected.len() <= 512);
        assert!(!projected.chars().any(char::is_control));
        assert!(!projected.contains('\u{202e}'));
    }
    assert!(label.len() <= 256);
}

#[test]
fn semantic_tool_projection_redacts_canaries_and_freezes_exit_timing() {
    let workspace = tempfile::tempdir().unwrap();
    let argument_canary = "sk-live-ARGUMENT-CANARY-123456";
    let output_canary = "ghp_OUTPUTCANARY123456789";
    let arguments = serde_json::json!({
        "command": format!("cargo test token={argument_canary}"),
        "cwd": "crates/octet"
    });
    let activity = semantic_tool_activity("bash", &arguments, workspace.path(), 10_000);
    assert_eq!(activity.kind, ToolKind::Command);
    assert_eq!(activity.phase, ActivityPhase::Verified);
    assert_eq!(
        activity.command_preview.as_deref(),
        Some("cargo test [redacted arguments]")
    );
    assert_eq!(activity.cwd.as_deref(), Some("crates/octet"));

    let raw = ToolOutput::new(format!("exit=7 duration=1.25s\nstderr:\n{output_canary}"));
    let (activity, mut result) = complete_tool_activity(
        activity,
        "bash",
        &Ok(raw),
        20_000,
        ProjectedToolProgress {
            observed_output_bytes: 99,
            dropped_output_bytes: 23,
        },
    );
    result.tool_call_item_id = ItemId::new("item-redaction-test").unwrap();
    assert_eq!(activity.status, ToolActivityStatus::Failed);
    assert_eq!(activity.exit_code, Some(7));
    assert_eq!(activity.duration_ms, Some(1_250));
    assert_eq!(result.duration_ms, 1_250);
    assert_eq!(result.dropped_output_bytes, 23);

    let public = serde_json::to_string(&(activity, result)).unwrap();
    for secret in [argument_canary, output_canary] {
        assert!(
            !public.contains(secret),
            "secret canary crossed the public projection: {public}"
        );
    }
    for forbidden_field in ["arguments", "content", "progress", "stdout", "stderr"] {
        assert!(
            !public.contains(&format!("\"{forbidden_field}\"")),
            "raw field crossed the public projection: {public}"
        );
    }
}

#[test]
fn semantic_command_projection_keeps_full_arbitrary_command() {
    let workspace = tempfile::tempdir().unwrap();
    let command = "rg -n 'worker shutdown' crates/octet-coding-agent/src && rustfmt --check crates/octet-coding-agent/src/lib.rs";
    let activity = semantic_tool_activity(
        "bash",
        &serde_json::json!({"command": command}),
        workspace.path(),
        10,
    );

    assert_eq!(activity.command_preview.as_deref(), Some(command));
    assert_eq!(activity.title, format!("Run {command}"));
    assert_eq!(activity.phase, ActivityPhase::Other);
}

#[test]
fn verified_test_command_projects_only_parser_proven_counts() {
    let workspace = tempfile::tempdir().unwrap();
    let item_id = ItemId::new("item-test-command").unwrap();
    let activity = semantic_tool_activity(
        "bash",
        &serde_json::json!({"command": "cargo test --workspace"}),
        workspace.path(),
        10,
    );
    let output = ToolOutput::new(format!(
        "exit=0 duration=0.10s\nstdout:\n{}",
        String::from_utf8_lossy(include_bytes!(
            "../../../../../../extensions/octet-serve/fixtures/test-results/cargo-libtest.txt"
        ))
    ));
    let (activity, _) = complete_tool_activity(
        activity,
        "bash",
        &Ok(output.clone()),
        110,
        ProjectedToolProgress::default(),
    );
    let projected =
        project_test_results(&item_id, &activity, &output).expect("supported test output");
    assert_eq!(projected.origin_item_id, item_id);
    assert_eq!(projected.framework, TestFramework::CargoLibtest);
    assert_eq!(projected.reported.total, None);
    assert_eq!(projected.reported.passed, None);
    assert_eq!(projected.suites[0].reported.passed, Some(2));
    assert_eq!(
        projected.verification,
        octet_serve_backend::TestVerificationOutcome::Passed
    );

    let unsupported = ToolOutput::new("exit=0 duration=0.01s\nstdout:\nbuild completed");
    assert!(project_test_results(&item_id, &activity, &unsupported).is_none());
}

#[test]
fn semantic_search_metadata_is_safe_bounded_and_workspace_relative() {
    let workspace = tempfile::tempdir().unwrap();
    let source_dir = workspace.path().join("src");
    std::fs::create_dir(&source_dir).unwrap();

    let local_search = semantic_tool_activity(
        "search",
        &serde_json::json!({
            "query": "focus trap",
            "path": "src",
            "cwd": source_dir,
        }),
        workspace.path(),
        1,
    );
    assert_eq!(local_search.kind, ToolKind::Search);
    assert_eq!(local_search.target.as_deref(), Some("focus trap in src"));
    assert_eq!(local_search.cwd.as_deref(), Some("src"));

    let web_search = semantic_tool_activity(
        "web_search",
        &serde_json::json!({
            "query": "Claude app local web search",
            "url": "https://example.test/docs?token=query-secret#private",
        }),
        workspace.path(),
        2,
    );
    assert_eq!(web_search.kind, ToolKind::Web);
    assert_eq!(
        web_search.target.as_deref(),
        Some("https://example.test/docs")
    );

    let query_canary = "sk-live-QUERY-CANARY-123456";
    let redacted_search = semantic_tool_activity(
        "web_search",
        &serde_json::json!({
            "query": format!("find onboarding notes with {query_canary}"),
        }),
        workspace.path(),
        3,
    );
    assert_eq!(redacted_search.target.as_deref(), Some("[redacted query]"));
    let public = serde_json::to_string(&redacted_search).unwrap();
    assert!(!public.contains(query_canary));
    assert!(redacted_search
        .target
        .as_deref()
        .is_some_and(|target| target.len() <= 512));

    let outside = tempfile::tempdir().unwrap();
    let outside_cwd = semantic_tool_activity(
        "bash",
        &serde_json::json!({
            "command": "cargo test",
            "cwd": outside.path(),
        }),
        workspace.path(),
        4,
    );
    assert_eq!(outside_cwd.cwd, None);
    let remote_cwd = semantic_tool_activity(
        "bash",
        &serde_json::json!({
            "command": "cargo test",
            "cwd": "https://example.test/private",
        }),
        workspace.path(),
        5,
    );
    assert_eq!(remote_cwd.cwd, None);
}

#[tokio::test]
async fn live_tool_progress_is_count_only_and_never_forwards_status_or_output_text() {
    let workspace = tempfile::tempdir().unwrap();
    let run_id = RunId::new("run-progress-redaction").unwrap();
    let call_id = "call-progress-redaction";
    let item_id = stable_tool_item_id(call_id).unwrap();
    let mut projection = ProjectionState::new(0);
    projection
        .tool_items
        .insert(call_id.into(), item_id.clone());
    projection.tool_calls.insert(
        call_id.into(),
        projected_tool(
            workspace.path(),
            "bash",
            serde_json::json!({"command": "cargo test"}),
        ),
    );
    let (events, mut receiver) = mpsc::channel(4);
    let canary = "xoxb-LIVE-PROGRESS-CANARY-123456";
    project_tool_progress(
        ToolCallId(call_id.into()),
        ToolProgress::Output {
            stream: octet_agent::OutputStream::Stdout,
            bytes: bytes::Bytes::copy_from_slice(canary.as_bytes()),
        },
        &run_id,
        &mut projection,
        &events,
    )
    .await
    .unwrap();
    let output_event = receiver.recv().await.unwrap();
    let serialized = serde_json::to_string(&output_event.payload).unwrap();
    assert!(!serialized.contains(canary));
    assert!(matches!(
        output_event.payload,
        EventPayload::ItemDelta {
            item_id: actual_item_id,
            delta: ItemDelta::ToolActivity {
                activity: ToolActivity {
                    observed_output_bytes,
                    ..
                }
            }
        } if actual_item_id == item_id && observed_output_bytes == canary.len() as u64
    ));

    project_tool_progress(
        ToolCallId(call_id.into()),
        ToolProgress::Status("token=STATUS-CANARY-SECRET".into()),
        &run_id,
        &mut projection,
        &events,
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), receiver.recv())
            .await
            .is_err(),
        "raw status text unexpectedly produced a public event"
    );
}

#[test]
fn approval_progress_forwards_the_trusted_prompt_and_bounded_intent_detail() {
    let action = approval_action(
        "Approve exact workspace mutation?",
        Some("path: src/lib.rs\ncontent: bounded-preview\nintent SHA-256: digest-canary"),
    );

    assert!(action.contains("Approve exact workspace mutation?"));
    assert!(action.contains("path: src/lib.rs"));
    assert!(action.contains("content: bounded-preview"));
    assert!(action.contains("intent SHA-256: digest-canary"));
    assert!(!action.contains("Approve this tool action?"));
}

#[test]
fn completion_review_links_changes_verification_failures_warnings_and_outputs() {
    let workspace = tempfile::tempdir().unwrap();
    let mut projection = ProjectionState::new(0);
    let command_item = ItemId::new("item-review-command").unwrap();
    let edit_item = ItemId::new("item-review-edit").unwrap();

    let command_args = serde_json::json!({"command": "cargo test"});
    let command = semantic_tool_activity("bash", &command_args, workspace.path(), 10);
    let (command, mut command_result) = complete_tool_activity(
        command,
        "bash",
        &Ok(ToolOutput::new("exit=1 duration=0.20s\nstderr:\nfailed")),
        210,
        ProjectedToolProgress::default(),
    );
    command_result.tool_call_item_id = command_item.clone();
    projection
        .tool_items
        .insert("call-review-command".into(), command_item.clone());
    projection.tool_calls.insert(
        "call-review-command".into(),
        ProjectedToolCall {
            name: "bash".into(),
            arguments: command_args,
            activity: command,
            result: Some(command_result),
            turn_id: TurnId::new("turn-review-command").unwrap(),
        },
    );

    let edit_args = serde_json::json!({"path": "src/lib.rs"});
    let mut edit = semantic_tool_activity("edit", &edit_args, workspace.path(), 20);
    edit.status = ToolActivityStatus::Succeeded;
    edit.summary = Some("Completed".into());
    edit.completed_at_ms = Some(30);
    edit.duration_ms = Some(10);
    edit.output_summary = Some("File updated".into());
    edit.changed_paths = vec!["src/lib.rs".into()];
    projection
        .tool_items
        .insert("call-review-edit".into(), edit_item);
    projection.tool_calls.insert(
        "call-review-edit".into(),
        ProjectedToolCall {
            name: "edit".into(),
            arguments: edit_args,
            activity: edit,
            result: None,
            turn_id: TurnId::new("turn-review-edit").unwrap(),
        },
    );
    let terminal = TerminalProjection::completed();
    let changed_item = ItemId::new("item-review-change").unwrap();
    let output_id = ArtifactId::new("artifact-review").unwrap();
    let review = build_completion_review(
        &terminal,
        1,
        1_001,
        &projection,
        BTreeSet::from([changed_item.clone()]),
        BTreeSet::new(),
        BTreeSet::from([output_id.clone()]),
    );
    assert_eq!(review.duration_ms, 1_000);
    assert_eq!(review.action_count, 2);
    assert_eq!(review.changed_file_item_ids, vec![changed_item]);
    assert_eq!(
        review.verification_action_item_ids,
        vec![command_item.clone()]
    );
    assert_eq!(review.failed_action_item_ids, vec![command_item.clone()]);
    assert_eq!(review.warning_action_item_ids, vec![command_item]);
    assert_eq!(review.output_ids, vec![output_id]);
    assert_eq!(review.evidence_coverage, EvidenceCoverage::Partial);
    assert!(review
        .phases
        .iter()
        .any(|phase| phase.phase == ActivityPhase::Verified && phase.failed_count == 1));
    review.validate().unwrap();
}

#[test]
fn legacy_session_without_run_record_rehydrates_a_terminal_safe_tool_call() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("legacy-semantic.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("verify this".into())],
        })))
        .unwrap();
    let argument_canary = "sk-live-LEGACY-ARGUMENT-CANARY-123456";
    let output_canary = "ghp_LEGACYOUTPUTCANARY123456789";
    let arguments = serde_json::json!({"command": format!("cargo test token={argument_canary}")});
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(octet_ai::ToolCall {
                async_execution: false,
                id: ToolCallId("call-legacy-semantic".into()),
                name: "bash".into(),
                arguments_json: serde_json::to_string(&arguments).unwrap(),
                argument_error: None,
            })],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(octet_ai::ToolResult {
                tool_call_id: ToolCallId("call-legacy-semantic".into()),
                content: vec![ToolResultPart::Text(format!(
                    "exit=0 duration=0.05s\n{output_canary}"
                ))],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();

    let seed = seed_from_session(
        &session,
        SessionId::new("legacy-semantic").unwrap(),
        SessionSeedOptions {
            workspace: workspace.path(),
            project_id: None,
            model: ModelSelection {
                provider: "test".into(),
                model: "test-model".into(),
                reasoning: "off".into(),
            },
            authority: AuthorityProfile::FullAccess,
            generation: 1,
            meta: None,
            attachment_store: None,
            resource_store: None,
        },
    )
    .unwrap();
    let activity = seed
        .snapshot
        .items
        .iter()
        .find_map(|item| match &item.payload {
            ItemPayload::ToolCall(activity) => Some(activity),
            _ => None,
        })
        .unwrap();
    assert_eq!(activity.status, ToolActivityStatus::Succeeded);
    assert_eq!(
        activity.command_preview.as_deref(),
        Some("cargo test [redacted arguments]")
    );
    assert_eq!(activity.duration_ms, Some(50));
    let public = serde_json::to_string(&seed.snapshot).unwrap();
    for secret in [argument_canary, output_canary] {
        assert!(!public.contains(secret));
    }
    assert!(!public.contains("\"arguments\""));
}

#[test]
fn semantic_run_record_rehydrates_live_ids_timestamps_results_and_review_exactly() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("semantic-replay.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("verify this".into())],
        })))
        .unwrap();
    let arguments = serde_json::json!({"command": "cargo test"});
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(octet_ai::ToolCall {
                async_execution: false,
                id: ToolCallId("call-semantic-replay".into()),
                name: "bash".into(),
                arguments_json: serde_json::to_string(&arguments).unwrap(),
                argument_error: None,
            })],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    let raw_result = "exit=0 duration=0.25s\n(no output)";
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(octet_ai::ToolResult {
                tool_call_id: ToolCallId("call-semantic-replay".into()),
                content: vec![ToolResultPart::Text(raw_result.into())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();
    session
        .append_run_outcome(SessionRunOutcome {
            status: SessionRunOutcomeStatus::Completed,
            message: None,
        })
        .unwrap();

    let session_id = SessionId::new("semantic-replay").unwrap();
    let run_id = RunId::new("run-stable-semantic").unwrap();
    let turn_id = TurnId::new("turn-stable-semantic").unwrap();
    let tool_item_id = stable_tool_item_id("call-semantic-replay").unwrap();
    let mut projection = ProjectionState::new(0);
    projection.run_started_at_ms = 1_000;
    projection.pending_user_items.push_back(PendingUserItem {
        id: ItemId::new("item-stable-user").unwrap(),
        delivery: UserMessageDelivery::Submit,
        turn_id: TurnId::new("turn-stable-user").unwrap(),
        documents: Vec::new(),
        project_files: Vec::new(),
        document_context_tokens: 0,
        project_file_context_tokens: 0,
        context_attributed: true,
        branch_provenance: None,
    });
    projection
        .tool_items
        .insert("call-semantic-replay".into(), tool_item_id.clone());
    projection
        .item_turns
        .insert(tool_item_id.clone(), turn_id.clone());
    let activity = semantic_tool_activity("bash", &arguments, workspace.path(), 1_100);
    let (activity, mut result) = complete_tool_activity(
        activity,
        "bash",
        &Ok(ToolOutput::new(raw_result)),
        1_350,
        ProjectedToolProgress::default(),
    );
    result.tool_call_item_id = tool_item_id;
    projection.tool_calls.insert(
        "call-semantic-replay".into(),
        ProjectedToolCall {
            name: "bash".into(),
            arguments,
            activity,
            result: Some(result),
            turn_id,
        },
    );
    let review = build_completion_review(
        &TerminalProjection::completed(),
        1_000,
        1_500,
        &projection,
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    let live = project_new_entries(
        &session,
        workspace.path(),
        &mut projection,
        Some(&run_id),
        Some(&review),
        None,
        &session_id,
    )
    .unwrap();
    let resources = octet_serve_backend::ResourceStore::open(workspace.path()).unwrap();
    persist_run_projection(
        &resources,
        &session_id,
        &run_id,
        1_000,
        1_500,
        &projection,
        &live,
        &review,
    )
    .unwrap();
    drop(session);
    drop(resources);

    let reopened = Session::open_read_only(&path).unwrap();
    let resources = octet_serve_backend::ResourceStore::open(workspace.path()).unwrap();
    let seed = seed_from_session(
        &reopened,
        session_id,
        SessionSeedOptions {
            workspace: workspace.path(),
            project_id: None,
            model: ModelSelection {
                provider: "test".into(),
                model: "test-model".into(),
                reasoning: "off".into(),
            },
            authority: AuthorityProfile::FullAccess,
            generation: 99,
            meta: None,
            attachment_store: None,
            resource_store: Some(&resources),
        },
    )
    .unwrap();
    assert_eq!(seed.snapshot.items, live);
    let outcome = seed
        .snapshot
        .items
        .iter()
        .find_map(|item| match &item.payload {
            ItemPayload::RunOutcome { review, .. } => Some(review),
            _ => None,
        })
        .unwrap();
    assert_eq!(outcome, &review);
    assert_eq!(outcome.duration_ms, 500);
    assert_eq!(outcome.evidence_coverage, EvidenceCoverage::Partial);
}
