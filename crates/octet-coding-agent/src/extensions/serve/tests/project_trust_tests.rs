//! Project trust, the only thing between a session and the filesystem.
//! A workspace must be trusted before its sidecars are written, an imported
//! project must never widen its own root authority, and the file browser has to
//! honour the same write policy the rest of the host does.

use super::*;

use super::test_support::*;

#[tokio::test]
async fn real_project_trust_is_required_and_session_binding_survives_restart() {
    let fixture = tempfile::tempdir().unwrap();
    let config = project_test_config(fixture.path(), false);
    let host = OctetHost::new(config.clone()).unwrap();
    let launch_project = host.launch_project_id.clone();
    let projects = host.list_projects().await.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, launch_project);
    assert!(!projects[0].trusted);
    assert!(projects[0].available);
    assert!(projects[0].is_default);

    let request = CreateSessionRequest {
        project_id: Some(launch_project.clone()),
        provisional: true,
        authority: AuthorityProfile::FullAccess,
        model: None,
    };
    assert!(matches!(
        host.create_session(request.clone()).await,
        Err(ServiceError::Unauthorized)
    ));
    let trusted = host.set_project_trust(&launch_project, true).await.unwrap();
    assert!(trusted.trusted);
    let driver = host.create_session(request.clone()).await.unwrap();
    let session_id = driver.seed().summary.id;
    assert_eq!(
        driver.seed().summary.project_id,
        Some(launch_project.clone())
    );
    assert_eq!(
        host.projects
            .lock()
            .unwrap()
            .project_for_session(session_id.as_str())
            .unwrap()
            .as_str(),
        launch_project.as_str()
    );
    drop(driver);
    drop(host);

    let reopened = OctetHost::new(config).unwrap();
    assert!(
        reopened
            .list_projects()
            .await
            .unwrap()
            .into_iter()
            .find(|project| project.id == launch_project)
            .unwrap()
            .trusted,
        "a durable explicit trust grant must not depend on the next CLI flag"
    );
    assert_eq!(
        reopened
            .projects
            .lock()
            .unwrap()
            .project_for_session(session_id.as_str())
            .unwrap()
            .as_str(),
        launch_project.as_str()
    );
    reopened
        .set_project_trust(&launch_project, false)
        .await
        .unwrap();
    assert!(matches!(
        reopened.create_session(request).await,
        Err(ServiceError::Unauthorized)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_trust_recovers_a_replaced_launch_workspace() {
    let fixture = tempfile::tempdir().unwrap();
    let config = project_test_config(fixture.path(), false);
    let host = OctetHost::new(config.clone()).unwrap();
    let launch_project = host.launch_project_id.clone();
    host.set_project_trust(&launch_project, true).await.unwrap();
    drop(host);

    std::fs::remove_dir(&config.workspace).unwrap();
    std::fs::create_dir(&config.workspace).unwrap();

    let host = OctetHost::new(config.clone()).unwrap();
    let stale = host
        .list_projects()
        .await
        .unwrap()
        .into_iter()
        .find(|project| project.id == launch_project)
        .unwrap();
    assert!(stale.trusted);
    assert!(!stale.available);

    let recovered = host.set_project_trust(&launch_project, true).await.unwrap();
    assert!(recovered.trusted);
    assert!(recovered.available);
    assert_eq!(recovered.id, launch_project);
}

#[cfg(unix)]
#[tokio::test]
async fn project_file_browser_is_trust_confined_and_honors_write_policy() {
    let fixture = tempfile::tempdir().unwrap();
    let config = project_test_config(fixture.path(), true);
    std::fs::write(config.workspace.join("main.rs"), "fn main() {}\n").unwrap();

    let host = OctetHost::new(config.clone()).unwrap();
    let project_id = host.launch_project_id.clone();
    assert!(host.capabilities().project_file_browser);
    assert!(host.capabilities().project_file_write);

    let tree = host.project_file_tree(&project_id, "").await.unwrap();
    assert!(tree.entries.iter().any(|entry| entry.name == "main.rs"));
    assert_eq!(tree.path, "");

    let read = host
        .read_project_file(&project_id, "main.rs", None, None)
        .await
        .unwrap();
    assert_eq!(read.path, "main.rs");
    let version = read.sha256.unwrap();
    let write = host
        .write_project_file(
            &project_id,
            "main.rs",
            "fn main() { println!(\"updated\"); }\n",
            &version,
            false,
        )
        .await
        .unwrap();
    assert_eq!(write.path, "main.rs");
    assert_eq!(
        std::fs::read_to_string(config.workspace.join("main.rs")).unwrap(),
        "fn main() { println!(\"updated\"); }\n"
    );
    drop(host);

    let mut read_only = config;
    read_only.sandbox.allow_write = false;
    let read_only_host = OctetHost::new(read_only).unwrap();
    assert!(read_only_host.capabilities().project_file_browser);
    assert!(!read_only_host.capabilities().project_file_write);
    assert!(matches!(
        read_only_host
            .write_project_file(&project_id, "main.rs", "updated", &write.sha256, false)
            .await,
        Err(ProjectFileSystemError::WriteUnavailable)
    ));
}

#[tokio::test]
async fn imported_project_lifecycle_never_exposes_or_bypasses_its_root_authority() {
    let fixture = tempfile::tempdir().unwrap();
    let config = project_test_config(fixture.path(), true);
    let imported_root = fixture.path().join("private-imported-root");
    std::fs::create_dir(&imported_root).unwrap();
    let first_host = OctetHost::new(config.clone()).unwrap();
    assert_eq!(
        first_host
            .import_project("browser-authored-candidate", Some("Rejected"))
            .await
            .unwrap_err(),
        ServiceError::Unavailable,
        "the browser cannot mint or submit filesystem authority"
    );
    drop(first_host);

    let mut imported_launch = config;
    imported_launch.workspace = imported_root.clone();
    imported_launch.invocation_cwd = imported_root.clone();
    imported_launch.workspace_trusted = false;
    let host = OctetHost::new(imported_launch).unwrap();
    let imported = host
        .list_projects()
        .await
        .unwrap()
        .into_iter()
        .find(|project| project.name == "private-imported-root")
        .unwrap();
    host.set_default_project(&imported.id).await.unwrap();
    assert_eq!(
        host.project_context(None).unwrap().config.workspace,
        fixture.path().join("workspace").canonicalize().unwrap(),
        "a cold launch must skip an untrusted default when another trusted project exists"
    );
    assert!(!imported.trusted);
    assert!(imported.available);
    assert!(!imported.archived);
    let public_json = serde_json::to_string(&imported).unwrap();
    assert!(!public_json.contains(imported_root.to_str().unwrap()));
    assert!(matches!(
        host.create_session(CreateSessionRequest {
            project_id: Some(imported.id.clone()),
            provisional: true,
            authority: AuthorityProfile::FullAccess,
            model: None,
        })
        .await,
        Err(ServiceError::Unauthorized)
    ));

    host.set_project_trust(&imported.id, true).await.unwrap();
    let context = host.project_context(Some(&imported.id)).unwrap();
    assert_eq!(
        context.config.workspace,
        imported_root.canonicalize().unwrap()
    );
    assert_eq!(context.config.invocation_cwd, context.config.workspace);
    assert!(context.config.workspace_trusted);

    let renamed = host.rename_project(&imported.id, "Renamed").await.unwrap();
    assert_eq!(renamed.name, "Renamed");
    assert!(
        host.set_default_project(&imported.id)
            .await
            .unwrap()
            .is_default
    );
    let archived = host.archive_project(&imported.id).await.unwrap();
    assert!(archived.archived);
    assert!(!archived.trusted);
    assert!(!archived.is_default);
    assert!(matches!(
        host.project_context(Some(&imported.id)),
        Err(ServiceError::InvalidBoundary)
    ));
}
