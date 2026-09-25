//! Session-core isolation and bounded replay integration tests.

use octet_serve_backend::{
    ActorConfig, ActorOwnerState, AuthorityProfile, CommandId, ContextUsage, DeviceId,
    DriverCommandOutcome, EventPayload, HostId, JournalConfig, ModelSelection, PromptInput,
    ReplayResponse, RunId, SessionActorCore, SessionCommand, SessionCommandEnvelope, SessionCursor,
    SessionId, SessionLiveState, SessionSeed, SessionSnapshot, SessionSummary,
};

fn seed(index: usize) -> SessionSeed {
    let session_id = SessionId::new(format!("lifecycle-session-{index}")).unwrap();
    let model = ModelSelection {
        provider: "fixture".into(),
        model: "fixture-model".into(),
        reasoning: "off".into(),
    };
    SessionSeed {
        summary: SessionSummary {
            id: session_id.clone(),
            project_id: None,
            title: "Fresh session".into(),
            tags: Vec::new(),
            created_at_ms: index as u64,
            modified_at_ms: index as u64,
            pinned: false,
            archived: false,
            lifecycle: octet_serve_backend::SessionCatalogState::Active,
            retention: None,
            forked_from: None,
            provisional: true,
            live_state: SessionLiveState::Idle,
            attention: octet_serve_backend::AttentionState::None,
            pull_request: None,
            owner: ActorOwnerState::Hosted,
            model: model.clone(),
        },
        snapshot: SessionSnapshot {
            session_id,
            delegated_parent_session_id: None,
            actor_generation: 1,
            cursor: SessionCursor::zero(1),
            durable_head: None,
            branches: Default::default(),
            live_state: SessionLiveState::Idle,
            active_run_id: None,
            model,
            authority: AuthorityProfile::FullAccess,
            context: ContextUsage::default(),
            items: Vec::new(),
            extension_presentations: Vec::new(),
            pending_requests: Vec::new(),
            sources: Vec::new(),
            artifacts: Vec::new(),
        },
    }
}

fn prompt_command(session_id: SessionId, command_id: &str) -> SessionCommandEnvelope {
    SessionCommandEnvelope::new(
        HostId::new("lifecycle-host").unwrap(),
        DeviceId::new("lifecycle-device").unwrap(),
        session_id,
        CommandId::new(command_id).unwrap(),
        1,
        Some(1),
        SessionCommand::SubmitPrompt {
            input: PromptInput {
                text: "bounded prompt".into(),
                attachments: Vec::new(),
                document_ids: Vec::new(),
                project_file_ids: Vec::new(),
            },
        },
    )
}

fn abort_command(session_id: SessionId, command_id: &str) -> SessionCommandEnvelope {
    SessionCommandEnvelope::new(
        HostId::new("lifecycle-host").unwrap(),
        DeviceId::new("lifecycle-device").unwrap(),
        session_id,
        CommandId::new(command_id).unwrap(),
        2,
        Some(1),
        SessionCommand::Abort { run_id: None },
    )
}

#[tokio::test]
async fn ten_core_sessions_keep_effects_isolated_and_replay_bounded() {
    let mut cores = (0..10)
        .map(|index| {
            SessionActorCore::new(
                HostId::new("lifecycle-host").unwrap(),
                seed(index),
                ActorConfig {
                    journal: JournalConfig {
                        event_capacity: 2,
                        byte_capacity: 64 * 1024,
                    },
                    ..ActorConfig::default()
                },
            )
            .unwrap()
        })
        .collect::<Vec<_>>();

    for (index, core) in cores.iter_mut().enumerate() {
        let session_id = SessionId::new(format!("lifecycle-session-{index}")).unwrap();
        let command = prompt_command(session_id.clone(), &format!("prompt-{index}"));
        let run_id = RunId::new(format!("run-{index}")).unwrap();
        let first = core
            .admit_command(command.clone(), 10, move |_| {
                let run_id = run_id.clone();
                async move {
                    Ok(DriverCommandOutcome::run(
                        run_id.clone(),
                        vec![octet_serve_backend::TimestampedEvent::new(
                            1,
                            EventPayload::SessionStateChanged {
                                state: SessionLiveState::Working,
                                active_run_id: Some(run_id),
                            },
                        )],
                    ))
                }
            })
            .await
            .unwrap();
        assert!(!first.cached);

        let repeated = core
            .admit_command(command, 99, |_| async {
                panic!("duplicate delivery must not dispatch a second run")
            })
            .await
            .unwrap();
        assert!(repeated.cached);
        assert_eq!(repeated.ack, first.ack);

        let cancelled = core
            .admit_command(
                abort_command(session_id.clone(), &format!("abort-{index}")),
                20,
                |_| async {
                    Ok(DriverCommandOutcome::with_events(vec![
                        octet_serve_backend::TimestampedEvent::new(
                            2,
                            EventPayload::SessionStateChanged {
                                state: SessionLiveState::Stopped,
                                active_run_id: None,
                            },
                        ),
                    ]))
                },
            )
            .await
            .unwrap();
        assert!(!cancelled.cached);
        assert!(matches!(
            cancelled.ack.disposition,
            octet_serve_backend::AckDisposition::Accepted { .. }
        ));
        assert_eq!(core.snapshot().live_state, SessionLiveState::Stopped);

        core.publish(octet_serve_backend::TimestampedEvent::new(
            3,
            EventPayload::SessionMetadataChanged {
                title: Some(format!("settled-{index}")),
                pinned: None,
                archived: None,
            },
        ))
        .unwrap();
        assert_eq!(core.view().summary.title, format!("settled-{index}"));

        let ReplayResponse::Gap { gap, snapshot } = core.replay_after(SessionCursor::zero(1))
        else {
            panic!("old cursors must receive a bounded snapshot fallback");
        };
        assert_eq!(gap.earliest_available.sequence, 2);
        assert_eq!(gap.latest_available.sequence, 3);
        assert_eq!(snapshot.session_id, session_id);

        let ReplayResponse::Events {
            events, through, ..
        } = core.replay_after(SessionCursor {
            actor_generation: 1,
            sequence: 2,
        })
        else {
            panic!("the retained replay tail should be available");
        };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cursor.sequence, 3);
        assert_eq!(through.sequence, 3);
    }

    for (index, core) in cores.iter().enumerate() {
        assert_eq!(core.snapshot().live_state, SessionLiveState::Stopped);
        assert_eq!(core.view().summary.title, format!("settled-{index}"));
    }
}
