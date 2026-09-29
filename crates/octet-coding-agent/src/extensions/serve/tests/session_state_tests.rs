//! Per-session state that must survive a restart and replay identically.
//! Prompt-title changes, the pre-prompt selection, the branch head, the exported
//! transcript, metadata mutations, attachment references, and prompt
//! attribution are all written once and read back later; a mismatch between the
//! live delivery and the durable replay is the failure each case hunts.

use super::*;
use octet_agent::EntryMetadata;
use octet_ai::{AssistantMessage, Protocol, UserMessage};
use std::sync::atomic::AtomicBool;

use super::test_support::*;

#[test]
fn durable_prompt_title_changes_are_published_once() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let session_dir = directory.path().join("sessions");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();
    let store = SessionStore::new(&session_dir, &workspace);
    let session_id = SessionId::new("title-change").unwrap();
    std::fs::create_dir_all(store.dir()).unwrap();
    let mut session = Session::create(store.dir().join("title-change.jsonl")).unwrap();

    assert!(session_meta_for_id(&store, &session_id).is_none());
    assert_eq!(changed_session_title(&store, &session_id, None), None);

    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(
                "  Keep   the new session title stable  ".into(),
            )],
        })))
        .unwrap();

    let changed = changed_session_title(&store, &session_id, None).unwrap();
    assert_eq!(changed, "Keep the new session title stable");
    assert_eq!(
        changed_session_title(&store, &session_id, Some(&changed)),
        None
    );
}

#[test]
fn pre_prompt_selection_is_durable_across_session_restart() {
    let directory = tempfile::tempdir().unwrap();
    let session_dir = directory.path().join("sessions");
    std::fs::create_dir(&session_dir).unwrap();
    let session_path = session_dir.join("selection.jsonl");
    let session_id = SessionId::new("selection").unwrap();
    let mut plan = WorkerPlan {
        config: serve_test_config(directory.path()),
        sessions: SessionStore::new(&session_dir, directory.path()),
        launch: LaunchSelection {
            model: ModelId("selected-model".into()),
            session: SessionSelection::CreateNew(session_path.clone()),
            reasoning: ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
            reasoning_mode: octet_ai::ReasoningMode::Standard,
        },
        prepared_session: Mutex::new(None),
        authority: AuthorityProfile::FullAccess,
        available_models: Vec::new(),
        actor_generation: 1,
        session_id,
        project_id: None,
        attachments: None,
        documents: None,
        projects: Arc::new(Mutex::new(
            ProjectRegistry::open(directory.path().join("selection-projects")).unwrap(),
        )),
        trusted_files: Arc::new(Mutex::new(HashMap::new())),
        search_index: Arc::new(Mutex::new(TranscriptSearchIndex::new())),
        resources: None,
        goal_store: None,
        usage: Arc::new(Mutex::new(
            InferenceRequestStore::open(directory.path()).unwrap(),
        )),
        pull_requests: Arc::new(Mutex::new(
            PullRequestStore::open(&directory.path().join("selection-pull-requests")).unwrap(),
        )),
        pull_request_projection: Arc::new(Mutex::new(None)),
        pull_request_discovery_enabled: Arc::new(AtomicBool::new(false)),
        pull_request_refresh_requested: Arc::new(tokio::sync::Notify::new()),
        checkout_hooks: CheckoutTestHooks::default(),
    };
    let mut projection = ProjectionState::new(0);

    let first = persist_idle_selection(
        &mut plan,
        &mut projection,
        ModelSelection {
            provider: "test".into(),
            model: "selected-model".into(),
            reasoning: "high".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        first.events.as_slice(),
        [
            TimestampedEvent {
                payload: EventPayload::SessionSettingsChanged {
                    model,
                    authority: AuthorityProfile::FullAccess,
                },
                ..
            },
            TimestampedEvent {
                payload: EventPayload::SessionBranchEntriesAppended { entries },
                ..
            },
            TimestampedEvent {
                payload: EventPayload::SessionDurableHeadChanged {
                    durable_entry_id: Some(head),
                },
                ..
            },
        ] if model.provider == "test"
            && model.model == "selected-model"
            && model.reasoning == "high"
            && entries.len() == 1
            && entries[0].kind == SessionBranchEntryKind::Internal
            && !entries[0].checkoutable
            && entries[0].entry_id == *head
    ));
    assert!(matches!(
        plan.launch.session,
        SessionSelection::OpenExisting(ref path) if path == &session_path
    ));

    plan.launch.reasoning = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low);
    persist_idle_selection(
        &mut plan,
        &mut projection,
        ModelSelection {
            provider: "test".into(),
            model: "selected-model".into(),
            reasoning: "low".into(),
        },
    )
    .unwrap();
    drop(plan);

    let reopened = Session::open_read_only(&session_path).unwrap();
    let latest = reopened.entries().last().expect("persisted config");
    assert!(matches!(
        &latest.value,
        EntryValue::Config {
            model: Some(model),
            reasoning: Some(reasoning),
            reasoning_mode: Some(reasoning_mode),
        } if model == "selected-model"
            && reasoning == "low"
            && reasoning_mode == "standard"
    ));
}

