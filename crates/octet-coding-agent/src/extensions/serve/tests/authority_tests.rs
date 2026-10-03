//! Host authority admission and the sandbox gate the serve host advertises.
//! Every case here answers one question: can a session reach a capability the
//! host never offered, or was a plan built under a policy that has since
//! changed? Kept apart from the projection tests because authority gates
//! admission, it does not shape anything the transcript renders.

use super::*;
use octet_serve_backend::{
    AckDisposition, CommandId, DeviceId, SessionCommandEnvelope, SessionSupervisor,
    SupervisorConfig,
};

use super::test_support::*;

fn authority_test_sandbox(authority: AuthorityProfile) -> crate::config::SandboxPolicy {
    crate::config::SandboxPolicy {
        allow_edit: authority != AuthorityProfile::ReadOnly,
        allow_write: authority != AuthorityProfile::ReadOnly,
        allow_process: authority == AuthorityProfile::FullAccess,
        allow_external_paths: authority == AuthorityProfile::FullAccess,
        ..crate::config::SandboxPolicy::default()
    }
}

#[test]
fn serve_authority_catalog_describes_all_host_sandbox_gate_combinations() {
    for bits in 0..32 {
        let sandbox = crate::config::SandboxPolicy {
            allow_edit: bits & 1 != 0,
            allow_write: bits & 2 != 0,
            allow_process: bits & 4 != 0,
            allow_shell: bits & 8 != 0,
            allow_external_paths: bits & 16 != 0,
            ..crate::config::SandboxPolicy::default()
        };
        let authority = authority_ceiling_from_sandbox(&sandbox);
        assert_eq!(authority_profiles_from_sandbox(&sandbox), vec![authority]);
        match authority {
            AuthorityProfile::ReadOnly => {
                assert!(!sandbox.allow_edit && !sandbox.allow_write);
                assert!(!sandbox.process_execution_allowed());
            }
            AuthorityProfile::Workspace => {
                assert!(sandbox.allow_edit || sandbox.allow_write);
                assert!(!sandbox.allow_external_paths);
                assert!(!sandbox.process_execution_allowed());
            }
            AuthorityProfile::FullAccess => {
                assert!(
                    sandbox.process_execution_allowed()
                        || ((sandbox.allow_edit || sandbox.allow_write)
                            && sandbox.allow_external_paths)
                );
            }
        }
    }
}

#[tokio::test]
async fn serve_authority_admission_rejects_unadvertised_fresh_session_profiles() {
    let profiles = [
        AuthorityProfile::ReadOnly,
        AuthorityProfile::Workspace,
        AuthorityProfile::FullAccess,
    ];
    for authority in profiles {
        let directory = tempfile::tempdir().unwrap();
        let mut config = project_test_config(directory.path(), true);
        config.sandbox = authority_test_sandbox(authority);
        let host = Arc::new(OctetHost::new(config).unwrap());
        assert_eq!(host.authority_profiles(), vec![authority]);
        assert_eq!(host.authority_ceiling(), authority);
        assert_eq!(
            host.capabilities().terminal,
            authority == AuthorityProfile::FullAccess
        );
        for requested in profiles
            .into_iter()
            .filter(|requested| *requested != authority)
        {
            assert!(matches!(
                host.create_session(CreateSessionRequest {
                    project_id: Some(host.launch_project_id.clone()),
                    provisional: true,
                    authority: requested,
                    model: None,
                })
                .await,
                Err(ServiceError::Unauthorized)
            ));
        }
        assert!(host
            .projects
            .lock()
            .unwrap()
            .sessions_for_project(&registry_project_id(&host.launch_project_id).unwrap())
            .is_empty());
        let supervisor = SessionSupervisor::new(Arc::clone(&host), SupervisorConfig::default());
        let bootstrap = supervisor.launch(None).await.unwrap();
        assert_eq!(bootstrap.authority_profiles, vec![authority]);
        assert_eq!(bootstrap.authority_ceiling, authority);
        let selected = bootstrap.selected_session.unwrap();
        assert_eq!(selected.authority, authority);
        for (index, requested) in profiles.into_iter().enumerate() {
            let admission = supervisor
                .command(
                    SessionCommandEnvelope::new(
                        host.descriptor.id.clone(),
                        DeviceId::new("authority-device").unwrap(),
                        selected.session_id.clone(),
                        CommandId::new(format!("authority-{index}")).unwrap(),
                        1,
                        Some(selected.actor_generation),
                        SessionCommand::SetAuthority {
                            authority: requested,
                        },
                    ),
                    1,
                )
                .await
                .unwrap();
            if requested == authority {
                assert!(matches!(
                    admission.ack.disposition,
                    AckDisposition::Accepted {
                        run_id: None,
                        created_session_id: None,
                    }
                ));
            } else {
                assert!(matches!(
                    admission.ack.disposition,
                    AckDisposition::Rejected { .. }
                ));
            }
            assert!(admission.published.is_empty());
            assert_eq!(
                supervisor
                    .session_view(&selected.session_id)
                    .await
                    .unwrap()
                    .snapshot
                    .authority,
                authority
            );
        }
    }
}

