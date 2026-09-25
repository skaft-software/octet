//! Session-owner lifecycle, generation fencing, and attachment integration tests.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use async_trait::async_trait;
use octet_serve_backend::{
    AckDisposition, ActorConfig, ActorError, ActorOwnerState, AuthorityProfile, CommandId,
    ContextUsage, DeviceId, DriverCommandOutcome, EventPayload, HostId, JournalConfig,
    ModelSelection, PromptInput, ReplayResponse, ServiceError, SessionActor, SessionActorCore,
    SessionCommand, SessionCommandEnvelope, SessionCursor, SessionDriver, SessionId,
    SessionLiveState, SessionSeed, SessionSnapshot, SessionSummary, TimestampedEvent,
    UsageSnapshot,
};

fn seed(index: usize) -> SessionSeed {
    let session_id = SessionId::new(format!("lifecycle-full-session-{index}")).unwrap();
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
        HostId::new("lifecycle-full-host").unwrap(),
        DeviceId::new("lifecycle-full-device").unwrap(),
        session_id,
        CommandId::new(command_id).unwrap(),
        1,
        Some(1),
        SessionCommand::SubmitPrompt {
            input: PromptInput {
                text: "bounded lifecycle prompt".into(),
                attachments: Vec::new(),
                document_ids: Vec::new(),
                project_file_ids: Vec::new(),
            },
        },
    )
}

struct FixtureDriver {
    seed: SessionSeed,
    dispatch_count: Arc<AtomicUsize>,
    shutdown_count: Arc<AtomicUsize>,
    owner_lost: bool,
}

#[async_trait]
impl SessionDriver for FixtureDriver {
    fn seed(&self) -> SessionSeed {
        self.seed.clone()
    }

    async fn dispatch(
        &mut self,
        _command: SessionCommand,
    ) -> Result<DriverCommandOutcome, ServiceError> {
        self.dispatch_count.fetch_add(1, Ordering::SeqCst);
        if self.owner_lost {
            Err(ServiceError::OwnerLost)
        } else {
            Ok(DriverCommandOutcome::default())
        }
    }

    async fn shutdown(&mut self) {
        self.shutdown_count.fetch_add(1, Ordering::SeqCst);
    }
}

async fn wait_for_shutdown(counter: &AtomicUsize) {
    let settled = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if counter.load(Ordering::SeqCst) == 1 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(settled.is_ok(), "the detached driver did not quiesce");
}

#[tokio::test]
async fn dropping_one_attachment_leaves_owner_running_and_command_effect_once() {
    let dispatch_count = Arc::new(AtomicUsize::new(0));
    let shutdown_count = Arc::new(AtomicUsize::new(0));
    let session_id = seed(0).summary.id.clone();
    let owner = SessionActor::spawn(
        HostId::new("lifecycle-full-host").unwrap(),
        FixtureDriver {
            seed: seed(0),
            dispatch_count: dispatch_count.clone(),
            shutdown_count: shutdown_count.clone(),
            owner_lost: false,
        },
        ActorConfig::default(),
    )
    .unwrap();
    let attachment = owner.clone();
    drop(owner);

    let command = prompt_command(session_id, "lifecycle-command-once");
    let first = attachment.command(command.clone(), 10).await.unwrap();
    let duplicate = attachment.command(command, 99).await.unwrap();

    assert!(matches!(
        first.ack.disposition,
        AckDisposition::Accepted { .. }
    ));
    assert!(!first.cached);
    assert!(duplicate.cached);
    assert_eq!(duplicate.ack, first.ack);
    assert_eq!(dispatch_count.load(Ordering::SeqCst), 1);
    assert_eq!(shutdown_count.load(Ordering::SeqCst), 0);

    drop(attachment);
    wait_for_shutdown(&shutdown_count).await;
}

#[tokio::test]
async fn owner_loss_fences_actor_without_ack_cache_or_second_dispatch() {
    let dispatch_count = Arc::new(AtomicUsize::new(0));
    let shutdown_count = Arc::new(AtomicUsize::new(0));
    let session_id = seed(1).summary.id.clone();
    let actor = SessionActor::spawn(
        HostId::new("lifecycle-full-host").unwrap(),
        FixtureDriver {
            seed: seed(1),
            dispatch_count: dispatch_count.clone(),
            shutdown_count: shutdown_count.clone(),
            owner_lost: true,
        },
        ActorConfig::default(),
    )
    .unwrap();

    assert!(matches!(
        actor
            .command(prompt_command(session_id.clone(), "owner-lost-first"), 10)
            .await,
        Err(ActorError::Closed)
    ));
    wait_for_shutdown(&shutdown_count).await;
    assert!(actor.view().snapshot.items.is_empty());
    assert_eq!(dispatch_count.load(Ordering::SeqCst), 1);

    assert!(matches!(
        actor
            .command(prompt_command(session_id, "owner-lost-second"), 11)
            .await,
        Err(ActorError::Closed)
    ));
    assert_eq!(dispatch_count.load(Ordering::SeqCst), 1);
    assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_generation_is_rejected_before_dispatch_and_replay_is_cursor_bound() {
    let mut core = SessionActorCore::new(
        HostId::new("lifecycle-full-host").unwrap(),
        seed(2),
        ActorConfig {
            journal: JournalConfig {
                event_capacity: 2,
                byte_capacity: 64 * 1024,
            },
            ..ActorConfig::default()
        },
    )
    .unwrap();

    let mut stale = prompt_command(core.session_id().clone(), "stale-generation");
    stale.expected_actor_generation = Some(2);
    let admission = core
        .admit_command(stale, 10, |_| async {
            panic!("stale generations must not reach the driver")
        })
        .await
        .unwrap();
    let error = admission.ack.error().expect("stale command must reject");
    assert_eq!(error.code, octet_serve_backend::ErrorCode::StaleGeneration);
    assert_eq!(error.current_generation, Some(1));
    assert_eq!(core.snapshot().cursor.sequence, 0);

    for timestamp in 1..=3 {
        core.publish(TimestampedEvent::new(
            timestamp,
            EventPayload::UsageUpdated {
                usage: UsageSnapshot::default(),
            },
        ))
        .unwrap();
    }

    let ReplayResponse::Gap { gap, snapshot } = core.replay_after(SessionCursor::zero(1)) else {
        panic!("a cursor older than the bounded journal must return a gap");
    };
    assert_eq!(gap.earliest_available.sequence, 2);
    assert_eq!(gap.latest_available.sequence, 3);
    assert_eq!(snapshot.cursor.sequence, 3);

    let ReplayResponse::Events {
        events, through, ..
    } = core.replay_after(SessionCursor {
        actor_generation: 1,
        sequence: 1,
    })
    else {
        panic!("the retained cursor tail must replay directly");
    };
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].cursor.sequence, 2);
    assert_eq!(events[1].cursor.sequence, 3);
    assert_eq!(through.sequence, 3);
}