#[test]
fn branch_projection_is_bounded_and_always_preserves_the_selected_head() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded-branches.jsonl");
    let mut session = Session::create(&path).unwrap();
    let mut first = None;
    for index in 0..(MAX_PROJECTED_BRANCH_ENTRIES + 2) {
        let entry = session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text(format!("message {index}"))],
            })))
            .unwrap();
        first.get_or_insert(entry);
    }
    let first = first.unwrap();
    session.checkout(first.clone()).unwrap();

    let graph = branch_graph(&session).unwrap();
    assert!(graph.truncated);
    assert_eq!(graph.entries.len(), MAX_PROJECTED_BRANCH_ENTRIES);
    assert_eq!(
        graph.head,
        Some(DurableEntryId::new(first.0.clone()).unwrap())
    );
    assert!(graph
        .entries
        .iter()
        .any(|entry| entry.entry_id.as_str() == first.0));
    graph.validate().unwrap();
}

#[test]
fn graphical_session_export_is_redacted_bounded_and_cleans_temporary_files() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let session_dir = directory.path().join("sessions");
    let sessions = SessionStore::new(&session_dir, workspace.path());
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_path = sessions.dir().join("safe-export.jsonl");
    let mut session = Session::create(&session_path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("sk-1234567890123456".into())],
        })))
        .unwrap();
    drop(session);
    let session_id = SessionId::new("safe-export").unwrap();
    let serve_state_dir = directory.path().join("serve-state");
    std::fs::create_dir(&serve_state_dir).unwrap();

    let exported = export_session_bytes(
        &sessions,
        &session_id,
        &serve_state_dir,
        MAX_GRAPHICAL_SESSION_EXPORT_BYTES,
    )
    .unwrap();
    let exported: serde_json::Value = serde_json::from_slice(&exported).unwrap();
    assert_eq!(exported["format"], "octet-session-export");
    assert_eq!(exported["redacted"], true);
    let serialized = exported.to_string();
    assert!(serialized.contains("[REDACTED]"));
    assert!(!serialized.contains("sk-1234567890123456"));
    assert_eq!(std::fs::read_dir(&serve_state_dir).unwrap().count(), 0);

    assert_eq!(
        export_session_bytes(&sessions, &session_id, &serve_state_dir, 16),
        Err(ServiceError::PayloadTooLarge)
    );
    assert_eq!(std::fs::read_dir(&serve_state_dir).unwrap().count(), 0);
    assert_eq!(
        export_session_bytes(
            &sessions,
            &SessionId::new("missing-export").unwrap(),
            &serve_state_dir,
            MAX_GRAPHICAL_SESSION_EXPORT_BYTES,
        ),
        Err(ServiceError::NotFound)
    );
    assert_eq!(std::fs::read_dir(&serve_state_dir).unwrap().count(), 0);
}