#[tokio::test]
async fn serve_authority_changes_never_reach_idle_or_busy_worker_mailboxes() {
    let profiles = [
        AuthorityProfile::ReadOnly,
        AuthorityProfile::Workspace,
        AuthorityProfile::FullAccess,
    ];
    for authority in profiles {
        for busy in [false, true] {
            let (commands, mut worker_commands) = mpsc::channel(1);
            let (_event_sender, events) = mpsc::channel(1);
            let mut seed = empty_seed(
                SessionId::new("immutable-authority").unwrap(),
                None,
                ModelSelection {
                    provider: "test".into(),
                    model: "test".into(),
                    reasoning: "off".into(),
                },
                authority,
                1,
            );
            if busy {
                seed.snapshot.live_state = SessionLiveState::Working;
                seed.snapshot.active_run_id = Some(RunId::new("authority-run").unwrap());
            }
            let mut driver = OctetSessionDriver {
                seed: seed.clone(),
                commands: Some(commands),
                events,
                buffered_events: VecDeque::new(),
                worker: None,
                inspect_only: false,
                inspection: None,
            };
            for requested in profiles {
                let outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    driver.dispatch(SessionCommand::SetAuthority {
                        authority: requested,
                    }),
                )
                .await
                .expect("authority admission must not await an idle worker");
                if requested == authority {
                    let outcome = outcome.unwrap();
                    assert!(outcome.events.is_empty());
                    assert!(outcome.run_id.is_none());
                    assert!(outcome.created_session_id.is_none());
                } else {
                    assert!(matches!(outcome, Err(ServiceError::Unauthorized)));
                }
                assert_eq!(driver.seed().snapshot, seed.snapshot);
                assert!(matches!(
                    worker_commands.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
                assert!(driver.buffered_events.is_empty());
            }
        }
    }
}

#[test]
fn serve_authority_rejects_mislabeled_plans_before_bootstrap_or_prepared_session_consumption() {
    for authority in [AuthorityProfile::ReadOnly, AuthorityProfile::Workspace] {
        let directory = tempfile::tempdir().unwrap();
        let mut plan = pull_request_worker_plan(directory.path(), "mislabeled-authority");
        let SessionSelection::CreateNew(path) = plan.launch.session.clone() else {
            panic!("expected a new session");
        };
        plan.authority = authority;
        let error = build_worker_app(&mut plan).err().unwrap();
        assert!(error.to_string().contains("immutable host policy"));
        assert!(!path.exists());

        let session = Session::create(&path).unwrap();
        plan.launch.session = SessionSelection::OpenExisting(path);
        *plan.prepared_session.get_mut().unwrap() = Some(session);
        let error = build_worker_app(&mut plan).err().unwrap();
        assert!(error.to_string().contains("immutable host policy"));
        assert!(plan.prepared_session.get_mut().unwrap().is_some());
    }
}

#[test]
fn serve_authority_rebuild_preserves_launch_gates_tools_and_runtime_domain() {
    for authority in [
        AuthorityProfile::ReadOnly,
        AuthorityProfile::Workspace,
        AuthorityProfile::FullAccess,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut plan = pull_request_worker_plan(directory.path(), "authority-rebuild");
        plan.config.sandbox = authority_test_sandbox(authority);
        plan.authority = authority;
        plan.launch.model = ModelId("gpt-4o-mini".into());
        let app = build_worker_app(&mut plan).unwrap();
        let tools = app.agent.registered_tool_names();
        assert_eq!(
            tools.contains(&"edit".to_owned()),
            plan.config.sandbox.allow_edit
        );
        assert_eq!(
            tools.contains(&"write".to_owned()),
            plan.config.sandbox.allow_write
        );
        assert_eq!(
            tools.contains(&"bash".to_owned()),
            plan.config.sandbox.process_execution_allowed()
        );
        let domain = app
            .executable_extensions
            .runtime_manager()
            .unwrap()
            .domain()
            .clone();
        let mut rebuilt = rebuild_app(app, None, None, None, None).unwrap();
        assert_eq!(rebuilt.config.sandbox, plan.config.sandbox);
        assert_eq!(rebuilt.config.effect_policy, plan.config.effect_policy);
        assert_eq!(rebuilt.agent.registered_tool_names(), tools);
        assert_eq!(
            rebuilt
                .executable_extensions
                .runtime_manager()
                .unwrap()
                .domain(),
            &domain
        );
        assert_eq!(
            authority_ceiling_from_sandbox(&rebuilt.config.sandbox),
            authority
        );
        rebuilt.executable_extensions.shutdown_blocking();
    }
}

#[tokio::test]
async fn serve_authority_resume_fork_and_reconfiguration_use_current_host_policy() {
    for authority in [
        AuthorityProfile::ReadOnly,
        AuthorityProfile::Workspace,
        AuthorityProfile::FullAccess,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (previous_host, session_id, _, fork_entry, _) =
            worker_checkout_fixture(directory.path(), "authority-resume");
        let mut config = previous_host.config.clone();
        config.sandbox = authority_test_sandbox(authority);
        drop(previous_host);
        // Historical sessions take the current launch policy, never a
        // stale browser label or a policy inferred from transcript content.
        let host = OctetHost::new(config.clone()).unwrap();
        let mut driver = host.open_session(&session_id).await.unwrap();
        assert_eq!(driver.seed().snapshot.authority, authority);
        driver.command_discovery().await.unwrap();
        let outcome = driver
            .dispatch(SessionCommand::ChangeReasoning {
                reasoning: "off".into(),
            })
            .await
            .unwrap();
        assert!(outcome.events.iter().any(|event| matches!(
            event.payload,
            EventPayload::SessionSettingsChanged { authority: projected, .. }
                if projected == authority
        )));
        for requested in [
            AuthorityProfile::ReadOnly,
            AuthorityProfile::Workspace,
            AuthorityProfile::FullAccess,
        ]
        .into_iter()
        .filter(|requested| *requested != authority)
        {
            assert!(matches!(
                driver
                    .dispatch(SessionCommand::SetAuthority {
                        authority: requested
                    })
                    .await,
                Err(ServiceError::Unauthorized)
            ));
        }
        let fork = driver
            .dispatch(SessionCommand::ForkConversation {
                entry_id: fork_entry,
            })
            .await
            .unwrap()
            .created_session_id
            .unwrap();
        driver.shutdown().await;
        drop(host);
        let reopened = OctetHost::new(config).unwrap();
        for id in [&session_id, &fork] {
            let mut resumed = reopened.open_session(id).await.unwrap();
            assert_eq!(resumed.seed().snapshot.authority, authority);
            resumed.command_discovery().await.unwrap();
            resumed.shutdown().await;
        }
    }
}
