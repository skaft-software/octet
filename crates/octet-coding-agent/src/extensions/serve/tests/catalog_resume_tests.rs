//! Model-catalog fallbacks and resuming a session whose model is gone.
//! The host has to keep serving a session whose historical model was retired,
//! reconcile a terminal session that lives in another registered project, and
//! still replay the active branch exactly. Grouped together because each one
//! starts from catalog reconciliation rather than from a run.

use super::*;
use octet_ai::UserMessage;
use octet_serve_backend::{SessionSupervisor, SupervisorConfig};

use super::test_support::*;

const REMOVED_MODEL_ID: &str = "removed-provider/retired-model";

fn configure_removed_model(config: &mut Config) {
    config.model = Some(ModelId(REMOVED_MODEL_ID.into()));
    config.model_explicit = true;
}

#[tokio::test]
async fn inventory_bootstrap_falls_back_when_the_configured_model_is_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    configure_removed_model(&mut config);
    let host = OctetHost::new(config).unwrap();
    let supervisor = SessionSupervisor::new(Arc::new(host), SupervisorConfig::default());

    let bootstrap = supervisor.inventory_bootstrap().await.unwrap();
    assert!(!bootstrap.models.is_empty());
    bootstrap.validate().unwrap();
}

#[tokio::test]
async fn terminal_session_in_another_registered_project_is_reconciled_and_resumable() {
    let directory = tempfile::tempdir().unwrap();
    let session_dir = directory.path().join("sessions");
    let first_workspace = directory.path().join("first-workspace");
    let launch_workspace = directory.path().join("launch-workspace");
    std::fs::create_dir_all(&first_workspace).unwrap();
    std::fs::create_dir_all(&launch_workspace).unwrap();
    let first_workspace = first_workspace.canonicalize().unwrap();
    let launch_workspace = launch_workspace.canonicalize().unwrap();

    let mut first_config = serve_test_config(&first_workspace);
    first_config.session_dir = session_dir.clone();
    let first_host = OctetHost::new(first_config).unwrap();
    let first_project_id = first_host.launch_project_id.clone();
    let terminal_selection = first_host.default_selection().unwrap();
    drop(first_host);

    let sessions = SessionStore::new(&session_dir, &first_workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let session_id = SessionId::new("terminal-created-session").unwrap();
    let mut session =
        Session::create(sessions.dir().join("terminal-created-session.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("created in the terminal".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Config {
            model: Some(terminal_selection.model.clone()),
            reasoning: Some(terminal_selection.reasoning.clone()),
            reasoning_mode: Some("standard".into()),
        })
        .unwrap();
    drop(session);

    let mut launch_config = serve_test_config(&launch_workspace);
    launch_config.session_dir = session_dir;
    let host = OctetHost::new(launch_config).unwrap();

    assert_eq!(
        host.projects
            .lock()
            .unwrap()
            .project_for_session(session_id.as_str()),
        Some(registry_project_id(&first_project_id).unwrap())
    );
    let summary = host
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .find(|summary| summary.id == session_id)
        .unwrap();
    assert_eq!(summary.project_id, Some(first_project_id));
    assert_eq!(summary.model, terminal_selection);

    let mut driver = host.open_session(&session_id).await.unwrap();
    assert_eq!(driver.seed().summary.model, terminal_selection);
    driver.shutdown().await;
}

#[tokio::test]
async fn catalog_selection_matches_full_replay_on_the_active_branch() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = serve_test_config(directory.path());
    let workspace = directory.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    config.workspace = workspace.clone();
    config.invocation_cwd = workspace;
    config.model = Some(ModelId("gpt-4o-mini".into()));
    config.model_explicit = true;
    let host = OctetHost::new(config).unwrap();
    let active = host.default_selection().unwrap();
    let inactive = host
        .models
        .iter()
        .find(|summary| summary.id != active.model)
        .map(selection_from_summary)
        .expect("Serve test catalog must expose a second model");
    let session_id = SessionId::new("catalog-active-branch").unwrap();
    let context = host.project_context(Some(&host.launch_project_id)).unwrap();
    std::fs::create_dir_all(context.sessions.dir()).unwrap();
    host.projects
        .lock()
        .unwrap()
        .bind_session(
            session_id.as_str(),
            &registry_project_id(&host.launch_project_id).unwrap(),
        )
        .unwrap();
    let path = context.sessions.dir().join("catalog-active-branch.jsonl");
    let mut session = Session::create(&path).unwrap();
    let root = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("root prompt".into())],
        })))
        .unwrap();
    let active_config = session
        .append(EntryValue::Config {
            model: Some(active.model.clone()),
            reasoning: Some(active.reasoning.clone()),
            reasoning_mode: Some("standard".into()),
        })
        .unwrap();
    session.checkout(root).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("inactive prompt".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Config {
            model: Some(inactive.model),
            reasoning: Some(inactive.reasoning),
            reasoning_mode: Some("standard".into()),
        })
        .unwrap();
    session.checkout(active_config).unwrap();
    drop(session);

    let replayed = Session::open_read_only(&path).unwrap();
    let catalog_entry = context.sessions.catalog_by_id(session_id.as_str()).unwrap();
    let full = selection_from_session(&replayed, &host.catalog, &context.config).unwrap();
    let catalog =
        selection_from_catalog_entry(&catalog_entry, &host.catalog, &context.config).unwrap();
    assert_eq!(catalog, full);
    assert_eq!(catalog, active);

    let listed = host
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .find(|summary| summary.id == session_id)
        .unwrap();
    assert_eq!(listed.model, active);
}

#[tokio::test]
async fn serve_created_session_resumes_when_its_historical_model_is_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let host = OctetHost::new(config.clone()).unwrap();
    let project_id = host.launch_project_id.clone();
    let mut driver = host
        .create_session(CreateSessionRequest {
            project_id: Some(project_id.clone()),
            provisional: true,
            authority: AuthorityProfile::FullAccess,
            model: None,
        })
        .await
        .unwrap();
    let created_seed = driver.seed();
    let session_id = created_seed.summary.id;
    let historical_selection = created_seed.summary.model;
    driver.command_discovery().await.unwrap();
    driver.shutdown().await;

    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    let path = sessions.path_by_id(session_id.as_str()).unwrap();
    let mut session = Session::open(path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("created in Serve".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Config {
            model: Some(historical_selection.model.clone()),
            reasoning: Some(historical_selection.reasoning.clone()),
            reasoning_mode: Some("standard".into()),
        })
        .unwrap();
    drop(session);
    drop(host);

    let mut reopened = OctetHost::new(config).unwrap();
    assert!(reopened
        .catalog
        .resolve(&ModelId(historical_selection.model.clone()))
        .is_ok());
    let advertised_model_count = reopened.models.len();
    reopened.models.retain(|model| {
        model.provider != historical_selection.provider || model.id != historical_selection.model
    });
    assert!(!reopened.models.is_empty());
    assert!(reopened.models.len() < advertised_model_count);
    let summary = reopened
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .find(|summary| summary.id == session_id)
        .unwrap();
    assert_eq!(summary.project_id, Some(project_id));
    assert_ne!(summary.model, historical_selection);
    assert!(reopened.models.iter().any(|model| {
        model.provider == summary.model.provider && model.id == summary.model.model
    }));

    let mut resumed = reopened.open_session(&session_id).await.unwrap();
    let resumed_selection = resumed.seed().summary.model;
    assert_ne!(resumed_selection, historical_selection);
    assert!(reopened.models.iter().any(|model| {
        model.provider == resumed_selection.provider && model.id == resumed_selection.model
    }));
    resumed.shutdown().await;
}
