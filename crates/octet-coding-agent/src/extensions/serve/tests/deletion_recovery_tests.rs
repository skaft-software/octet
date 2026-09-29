//! Permanent session deletion and crash recovery around it.
//! Deletion is the one operation in this host that destroys a session's source,
//! so it owns a committed journal, refuses to run when a required store is
//! unavailable, reclaims every sidecar, and finishes an interrupted deletion on
//! the next startup without ever downgrading a committed record.

use super::*;
use octet_ai::{AssistantMessage, Protocol, UserMessage};

use super::test_support::*;

#[test]
fn terminal_capability_tracks_process_execution_permission() {
    let directory = tempfile::tempdir().unwrap();
    let config = project_test_config(directory.path(), true);
    let enabled = OctetHost::new(config.clone()).unwrap();
    assert!(enabled.capabilities().terminal);
    drop(enabled);

    let mut restricted = config;
    restricted.sandbox.allow_process = false;
    let disabled = OctetHost::new(restricted).unwrap();
    assert!(!disabled.capabilities().terminal);
}

#[tokio::test]
async fn permanent_delete_fails_closed_before_commit_when_a_required_store_is_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("unavailable-delete-store").unwrap();
    let mut session =
        Session::create(sessions.dir().join("unavailable-delete-store.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("retain on unavailable delete".into())],
        })))
        .unwrap();
    drop(session);
    sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 41_000)
        .unwrap();
    let mut host = OctetHost::new(config).unwrap();
    host.attachments = None;

    let error = host
        .delete_session_permanently(
            &session_id,
            &PermanentDeleteConfirmation {
                session_id: session_id.clone(),
                trashed_at_ms: 41_000,
                phrase: format!("permanently delete {}", session_id.as_str()),
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, ServiceError::Unavailable),
        "unexpected permanent-delete error: {error:?}"
    );
    assert!(sessions.path_by_id(session_id.as_str()).is_ok());
    assert_eq!(
        sessions
            .load_metadata(session_id.as_str())
            .unwrap()
            .trashed_at_ms,
        Some(41_000)
    );
    assert!(load_pending_session_deletions(&host.serve_state_dir)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn permanent_delete_requires_accounting_recovery_before_removing_source() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("accounting-delete-failure").unwrap();
    let path = sessions.dir().join("accounting-delete-failure.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("retain accounting source".into())],
        })))
        .unwrap();
    let host = OctetHost::new(config.clone()).unwrap();
    // New evidence since startup has not yet reached the host ledger.
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("openai".into()),
            ModelId("gpt-4o-mini".into()),
            "assistant_turn",
        )
        .unwrap();
    drop(session);
    sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 42_000)
        .unwrap();
    let confirmation = PermanentDeleteConfirmation {
        session_id: session_id.clone(),
        trashed_at_ms: 42_000,
        phrase: format!("permanently delete {}", session_id.as_str()),
    };
    assert!(host.usage.lock().unwrap().record_uncertainty("").is_err());
    assert!(matches!(
        host.delete_session_permanently(&session_id, &confirmation)
            .await,
        Err(ServiceError::Unavailable)
    ));
    assert!(sessions.session_file_exists(session_id.as_str()).unwrap());
    assert!(load_pending_session_deletions(&host.serve_state_dir)
        .unwrap()
        .is_empty());
    assert!(host.usage_lifetime().await.unwrap().usage_uncertain);
    assert!(
        host.usage_stats(UsagePeriod::Daily)
            .await
            .unwrap()
            .usage_uncertain
    );
    assert!(host.usage_activity().await.unwrap().usage_uncertain);
    drop(host);
    let recovered = OctetHost::new(config).unwrap();
    assert!(recovered.usage.lock().unwrap().ensure_available().is_ok());
    recovered
        .delete_session_permanently(&session_id, &confirmation)
        .await
        .unwrap();
    assert!(!sessions.session_file_exists(session_id.as_str()).unwrap());
    let accounting = InferenceRequestStore::open(&recovered.serve_state_dir).unwrap();
    assert!(accounting.lifetime().usage_uncertain);
    assert_eq!(accounting.lifetime().request_count, 0);
}

