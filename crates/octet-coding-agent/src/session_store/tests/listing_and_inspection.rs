//! Listing and targeted inspection at scale: empty and config-only sessions are
//! omitted, the lightweight listing agrees with the active branch and ignores
//! large bodies, many session files are listed without quadratic behaviour, and
//! inspection defaults when the metadata directory is absent or validates only
//! the one session that was asked for.
//!
//! Separate from the lightweight-summary group because these tests compare the
//! list against an expectation, while that group compares the parser against
//! its own invariants.

use super::*;

#[test]
fn list_omits_empty_and_config_only_sessions() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let _empty = Session::create(store.dir().join("empty.jsonl")).unwrap();
    let mut config_only = Session::create(store.dir().join("config.jsonl")).unwrap();
    config_only
        .append(EntryValue::Config {
            model: Some("model".into()),
            reasoning: Some("high".into()),
            reasoning_mode: None,
        })
        .unwrap();

    assert!(store.list().is_empty());
}

#[test]
fn lightweight_listing_matches_the_active_branch_and_ignores_large_bodies() {
    use octet_ai::{
        AssistantMessage, AssistantPart, ModelId, Protocol, ToolCallId, ToolResult, ToolResultPart,
    };

    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("large.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append_with_metadata(
            EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text(
                    "model-only prompt text that must not title the session".into(),
                )],
            })),
            Some(octet_agent::EntryMetadata {
                display_text: Some("  title   with whitespace that the picker normalizes  ".into()),
                ..octet_agent::EntryMetadata::default()
            }),
        )
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("x".repeat(2 * 1024 * 1024))],
            model: ModelId("model".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: ToolCallId("call-1".into()),
                content: vec![ToolResultPart::Text("y".repeat(2 * 1024 * 1024))],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();
    let expected = active_branch_title(&session);
    drop(session);

    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, expected);
    assert_eq!(
        listed[0].title,
        "title with whitespace that the picker normalizes"
    );
}

#[test]
fn listing_scales_across_many_session_files() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let template_path = store.dir().join("session-0000.jsonl");
    let mut template = Session::create(&template_path).unwrap();
    template
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("scale fixture".into())],
        })))
        .unwrap();
    drop(template);
    let bytes = std::fs::read(&template_path).unwrap();
    for index in 1..512 {
        std::fs::write(
            store.dir().join(format!("session-{index:04}.jsonl")),
            &bytes,
        )
        .unwrap();
    }

    let listed = store.list();
    assert_eq!(listed.len(), 512);
    assert!(listed
        .iter()
        .all(|session| session.title == "scale fixture"));
}

#[test]
fn catalog_inspection_defaults_when_metadata_directory_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("unannotated.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("unannotated title".into())],
        })))
        .unwrap();
    drop(session);

    assert!(!store.metadata_dir().exists());
    assert_eq!(
        store.load_metadata("unannotated").unwrap(),
        SessionUserMetadata::default()
    );
    assert_eq!(
        store
            .inspect_by_id("unannotated")
            .unwrap()
            .catalog
            .meta
            .unwrap()
            .title,
        "unannotated title"
    );
    assert_eq!(
        store
            .catalog_by_id("unannotated")
            .unwrap()
            .meta
            .unwrap()
            .title,
        "unannotated title"
    );
}

