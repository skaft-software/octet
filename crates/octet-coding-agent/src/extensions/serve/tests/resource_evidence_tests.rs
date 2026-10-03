//! Turning a tool call into durable, workspace-relative evidence.
//! Attachments cross into the agent as native inline media, a read mints
//! path-free source evidence, an edit snapshots the file it actually changed,
//! and a two-resource batch rolls back completely when the second one cannot
//! stage. Only files this run created are promoted to artifacts.

use super::*;
use octet_ai::{AssistantMessage, Protocol, UserMessage};

use super::test_support::*;

#[test]
fn stored_attachments_cross_the_agent_boundary_as_native_inline_media() {
    let directory = tempfile::tempdir().unwrap();
    let store = AttachmentStore::open(directory.path()).unwrap();
    let image = png();
    let reference = store
        .ingest(
            "alignment.png",
            "image/png",
            bytes::Bytes::from(image.clone()),
        )
        .unwrap();

    let media = resolve_stored_media(true, Some(&store), &[reference]).unwrap();
    assert_eq!(media.len(), 1);
    let Media::Image(image_media) = &media[0] else {
        panic!("image attachment was not represented as image media");
    };
    assert_eq!(
        image_media.media_type.as_ref().map(mime::Mime::essence_str),
        Some("image/png")
    );
    assert!(matches!(&image_media.source, ImageSource::Inline(bytes) if bytes.as_ref() == image));
}

#[test]
fn unsupported_or_tampered_attachments_fail_before_agent_input_is_built() {
    let directory = tempfile::tempdir().unwrap();
    let store = AttachmentStore::open(directory.path()).unwrap();
    let reference = store
        .ingest("alignment.png", "image/png", bytes::Bytes::from(png()))
        .unwrap();

    assert_eq!(
        resolve_stored_media(false, Some(&store), std::slice::from_ref(&reference)).unwrap_err(),
        ServiceError::InvalidBoundary
    );
    let mut tampered = reference;
    tampered.byte_len += 1;
    assert_eq!(
        resolve_stored_media(true, Some(&store), &[tampered]).unwrap_err(),
        ServiceError::InvalidBoundary
    );
}

#[test]
fn successful_read_mints_openable_path_free_source_evidence() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("src")).unwrap();
    let content = b"pub fn octet() {}\n";
    std::fs::write(workspace.path().join("src/lib.rs"), content).unwrap();
    let registry = octet_serve_backend::ResourceStore::open(workspace.path()).unwrap();
    let session_id = SessionId::new("session-evidence").unwrap();
    let run_id = RunId::new("run-evidence").unwrap();
    let turn_id = TurnId::new("turn-evidence").unwrap();
    let tool_item_id = ItemId::new("item-tool-read").unwrap();
    let tool = projected_tool(
        workspace.path(),
        "read",
        serde_json::json!({"path": "src/lib.rs"}),
    );
    let output = format!(
        "src/lib.rs:1-1/1 hash={}\n1: pub fn octet() {{}}\ntruncated=false",
        stable_hash(content)
    );
    let session = session_with_successful_tool_result(
        &workspace.path().join("read-session.jsonl"),
        "call-read",
        "read",
        tool.arguments.clone(),
        &output,
    );

    let events = project_tool_evidence(
        &session,
        workspace.path(),
        &registry,
        &session_id,
        &run_id,
        &turn_id,
        "call-read",
        &tool_item_id,
        &tool,
        &ToolOutput::new(output),
    );
    assert_eq!(events.len(), 2);
    let EventPayload::SourceUpserted { source } = &events[0] else {
        panic!("first event was not source evidence");
    };
    assert_eq!(source.title, "src/lib.rs");
    assert_eq!(source.kind, SourceKind::File);
    assert_eq!(source.origin_item_id.as_ref(), Some(&tool_item_id));
    assert_eq!(
        registry.content(&session_id, &source.handle).unwrap().bytes,
        bytes::Bytes::from_static(b"pub fn octet() {}\n")
    );
    assert!(!source.handle.contains("src"));
}

