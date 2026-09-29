//! Reading a transcript back off disk before a session is admitted.
//! Targeted resume, transcript search, and delegated-session inspection all
//! parse history this process did not write itself, so they are the boundary
//! where a corrupt, trashed, or symlinked transcript must be refused rather
//! than replayed.

use super::*;
use octet_ai::UserMessage;

use super::test_support::*;

#[tokio::test]
async fn transcript_search_reuses_the_initialized_index() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let mut session = Session::create(sessions.dir().join("search-session.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("searchable historical text".into())],
        })))
        .unwrap();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("openai".into()),
            ModelId("gpt-4o-mini".into()),
            "assistant_turn",
        )
        .unwrap();
    drop(session);

    let host = OctetHost::new(config.clone()).unwrap();
    let request = TranscriptSearchRequest {
        query: "historical".into(),
        filter: Default::default(),
        limit: 10,
    };
    let first = HostService::search_transcripts(&host, &request)
        .await
        .unwrap();
    assert_eq!(first.hits.len(), 1);
    assert!(host.search_index_initialized.load(Ordering::Acquire));

    let second = HostService::search_transcripts(&host, &request)
        .await
        .unwrap();
    assert_eq!(second, first);
    drop(host);
    let reopened = OctetHost::new(config).unwrap();
    assert_eq!(
        HostService::search_transcripts(&reopened, &request)
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    assert!(reopened
        .list_sessions()
        .await
        .unwrap()
        .iter()
        .any(|session| session.id.as_str() == "search-session"));
}

#[tokio::test]
async fn delegated_session_references_open_as_locked_path_free_inspectors() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let parent_session_id = "parent-auth";
    let parent_path = sessions.dir().join(format!("{parent_session_id}.jsonl"));
    let mut parent = Session::create(&parent_path).unwrap();
    parent
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("parent session".into())],
        })))
        .unwrap();
    let parent_resource_owner = parent.resource_owner_key();
    drop(parent);
    let principal = format!("octet-subagents@sha256:{}", "a".repeat(64));
    let principal_digest = Sha256::digest(principal.as_bytes());
    let principal_prefix = principal_digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let owner_digest = Sha256::digest(parent_resource_owner.as_bytes());
    let owner_prefix = owner_digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let team = sessions
        .dir()
        .join(".delegation")
        .join("team-0123456789abcdef");
    octet_agent::secure_fs::create_private_directory_all(&team).unwrap();
    let path = team.join(format!(
        "0001-ext-{principal_prefix}-{owner_prefix}-task-cafebabefeed.jsonl"
    ));
    let mut child = Session::create(&path).unwrap();
    child
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("inspect authentication".into())],
        })))
        .unwrap();
    drop(child);
    let child_reference =
        octet_agent::delegated_session_reference(&path).expect("generated child reference");
    let mut provenance = serde_json::to_vec(&serde_json::json!({
        "event": "agent_spawned",
        "session_reference": child_reference.clone(),
        "display_task_name": "review-auth",
        "extension_parent_session_id": parent_session_id,
        "extension_principal": principal.clone(),
        "extension_resource_owner": parent_resource_owner.clone(),
    }))
    .unwrap();
    provenance.push(b'\n');
    octet_agent::secure_fs::write_private_atomic(
        &team.join("provenance.jsonl"),
        &provenance,
        4 * 1024,
    )
    .unwrap();
    let session_id = SessionId::new(child_reference.clone()).unwrap();
    let host = OctetHost::new(config).unwrap();

    let mut driver = host.open_session(&session_id).await.unwrap();
    let seed = driver.seed();
    assert_eq!(seed.summary.title, "parent > review-auth");
    assert_eq!(seed.summary.live_state, SessionLiveState::Locked);
    assert_eq!(seed.snapshot.live_state, SessionLiveState::Locked);
    assert_eq!(seed.snapshot.authority, AuthorityProfile::ReadOnly);
    assert_eq!(
        seed.snapshot
            .delegated_parent_session_id
            .as_ref()
            .map(SessionId::as_str),
        Some(parent_session_id)
    );
    assert_eq!(seed.snapshot.items.len(), 1);
    assert!(!format!("{seed:?}").contains(path.to_str().unwrap()));

    let mut child = Session::open(&path).unwrap();
    child
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(
                "new live child turn: sk-1234567890123456".into(),
            )],
        })))
        .unwrap();
    drop(child);
    let mut saw_live_item = false;
    for _ in 0..4 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), driver.next_event())
            .await
            .unwrap()
            .unwrap();
        if matches!(event.payload, EventPayload::ItemCommitted { .. }) {
            saw_live_item = true;
            break;
        }
    }
    assert!(
        saw_live_item,
        "delegated inspector did not stream the durable turn"
    );
    let original = std::fs::read(&path).unwrap();
    let exported = host.session_export(&session_id).await.unwrap();
    let exported = String::from_utf8(exported.to_vec()).unwrap();
    assert!(exported.contains("new live child turn"));
    assert!(!exported.contains(path.to_str().unwrap()));
    assert!(!exported.contains("sk-1234567890123456"));
    assert!(exported.contains("[REDACTED]"));
    let package: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(package["source_id"], child_reference);
    assert_eq!(package["redacted"], true);

    // Neither inspection nor export requires a launchable fleet roster.
    // Both the raw snapshot and the final restored export ID remain bounded.
    assert!(std::fs::read_dir(sessions.dir().join(".delegation"))
        .unwrap()
        .all(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            name != "fleet.json" && !(name.starts_with("fleet-") && name.ends_with(".json"))
        }));
    let context = host.delegated_session_context(&session_id).unwrap();
    assert!(original.len() < exported.len() - 1);
    for limit in [16, exported.len() - 1] {
        assert_eq!(
            export_delegated_session_bytes(
                &path,
                context.fingerprint,
                &session_id,
                &context.config.workspace,
                &host.serve_state_dir,
                limit,
            ),
            Err(ServiceError::PayloadTooLarge)
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(std::fs::read_dir(&host.serve_state_dir)
        .unwrap()
        .all(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            !name.starts_with(".delegated-session-export-") && !name.starts_with(".session-export-")
        }));

    let discovery = driver.command_discovery().await.unwrap();
    assert!(discovery.commands.is_empty());
    assert!(discovery.skills.is_empty());
    assert_eq!(
        driver
            .dispatch(SessionCommand::Abort { run_id: None })
            .await
            .unwrap_err(),
        ServiceError::Unauthorized
    );
    driver.shutdown().await;

    let mut foreign_owner = serde_json::to_vec(&serde_json::json!({
        "event": "agent_spawned",
        "session_reference": child_reference.clone(),
        "display_task_name": "review-auth",
        "extension_parent_session_id": parent_session_id,
        "extension_principal": principal.clone(),
        "extension_resource_owner": format!("session-{}", "b".repeat(64)),
    }))
    .unwrap();
    foreign_owner.push(b'\n');
    octet_agent::secure_fs::write_private_atomic(
        &team.join("provenance.jsonl"),
        &foreign_owner,
        4 * 1024,
    )
    .unwrap();
    assert!(matches!(
        host.open_session(&session_id).await,
        Err(ServiceError::NotFound)
    ));
    assert_eq!(
        host.session_export(&session_id).await,
        Err(ServiceError::NotFound)
    );

    let mut missing_principal = serde_json::to_vec(&serde_json::json!({
        "event": "agent_spawned",
        "session_reference": child_reference.clone(),
        "display_task_name": "review-auth",
        "extension_parent_session_id": parent_session_id,
        "extension_resource_owner": parent_resource_owner.clone(),
    }))
    .unwrap();
    missing_principal.push(b'\n');
    octet_agent::secure_fs::write_private_atomic(
        &team.join("provenance.jsonl"),
        &missing_principal,
        4 * 1024,
    )
    .unwrap();
    assert!(matches!(
        host.open_session(&session_id).await,
        Err(ServiceError::NotFound)
    ));
    assert_eq!(
        host.session_export(&session_id).await,
        Err(ServiceError::NotFound)
    );
}