#[tokio::test]
async fn permanent_delete_syncs_accounting_added_after_startup() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("accounting-delete-late").unwrap();
    let mut session = Session::create(sessions.dir().join("accounting-delete-late.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("late accounting".into())],
        })))
        .unwrap();
    let host = OctetHost::new(config).unwrap();
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("done".into())],
            model: ModelId("gpt-4o-mini".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    session
        .record_assistant_usage(
            assistant,
            octet_ai::EndpointId("openai".into()),
            ModelId("gpt-4o-mini".into()),
            octet_ai::Usage {
                input_tokens: 80,
                output_tokens: 20,
                total_tokens: 100,
                ..octet_ai::Usage::default()
            },
            None,
        )
        .unwrap();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("openai".into()),
            ModelId("gpt-4o-mini".into()),
            "assistant_turn",
        )
        .unwrap();
    drop(session);
    assert_eq!(host.usage_lifetime().await.unwrap().request_count, 0);
    assert!(!host.usage_lifetime().await.unwrap().usage_uncertain);
    sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 43_000)
        .unwrap();
    host.delete_session_permanently(
        &session_id,
        &PermanentDeleteConfirmation {
            session_id: session_id.clone(),
            trashed_at_ms: 43_000,
            phrase: format!("permanently delete {}", session_id.as_str()),
        },
    )
    .await
    .unwrap();
    assert!(!sessions.session_file_exists(session_id.as_str()).unwrap());
    let accounting = InferenceRequestStore::open(&host.serve_state_dir).unwrap();
    assert!(accounting.lifetime().usage_uncertain);
    assert_eq!(accounting.lifetime().request_count, 1);
    assert_eq!(accounting.lifetime().total_tokens, 100);
}

#[tokio::test]
async fn permanent_delete_reclaims_all_session_sidecars_and_journal() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("permanent-sidecars").unwrap();
    let mut session = Session::create(sessions.dir().join("permanent-sidecars.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("delete everything".into())],
        })))
        .unwrap();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("local".into()),
            ModelId("test-model".into()),
            "assistant_turn",
        )
        .unwrap();
    drop(session);
    sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 42_000)
        .unwrap();

    let host = OctetHost::new(config.clone()).unwrap();
    let project_id = host.launch_project_id.clone();
    host.usage
        .lock()
        .unwrap()
        .record(InferenceRequest {
            session_id: session_id.as_str().to_owned(),
            request_ordinal: 0,
            provider: "local".into(),
            model: "test-model".into(),
            timestamp_ms: 42_001,
            prompt_tokens: 10,
            completion_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_write_1h_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 15,
        })
        .unwrap();
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    png.extend_from_slice(&[0, 0, 0, 0]);
    png.extend_from_slice(&0u32.to_be_bytes());
    png.extend_from_slice(b"IEND");
    png.extend_from_slice(&[0, 0, 0, 0]);
    let attachment = host
        .attachments
        .as_ref()
        .unwrap()
        .ingest("remove.png", "image/png", bytes::Bytes::from(png))
        .unwrap();
    host.attachments
        .as_ref()
        .unwrap()
        .associate(
            &session_id,
            "attachment-entry",
            std::slice::from_ref(&attachment),
        )
        .unwrap();
    let document = host
        .documents
        .as_ref()
        .unwrap()
        .ingest(
            project_id.as_str(),
            session_id.as_str(),
            "remove.txt",
            "text/plain",
            bytes::Bytes::from_static(b"remove document"),
        )
        .unwrap();
    let resource_entry = DurableEntryId::new("resource-entry").unwrap();
    let run_entry = DurableEntryId::new("run-entry").unwrap();
    let resource = host
        .resources
        .as_ref()
        .unwrap()
        .register(
            &session_id,
            "resource-call",
            "source",
            "remove.rs",
            "text/plain",
            bytes::Bytes::from_static(b"remove resource"),
        )
        .unwrap();
    host.resources
        .as_ref()
        .unwrap()
        .persist_record(
            &session_id,
            &resource_entry,
            "resource-call",
            br#"{"version":1}"#,
        )
        .unwrap();
    host.resources
        .as_ref()
        .unwrap()
        .persist_run_record(&session_id, &run_entry, br#"{"version":1}"#)
        .unwrap();
    host.goals
        .set(&session_id, "Remove the session goal", None)
        .unwrap();
    host.pull_requests
        .lock()
        .unwrap()
        .replace(
            &session_id,
            Some(stored_pull_request(
                &session_id,
                124,
                PullRequestState::Ready,
            )),
        )
        .unwrap();

    host.delete_session_permanently(
        &session_id,
        &PermanentDeleteConfirmation {
            session_id: session_id.clone(),
            trashed_at_ms: 42_000,
            phrase: format!("permanently delete {}", session_id.as_str()),
        },
    )
    .await
    .unwrap();

    assert!(sessions.path_by_id(session_id.as_str()).is_err());
    assert!(host
        .projects
        .lock()
        .unwrap()
        .project_for_session(session_id.as_str())
        .is_none());
    assert_eq!(
        host.attachments
            .as_ref()
            .unwrap()
            .refs_for_entry(&session_id, "attachment-entry")
            .unwrap(),
        None
    );
    assert!(host
        .documents
        .as_ref()
        .unwrap()
        .list_for_session(project_id.as_str(), session_id.as_str())
        .unwrap()
        .is_empty());
    assert_eq!(
        host.documents.as_ref().unwrap().get_for_session(
            project_id.as_str(),
            session_id.as_str(),
            &document.id,
        ),
        Err(DocumentStoreError::NotFound)
    );
    assert!(host
        .resources
        .as_ref()
        .unwrap()
        .content(&session_id, &resource.handle)
        .is_err());
    assert!(host
        .resources
        .as_ref()
        .unwrap()
        .run_record(&session_id, &run_entry)
        .is_err());
    assert_eq!(host.goals.get(&session_id).unwrap(), None);
    assert_eq!(
        host.pull_requests.lock().unwrap().summary(&session_id),
        None
    );
    assert!(host.usage.lock().unwrap().lifetime().usage_uncertain);
    assert_eq!(host.usage.lock().unwrap().lifetime().request_count, 1);
    assert!(load_pending_session_deletions(&host.serve_state_dir)
        .unwrap()
        .is_empty());

    drop(host);
    let reopened = OctetHost::new(config).unwrap();
    assert!(reopened.usage_lifetime().await.unwrap().usage_uncertain);
    assert_eq!(reopened.usage_lifetime().await.unwrap().total_tokens, 15);
    assert_eq!(reopened.usage.lock().unwrap().lifetime().request_count, 1);
    assert_eq!(
        reopened.pull_requests.lock().unwrap().summary(&session_id),
        None
    );
    assert!(reopened
        .documents
        .as_ref()
        .unwrap()
        .list_for_session(project_id.as_str(), session_id.as_str())
        .unwrap()
        .is_empty());
    assert!(load_pending_session_deletions(&reopened.serve_state_dir)
        .unwrap()
        .is_empty());
}