#[test]
fn durable_evidence_rehydrates_only_on_the_active_branch() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("notes.txt"), b"durable evidence\n").unwrap();
    let session_path = workspace.path().join("branch-evidence.jsonl");
    let session_id = SessionId::new("branch-evidence").unwrap();
    let tool = projected_tool(
        workspace.path(),
        "read",
        serde_json::json!({"path": "notes.txt"}),
    );
    let output = format!(
        "notes.txt:1-1/1 hash={}\n1: durable evidence\ntruncated=false",
        stable_hash(b"durable evidence\n")
    );
    let mut session = session_with_successful_tool_result(
        &session_path,
        "call-branch-read",
        "read",
        tool.arguments.clone(),
        &output,
    );
    let call_entry = session.entries()[0].id.clone();
    let result_entry = session.entries()[1].id.clone();
    let store = octet_serve_backend::ResourceStore::open(workspace.path()).unwrap();
    let events = project_tool_evidence(
        &session,
        workspace.path(),
        &store,
        &session_id,
        &RunId::new("run-branch-evidence").unwrap(),
        &TurnId::new("turn-branch-evidence").unwrap(),
        "call-branch-read",
        &ItemId::new("item-call-branch-read").unwrap(),
        &tool,
        &ToolOutput::new(output),
    );
    assert_eq!(events.len(), 2);
    drop(store);

    let store = octet_serve_backend::ResourceStore::open(workspace.path()).unwrap();
    let seed_for = |session: &Session| {
        seed_from_session(
            session,
            session_id.clone(),
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
                resource_store: Some(&store),
            },
        )
        .unwrap()
    };
    assert_eq!(seed_for(&session).snapshot.sources.len(), 1);

    session.checkout(call_entry).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("alternate branch".into())],
        })))
        .unwrap();
    assert!(seed_for(&session).snapshot.sources.is_empty());

    session.checkout(result_entry).unwrap();
    let restored = seed_for(&session);
    assert_eq!(restored.snapshot.sources.len(), 1);
    assert_eq!(
        restored.snapshot.sources[0]
            .origin_item_id
            .as_ref()
            .map(ItemId::as_str),
        Some("item-call-branch-read")
    );
    assert!(restored
        .snapshot
        .items
        .iter()
        .any(|item| matches!(item.payload, ItemPayload::Source(_))));
}

#[test]
fn resource_projection_rejects_outside_workspace_and_snapshots_successful_edits() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let registry = octet_serve_backend::ResourceStore::open(workspace.path()).unwrap();
    let session_id = SessionId::new("session-evidence").unwrap();
    let run_id = RunId::new("run-evidence").unwrap();
    let turn_id = TurnId::new("turn-evidence").unwrap();
    let tool_item_id = ItemId::new("item-tool").unwrap();
    let outside_tool = projected_tool(
        workspace.path(),
        "read",
        serde_json::json!({"path": outside.path()}),
    );
    let outside_session = session_with_successful_tool_result(
        &workspace.path().join("outside-session.jsonl"),
        "call-outside",
        "read",
        outside_tool.arguments.clone(),
        "secret",
    );
    assert!(project_tool_evidence(
        &outside_session,
        workspace.path(),
        &registry,
        &session_id,
        &run_id,
        &turn_id,
        "call-outside",
        &tool_item_id,
        &outside_tool,
        &ToolOutput::new("secret"),
    )
    .is_empty());

    std::fs::write(workspace.path().join("notes.md"), b"after\nsecond\n").unwrap();
    let edit = projected_tool(
        workspace.path(),
        "edit",
        serde_json::json!({
            "path": "notes.md",
            "old": "before\n",
            "new": "after\nsecond\n"
        }),
    );
    let output = format!(
        "ok modified=1\nnotes.md  +2 -1 hash={}\n--- a/notes.md\n+++ b/notes.md\n@@ -1,1 +1,2 @@\n-before\n+after\n+second\n",
        stable_hash(b"after\nsecond\n")
    );
    let edit_session = session_with_successful_tool_result(
        &workspace.path().join("edit-session.jsonl"),
        "call-edit",
        "edit",
        edit.arguments.clone(),
        &output,
    );
    let events = project_tool_evidence(
        &edit_session,
        workspace.path(),
        &registry,
        &session_id,
        &run_id,
        &turn_id,
        "call-edit",
        &tool_item_id,
        &edit,
        &ToolOutput::new(output),
    );
    assert_eq!(events.len(), 1);
    let EventPayload::ItemCommitted { item } = &events[0] else {
        panic!("first edit event was not a file change");
    };
    let ItemPayload::FileChange(change) = &item.payload else {
        panic!("edit item was not a file change");
    };
    assert_eq!(change.display_path, "notes.md");
    assert_eq!(change.origin_item_id.as_ref(), Some(&tool_item_id));
    assert_eq!((change.additions, change.deletions), (2, 1));
    assert!(change.result_handle.is_some());
    assert_eq!(
        registry.content(&session_id, &change.handle).unwrap().bytes,
        bytes::Bytes::from_static(
            b"--- a/notes.md\n+++ b/notes.md\n@@ -1,1 +1,2 @@\n-before\n+after\n+second\n"
        )
    );
    assert_eq!(
        registry
            .content(&session_id, change.result_handle.as_deref().unwrap())
            .unwrap()
            .bytes,
        bytes::Bytes::from_static(b"after\nsecond\n")
    );
}