#[tokio::test]
async fn targeted_resume_rejects_trashed_transcripts() {
    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, _, _, _) = worker_checkout_fixture(directory.path(), "targeted-trashed");
    let context = host.project_context(Some(&host.launch_project_id)).unwrap();
    context
        .sessions
        .set_lifecycle(session_id.as_str(), SessionStorageLifecycle::Trash, 1_000)
        .unwrap();

    assert!(matches!(
        host.open_session(&session_id).await,
        Err(ServiceError::InvalidBoundary)
    ));
}

#[tokio::test]
async fn targeted_resume_rejects_unsafe_metadata_directory() {
    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, _, _, _) =
        worker_checkout_fixture(directory.path(), "targeted-unsafe-metadata");
    let context = host.project_context(Some(&host.launch_project_id)).unwrap();
    std::fs::write(context.sessions.dir().join(".metadata"), b"not a directory").unwrap();

    assert!(matches!(
        host.stored_session_summary(&session_id),
        Err(ServiceError::InvalidSeed)
    ));
    assert!(matches!(
        host.open_session(&session_id).await,
        Err(ServiceError::InvalidSeed)
    ));
}

#[tokio::test]
async fn targeted_resume_and_catalog_reject_corrupt_transcripts() {
    use std::io::Write as _;

    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, _, _, path) =
        worker_checkout_fixture(directory.path(), "targeted-corrupt");
    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    transcript.write_all(b"{\n").unwrap();
    drop(transcript);

    assert!(matches!(
        host.stored_session_summary(&session_id),
        Err(ServiceError::InvalidSeed)
    ));
    assert!(matches!(
        host.open_session(&session_id).await,
        Err(ServiceError::InvalidSeed)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn targeted_resume_and_catalog_reject_symlinked_transcripts() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, _, _, path) =
        worker_checkout_fixture(directory.path(), "targeted-symlink");
    let outside = directory.path().join("outside.jsonl");
    std::fs::rename(&path, &outside).unwrap();
    symlink(&outside, &path).unwrap();

    assert!(matches!(
        host.stored_session_summary(&session_id),
        Err(ServiceError::InvalidSeed)
    ));
    assert!(matches!(
        host.open_session(&session_id).await,
        Err(ServiceError::NotFound)
    ));
}