#[test]
fn committed_deletion_journal_cannot_be_downgraded_or_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let session_id = SessionId::new("journal-monotonic").unwrap();
    let project_id = ProjectId::new("project-monotonic").unwrap();
    let mut deletion = PendingSessionDeletion::new(&session_id, &project_id, 75_000);
    write_pending_session_deletion(directory.path(), &deletion).unwrap();
    deletion.committed = true;
    write_pending_session_deletion(directory.path(), &deletion).unwrap();

    let mut downgrade = deletion.clone();
    downgrade.committed = false;
    assert!(write_pending_session_deletion(directory.path(), &downgrade).is_err());
    let replacement = PendingSessionDeletion::new(&session_id, &project_id, 75_001);
    assert!(write_pending_session_deletion(directory.path(), &replacement).is_err());
    assert_eq!(
        load_pending_session_deletions(directory.path()).unwrap(),
        vec![deletion]
    );
}

#[test]
fn startup_rolls_back_an_uncommitted_permanent_deletion_journal() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("interrupted-pre-commit-delete").unwrap();
    drop(Session::create(sessions.dir().join("interrupted-pre-commit-delete.jsonl")).unwrap());
    sessions
        .rename(session_id.as_str(), "Retained title")
        .unwrap();
    sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 76_000)
        .unwrap();

    let host = OctetHost::new(config.clone()).unwrap();
    let project_id = host.launch_project_id.clone();
    let metadata_directory = sessions.dir().join(".metadata");
    let metadata_path = metadata_directory.join(format!("{}.json", session_id.as_str()));
    let staged_metadata =
        metadata_directory.join(".delete-interrupted-pre-commit-delete-deadbeefdeadbeef");
    std::fs::rename(&metadata_path, &staged_metadata).unwrap();
    write_pending_session_deletion(
        &host.serve_state_dir,
        &PendingSessionDeletion::new(&session_id, &project_id, 76_000),
    )
    .unwrap();
    drop(host);

    let reopened = OctetHost::new(config).unwrap();
    let metadata = sessions.load_metadata(session_id.as_str()).unwrap();
    assert_eq!(metadata.name.as_deref(), Some("Retained title"));
    assert_eq!(metadata.trashed_at_ms, Some(76_000));
    assert!(sessions.path_by_id(session_id.as_str()).is_ok());
    assert!(!staged_metadata.exists());
    assert!(reopened
        .projects
        .lock()
        .unwrap()
        .project_for_session(session_id.as_str())
        .is_some());
    assert!(load_pending_session_deletions(&reopened.serve_state_dir)
        .unwrap()
        .is_empty());
}