#[test]
fn evidence_projection_rolls_back_when_the_second_resource_cannot_stage() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("empty.txt"), b"").unwrap();
    let store = octet_serve_backend::ResourceStore::open(workspace.path()).unwrap();
    let session_id = SessionId::new("session-partial-evidence").unwrap();
    let tool = projected_tool(
        workspace.path(),
        "write",
        serde_json::json!({"path": "empty.txt", "content": ""}),
    );
    let output = format!(
        "ok\nempty.txt  created hash={}\n--- /dev/null\n+++ b/empty.txt\n@@ -0,0 +1,0 @@\n",
        stable_hash(b"")
    );
    let session = session_with_successful_tool_result(
        &workspace.path().join("partial-evidence.jsonl"),
        "call-partial-write",
        "write",
        tool.arguments.clone(),
        &output,
    );

    assert!(project_tool_evidence(
        &session,
        workspace.path(),
        &store,
        &session_id,
        &RunId::new("run-partial-evidence").unwrap(),
        &TurnId::new("turn-partial-evidence").unwrap(),
        "call-partial-write",
        &ItemId::new("item-partial-evidence").unwrap(),
        &tool,
        &ToolOutput::new(output),
    )
    .is_empty());

    let replacement = store
        .register(
            &session_id,
            "call-partial-write",
            "diff",
            "replacement.diff",
            "text/plain",
            bytes::Bytes::from_static(b"rollback freed this binding"),
        )
        .unwrap();
    assert_eq!(
        store
            .content(&session_id, &replacement.handle)
            .unwrap()
            .bytes,
        bytes::Bytes::from_static(b"rollback freed this binding")
    );
}

#[test]
fn only_created_deliverables_are_promoted_to_artifacts() {
    let workspace = tempfile::tempdir().unwrap();
    let store = octet_serve_backend::ResourceStore::open(workspace.path()).unwrap();
    let session_id = SessionId::new("session-artifact-semantics").unwrap();
    let run_id = RunId::new("run-artifact-semantics").unwrap();
    let turn_id = TurnId::new("turn-artifact-semantics").unwrap();
    let tool_item_id = ItemId::new("item-tool-write").unwrap();

    std::fs::write(workspace.path().join("report.md"), b"# Report\n").unwrap();
    let created = projected_tool(
        workspace.path(),
        "write",
        serde_json::json!({"path": "report.md", "content": "# Report\n"}),
    );
    let created_output = format!(
        "ok\nreport.md  created hash={}\n--- /dev/null\n+++ b/report.md\n@@ -0,0 +1,1 @@\n+# Report\n",
        stable_hash(b"# Report\n")
    );
    let mut created_session = session_with_successful_tool_result(
        &workspace.path().join("created-artifact.jsonl"),
        "call-write-created",
        "write",
        created.arguments.clone(),
        &created_output,
    );
    let created_events = project_tool_evidence(
        &created_session,
        workspace.path(),
        &store,
        &session_id,
        &run_id,
        &turn_id,
        "call-write-created",
        &tool_item_id,
        &created,
        &ToolOutput::new(created_output),
    );
    assert!(created_events
        .iter()
        .any(|event| matches!(event, EventPayload::ArtifactUpserted { artifact } if artifact.kind == ArtifactKind::Document)));
    assert!(created_events.iter().any(|event| {
        matches!(
            event,
            EventPayload::ArtifactUpserted { artifact }
                if artifact.origin_item_id.as_ref() == Some(&tool_item_id)
        )
    }));

    std::fs::write(workspace.path().join("report.md"), b"# Revised\n").unwrap();
    let replaced = projected_tool(
        workspace.path(),
        "write",
        serde_json::json!({"path": "report.md", "content": "# Revised\n"}),
    );
    let replaced_output = format!(
        "ok\nreport.md  replaced hash={}\n--- a/report.md\n+++ b/report.md\n@@ -1,1 +1,1 @@\n-# Report\n+# Revised\n",
        stable_hash(b"# Revised\n")
    );
    created_session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(octet_ai::ToolCall {
                async_execution: false,
                id: ToolCallId("call-write-replaced".into()),
                name: "write".into(),
                arguments_json: serde_json::to_string(&replaced.arguments).unwrap(),
                argument_error: None,
            })],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    created_session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(octet_ai::ToolResult {
                tool_call_id: ToolCallId("call-write-replaced".into()),
                content: vec![ToolResultPart::Text(replaced_output.clone())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();
    let replaced_events = project_tool_evidence(
        &created_session,
        workspace.path(),
        &store,
        &session_id,
        &run_id,
        &turn_id,
        "call-write-replaced",
        &tool_item_id,
        &replaced,
        &ToolOutput::new(replaced_output),
    );
    assert!(replaced_events
        .iter()
        .all(|event| !matches!(event, EventPayload::ArtifactUpserted { .. })));
    assert!(replaced_events.iter().any(|event| {
        matches!(
            event,
            EventPayload::ItemCommitted {
                item: SessionItem {
                    payload: ItemPayload::FileChange(_),
                    ..
                }
            }
        )
    }));
}