#[test]
fn session_metadata_mutations_are_durable_and_emit_exact_patches() {
    let directory = tempfile::tempdir().unwrap();
    let config = serve_test_config(directory.path());
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_path = sessions.dir().join("metadata-session.jsonl");
    let mut session = Session::create(&session_path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("original title".into())],
        })))
        .unwrap();
    drop(session);
    let plan = WorkerPlan {
        config,
        sessions: sessions.clone(),
        launch: LaunchSelection {
            model: ModelId("test-model".into()),
            session: SessionSelection::OpenExisting(session_path),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
        },
        prepared_session: Mutex::new(None),
        authority: AuthorityProfile::FullAccess,
        available_models: Vec::new(),
        actor_generation: 1,
        session_id: SessionId::new("metadata-session").unwrap(),
        project_id: None,
        attachments: None,
        documents: None,
        projects: Arc::new(Mutex::new(
            ProjectRegistry::open(directory.path().join("metadata-projects")).unwrap(),
        )),
        trusted_files: Arc::new(Mutex::new(HashMap::new())),
        search_index: Arc::new(Mutex::new(TranscriptSearchIndex::new())),
        resources: None,
        goal_store: None,
        usage: Arc::new(Mutex::new(
            InferenceRequestStore::open(directory.path()).unwrap(),
        )),
        pull_requests: Arc::new(Mutex::new(
            PullRequestStore::open(&directory.path().join("metadata-pull-requests")).unwrap(),
        )),
        pull_request_projection: Arc::new(Mutex::new(None)),
        pull_request_discovery_enabled: Arc::new(AtomicBool::new(false)),
        pull_request_refresh_requested: Arc::new(tokio::sync::Notify::new()),
        checkout_hooks: CheckoutTestHooks::default(),
    };

    let renamed = rename_session_outcome(&plan, "  Renamed session  ").unwrap();
    assert!(matches!(
        renamed.events.as_slice(),
        [TimestampedEvent {
            payload: EventPayload::SessionMetadataChanged {
                title: Some(title),
                pinned: None,
                archived: None,
            },
            ..
        }] if title == "Renamed session"
    ));
    let pinned = pin_session_outcome(&plan, true).unwrap();
    assert!(matches!(
        pinned.events.as_slice(),
        [TimestampedEvent {
            payload: EventPayload::SessionMetadataChanged {
                title: None,
                pinned: Some(true),
                archived: None,
            },
            ..
        }]
    ));
    let archived = archive_session_outcome(&plan, true).unwrap();
    assert!(matches!(
        archived.events.as_slice(),
        [TimestampedEvent {
            payload: EventPayload::SessionMetadataChanged {
                title: None,
                pinned: None,
                archived: Some(true),
            },
            ..
        }]
    ));

    let reopened = SessionStore::new(&plan.config.session_dir, &plan.config.workspace);
    let metadata = reopened.load_metadata("metadata-session").unwrap();
    assert_eq!(metadata.name.as_deref(), Some("Renamed session"));
    assert!(metadata.pinned);
    assert!(metadata.archived);
    let summary = summary_from_meta(
        &session_meta_for_id(&reopened, &plan.session_id).unwrap(),
        None,
        current_selection(&plan),
    )
    .unwrap();
    assert_eq!(summary.title, "Renamed session");
    assert!(summary.pinned);
    assert!(summary.archived);
}

#[test]
fn durable_projection_recovers_attachment_refs_from_native_media() {
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
    let entry = Entry {
        id: octet_agent::EntryId("entry-1".into()),
        parent: None,
        metadata: None,
        timestamp_unix_ms: None,
        value: EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Media(Media::image_bytes(
                bytes::Bytes::from(image),
                "image/png".parse().unwrap(),
            ))],
        })),
    };
    let session_id = SessionId::new("session-1").unwrap();
    let mut pending = VecDeque::from([vec![reference.clone()]]);

    let associated =
        attachment_refs_for_entry(&entry, Some(&store), &session_id, &mut pending).unwrap();
    assert_eq!(associated, vec![reference.clone()]);
    assert!(pending.is_empty());

    let mut after_restart = VecDeque::new();
    let restored =
        attachment_refs_for_entry(&entry, Some(&store), &session_id, &mut after_restart).unwrap();
    assert_eq!(restored, vec![reference]);
}