#[test]
fn uncertainty_reopens_and_keeps_warm_and_cold_catalogs_visible() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("uncertain.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("recoverable title".into())],
        })))
        .unwrap();
    assert_eq!(store.list().len(), 1); // Warm the index before the additive record.
    let head = session.head();
    for _ in 0..2 {
        session
            .record_usage_uncertainty(
                EndpointId("openai".into()),
                ModelId("test-model".into()),
                "assistant_turn",
            )
            .unwrap();
    }
    drop(session);
    let bytes = std::fs::read(&path).unwrap();
    let reopened = Session::open_read_only(&path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert_eq!(reopened.head(), head);
    assert!(reopened.usage_records().is_empty());
    let inspection = store.inspect_by_id("uncertain").unwrap();
    assert_eq!(inspection.usage_uncertainty_records.len(), 2);
    assert!(inspection.usage_records.is_empty());
    assert_eq!(inspection.catalog.meta.unwrap().title, "recoverable title");
    assert_eq!(store.list()[0].title, "recoverable title");
    let cold = SessionStore::new(root.path(), workspace.path());
    assert_eq!(cold.list()[0].message_count, 1);
    assert_eq!(cold.list_all()[0].title, "recoverable title");
    assert!(cold.catalog_by_id("uncertain").unwrap().meta.is_some());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn targeted_catalog_inspection_validates_only_the_requested_session() {
    use octet_ai::{AssistantMessage, AssistantPart, Protocol, Usage, UserMessage};

    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let target = store.dir().join("target.jsonl");
    let mut session = Session::create(&target).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("target title".into())],
        })))
        .unwrap();
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("done".into())],
            model: ModelId("target-model".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    session
        .record_assistant_usage(
            assistant,
            EndpointId("target-endpoint".into()),
            ModelId("target-model".into()),
            Usage {
                input_tokens: 11,
                cache_read_tokens: 2,
                cache_write_tokens: 3,
                cache_write_1h_tokens: 4,
                output_tokens: 5,
                reasoning_tokens: 6,
                total_tokens: 31,
            },
            None,
        )
        .unwrap();
    session
        .append(EntryValue::Config {
            model: Some("target-config".into()),
            reasoning: Some("high".into()),
            reasoning_mode: None,
        })
        .unwrap();
    session
        .record_usage_uncertainty(
            EndpointId("target-endpoint".into()),
            ModelId("target-model".into()),
            "assistant_turn",
        )
        .unwrap();
    drop(session);
    store
        .set_lifecycle("target", SessionStorageLifecycle::Trash, 1_000)
        .unwrap();

    // A corrupt sibling must not affect a targeted operation.
    let corrupt = store.dir().join("corrupt.jsonl");
    drop(Session::create(&corrupt).unwrap());
    std::fs::write(&corrupt, b"{not valid json}\n").unwrap();

    let inspection = store.inspect_by_id("target").unwrap();
    assert_eq!(inspection.usage_uncertainty_records.len(), 1);
    let meta = inspection.catalog.meta.as_ref().unwrap();
    assert_eq!(meta.title, "target title");
    assert_eq!(meta.trashed_at_ms, Some(1_000));
    assert_eq!(
        inspection.catalog.configured_model.as_deref(),
        Some("target-config")
    );
    assert_eq!(
        inspection.catalog.configured_reasoning.as_deref(),
        Some("high")
    );
    assert_eq!(inspection.usage_records.len(), 1);
    assert_eq!(
        inspection.usage_records[0].endpoint.as_deref(),
        Some("target-endpoint")
    );
    assert_eq!(inspection.usage_records[0].total_tokens, 31);

    // Populate the catalog before the targeted Serve lookup. The lookup must
    // retain the persisted configuration without reopening the transcript.
    store.list();
    let catalogs = store.catalog_by_ids(["target", "corrupt"]).unwrap();
    assert_eq!(catalogs.len(), 1);
    assert_eq!(catalogs[0].0, "target");
    assert_eq!(catalogs[0].1.meta.as_ref().unwrap().title, "target title");
    assert_eq!(
        catalogs[0].1.configured_model.as_deref(),
        Some("target-config")
    );
    assert_eq!(catalogs[0].1.configured_reasoning.as_deref(), Some("high"));
    let catalog = store.catalog_by_id("target").unwrap();
    assert_eq!(catalog.meta.unwrap().title, "target title");
    assert_eq!(catalog.configured_model.as_deref(), Some("target-config"));
    assert_eq!(catalog.configured_reasoning.as_deref(), Some("high"));
    assert!(store.catalog_by_id("corrupt").is_err());
    assert!(store.get_by_id("corrupt").is_err());
}