#[test]
fn startup_does_not_commit_when_a_transcript_cannot_be_validated() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("unsafe-pre-commit-delete").unwrap();
    let session_path = sessions.dir().join("unsafe-pre-commit-delete.jsonl");
    let mut session = Session::create(&session_path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("retain this transcript".into())],
        })))
        .unwrap();
    drop(session);
    sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 76_500)
        .unwrap();

    let host = OctetHost::new(config.clone()).unwrap();
    let project_id = host.launch_project_id.clone();
    host.goals
        .set(&session_id, "Retain while transcript is unsafe", None)
        .unwrap();
    write_pending_session_deletion(
        &host.serve_state_dir,
        &PendingSessionDeletion::new(&session_id, &project_id, 76_500),
    )
    .unwrap();
    std::fs::remove_file(&session_path).unwrap();
    std::fs::create_dir(&session_path).unwrap();
    drop(host);

    let reopened = OctetHost::new(config).unwrap();
    let pending = load_pending_session_deletions(&reopened.serve_state_dir).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].committed);
    assert!(reopened.goals.get(&session_id).unwrap().is_some());
    assert!(reopened
        .projects
        .lock()
        .unwrap()
        .project_for_session(session_id.as_str())
        .is_some());
}

#[test]
fn recovery_does_not_race_an_active_session_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("locked-recovery-delete").unwrap();
    drop(Session::create(sessions.dir().join("locked-recovery-delete.jsonl")).unwrap());
    sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 76_750)
        .unwrap();

    let host = OctetHost::new(config).unwrap();
    let project_id = host.launch_project_id.clone();
    sessions
        .delete_permanently(session_id.as_str(), 76_750)
        .unwrap();
    let deletion = PendingSessionDeletion {
        committed: true,
        ..PendingSessionDeletion::new(&session_id, &project_id, 76_750)
    };
    write_pending_session_deletion(&host.serve_state_dir, &deletion).unwrap();

    let deletion_guard = host.session_deletion_lock.try_lock().unwrap();
    host.recover_pending_session_deletions();
    assert_eq!(
        load_pending_session_deletions(&host.serve_state_dir).unwrap(),
        vec![deletion]
    );
    drop(deletion_guard);

    host.recover_pending_session_deletions();
    assert!(load_pending_session_deletions(&host.serve_state_dir)
        .unwrap()
        .is_empty());
    assert!(!sessions
        .dir()
        .join(".metadata")
        .read_dir()
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".delete-locked-recovery-delete-")));
}

#[test]
fn startup_finishes_a_committed_deletion_after_project_archive() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("interrupted-delete").unwrap();
    let mut session = Session::create(sessions.dir().join("interrupted-delete.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("interrupt deletion".into())],
        })))
        .unwrap();
    drop(session);
    sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 77_000)
        .unwrap();

    let host = OctetHost::new(config.clone()).unwrap();
    let project_id = host.launch_project_id.clone();
    host.documents
        .as_ref()
        .unwrap()
        .ingest(
            project_id.as_str(),
            session_id.as_str(),
            "interrupted.txt",
            "text/plain",
            bytes::Bytes::from_static(b"pending cleanup"),
        )
        .unwrap();
    let deletion = PendingSessionDeletion {
        committed: true,
        ..PendingSessionDeletion::new(&session_id, &project_id, 77_000)
    };
    write_pending_session_deletion(&host.serve_state_dir, &deletion).unwrap();
    sessions
        .finish_permanent_delete(session_id.as_str())
        .unwrap();
    assert!(host
        .projects
        .lock()
        .unwrap()
        .project_for_session(session_id.as_str())
        .is_some());
    let registry_project_id = RegistryProjectId::parse(project_id.as_str()).unwrap();
    host.projects
        .lock()
        .unwrap()
        .archive(&registry_project_id)
        .unwrap();
    drop(host);

    let reopened = OctetHost::new(config).unwrap();
    assert!(reopened
        .projects
        .lock()
        .unwrap()
        .project_for_session(session_id.as_str())
        .is_none());
    assert!(reopened
        .documents
        .as_ref()
        .unwrap()
        .list_for_session(project_id.as_str(), session_id.as_str())
        .unwrap()
        .is_empty());
    assert!(load_pending_session_deletions(&reopened.serve_state_dir)
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn serve_state_directory_rejects_a_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    symlink(target.path(), directory.path().join(".serve")).unwrap();
    assert!(secure_serve_state_dir(directory.path()).is_err());
}