#[test]
fn steered_prompt_attribution_is_exact_live_and_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steer-attribution.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append_with_metadata(
            EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("composed original context".into())],
            })),
            Some(EntryMetadata {
                display_text: Some("original prompt".into()),
                ..EntryMetadata::default()
            }),
        )
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("working".into())],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    session
        .append_with_metadata(
            EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("steer exact text".into())],
            })),
            Some(EntryMetadata {
                prompt_model: Some(ModelId("test-model".into())),
                ..EntryMetadata::default()
            }),
        )
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("done".into())],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();

    let session_id = SessionId::new("steer-attribution").unwrap();
    let mut projection = ProjectionState::new(0);
    projection.pending_user_items.push_back(PendingUserItem {
        id: ItemId::new("live-original").unwrap(),
        delivery: UserMessageDelivery::Submit,
        turn_id: TurnId::new("turn-live-original").unwrap(),
        documents: Vec::new(),
        project_files: Vec::new(),
        document_context_tokens: 0,
        project_file_context_tokens: 0,
        context_attributed: true,
        branch_provenance: None,
    });
    projection.pending_user_items.push_back(PendingUserItem {
        id: ItemId::new("live-steer").unwrap(),
        delivery: UserMessageDelivery::Steer,
        turn_id: TurnId::new("turn-live-steer").unwrap(),
        documents: Vec::new(),
        project_files: Vec::new(),
        document_context_tokens: 0,
        project_file_context_tokens: 0,
        context_attributed: false,
        branch_provenance: None,
    });
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
    let live_users = live
        .iter()
        .filter_map(|item| match &item.payload {
            ItemPayload::UserMessage { text, delivery, .. } => {
                Some((item.id.as_str(), text.as_str(), *delivery))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        live_users,
        vec![
            (
                "live-original",
                "original prompt",
                Some(UserMessageDelivery::Submit)
            ),
            (
                "live-steer",
                "steer exact text",
                Some(UserMessageDelivery::Steer)
            )
        ]
    );
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
    let replayed_users = seed
        .snapshot
        .items
        .iter()
        .filter_map(|item| match &item.payload {
            ItemPayload::UserMessage { text, delivery, .. } => {
                assert_eq!(*delivery, None);
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(replayed_users, vec!["original prompt", "steer exact text"]);
}

#[tokio::test]
async fn accepted_controls_have_exact_live_delivery_and_identity() {
    let run_id = RunId::new("run-1-1").unwrap();
    let mut projection = ProjectionState::new(0);
    projection.begin_run();
    let (sender, mut receiver) = mpsc::channel(2);

    for (text, delivery) in [
        ("steer exact text", UserMessageDelivery::Steer),
        ("follow up exact text", UserMessageDelivery::FollowUp),
    ] {
        publish_control_user_item(
            &run_id,
            ResolvedPromptInput {
                display_text: text.into(),
                model_text: text.into(),
                attachments: Vec::new(),
                documents: Vec::new(),
                project_files: Vec::new(),
                document_context_tokens: 0,
                project_file_context_tokens: 0,
            },
            delivery,
            &mut projection,
            &sender,
        )
        .await
        .unwrap();
    }

    for (index, (text, delivery)) in [
        ("steer exact text", UserMessageDelivery::Steer),
        ("follow up exact text", UserMessageDelivery::FollowUp),
    ]
    .into_iter()
    .enumerate()
    {
        let started = receiver.recv().await.expect("live control event");
        let EventPayload::ItemStarted { item } = started.payload else {
            panic!("accepted control did not start a visible item");
        };
        assert_eq!(
            item.id.as_str(),
            format!("item-run-1-1-user-1-{}", index + 1)
        );
        assert!(matches!(
            item.payload,
            ItemPayload::UserMessage {
                text: ref actual,
                ref attachments,
                delivery: Some(actual_delivery),
                ..
            } if actual == text
                && attachments.is_empty()
                && actual_delivery == delivery
        ));
    }
    assert_eq!(
        projection
            .pending_user_items
            .iter()
            .map(|pending| pending.delivery)
            .collect::<Vec<_>>(),
        [UserMessageDelivery::Steer, UserMessageDelivery::FollowUp]
    );
}
