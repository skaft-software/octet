//! Branching a transcript and unwinding the branch when the branch fails.
//! A guarded checkout has to leave the durable head and the reopened projection
//! exactly as they were whether it is rejected, seeded late, or quarantined by a
//! rollback that itself fails. The prepared-session descriptor lives here too,
//! because consuming it is what opens the checkout in the first place.

use super::*;
use octet_ai::{AssistantMessage, Protocol, UserMessage};
use octet_serve_backend::{
    AckDisposition, ActorConfig, ActorError, CommandId, DeviceId, SessionActorCore,
    SessionCommandEnvelope, SessionSupervisor, SupervisorConfig, SupervisorError,
};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use super::test_support::*;

struct TreeHook {
    veto: bool,
    fail_after: bool,
    seen: Arc<Mutex<Vec<octet_agent::compaction::SessionOperation>>>,
}

struct TreeReply(Option<octet_agent::compaction::SessionOperationDecision>);

impl octet_agent::compaction::SessionOperationInvocation for TreeReply {
    fn take_future(&mut self) -> octet_agent::compaction::SessionOperationFuture {
        let decision = self.0.take().unwrap();
        Box::pin(async move { Ok(decision) })
    }
}

impl octet_agent::compaction::SessionOperationHook for TreeHook {
    fn begin(
        &self,
        session: &Session,
        operation: &octet_agent::compaction::SessionOperation,
    ) -> Result<Option<Box<dyn octet_agent::compaction::SessionOperationInvocation>>, String> {
        use octet_agent::compaction::{SessionOperation, SessionOperationDecision};
        self.seen.lock().unwrap().push(operation.clone());
        let decision = match operation {
            SessionOperation::BeforeTree { .. } if self.veto => SessionOperationDecision::Cancel,
            SessionOperation::Tree { new_head, .. } => {
                assert_eq!(&session.head(), new_head);
                assert_eq!(
                    Session::open_read_only(session.path()).unwrap().head(),
                    *new_head
                );
                if self.fail_after {
                    return Err("post-checkout failure".into());
                }
                SessionOperationDecision::Continue
            }
            _ => SessionOperationDecision::Continue,
        };
        Ok(Some(Box::new(TreeReply(Some(decision)))))
    }
}

#[tokio::test]
async fn live_checkout_boundary_honors_veto_and_never_rolls_back_failed_after_hook() {
    for (veto, fail_after) in [(true, false), (false, false), (false, true)] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (_directory, mut app) = crate::compaction::tests::app_for_session_operation(TreeHook {
            veto,
            fail_after,
            seen: seen.clone(),
        });
        let target = app
            .agent
            .session_mut()
            .append(EntryValue::Config {
                model: Some("gpt-4o-mini".into()),
                reasoning: None,
                reasoning_mode: None,
            })
            .unwrap();
        let old_head = app
            .agent
            .session_mut()
            .append(EntryValue::Config {
                model: Some("gpt-4o".into()),
                reasoning: None,
                reasoning_mode: None,
            })
            .unwrap();
        let result = super::runs::navigate_checkout(&mut app.agent, target.clone()).await;
        if veto {
            assert_eq!(result, Err(ServiceError::InvalidBoundary));
            assert_eq!(app.agent.session().head(), Some(old_head.clone()));
            assert_eq!(seen.lock().unwrap().len(), 1);
        } else {
            assert_eq!(
                result,
                if fail_after {
                    Err(ServiceError::OwnerLost)
                } else {
                    Ok(())
                }
            );
            assert_eq!(app.agent.session().head(), Some(target.clone()));
            let events = seen.lock().unwrap();
            assert_eq!(events.len(), 2);
            assert!(
                matches!(&events[1], octet_agent::compaction::SessionOperation::Tree {
                old_head: Some(old), new_head: Some(new),
            } if old == &old_head && new == &target)
            );
        }
        let path = app.agent.session().path().to_owned();
        drop(app);
        assert_eq!(
            Session::open_read_only(path).unwrap().head(),
            Some(if veto { old_head } else { target })
        );
    }
}

#[test]
fn prepared_session_descriptor_is_consumed_once_and_checkout_rebuild_reopens_path() {
    let directory = tempfile::tempdir().unwrap();
    let mut plan = pull_request_worker_plan(directory.path(), "prepared-descriptor");
    let SessionSelection::CreateNew(path) = plan.launch.session.clone() else {
        panic!("test worker plan must create a session");
    };
    plan.launch.model = ModelId("gpt-4o-mini".into());
    let mut original = Session::create(&path).unwrap();
    original
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("authorized transcript".into())],
        })))
        .unwrap();
    drop(original);

    let file = octet_agent::secure_fs::open_regular_file_for_append(&path).unwrap();
    let prepared = Session::open_with_file(path.clone(), file).unwrap();
    plan.launch.session = SessionSelection::OpenExisting(path.clone());
    *plan.prepared_session.get_mut().unwrap() = Some(prepared);

    // Simulate a pathname replacement after descriptor-bound authorization.
    // The initial worker must keep the authorized descriptor. A checkout
    // rebuild deliberately reopens the current pathname through the normal
    // descriptor-bound path instead of retaining an unsafe broad cache.
    let displaced = path.with_extension("displaced");
    std::fs::rename(&path, &displaced).unwrap();
    let mut replacement = Session::create(&path).unwrap();
    replacement
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("replacement transcript".into())],
        })))
        .unwrap();
    drop(replacement);

    let app = build_worker_app(&mut plan).unwrap();
    assert!(app.agent.session().entries().iter().any(|entry| {
        matches!(
            &entry.value,
            EntryValue::Message(Message::User(UserMessage { content }))
                if matches!(content.as_slice(), [UserPart::Text(text)] if text == "authorized transcript")
        )
    }));
    assert!(plan.prepared_session.get_mut().unwrap().is_none());

    let rebuilt = rebuild_worker_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::OpenExisting(path)),
    )
    .unwrap();
    assert!(rebuilt.agent.session().entries().iter().any(|entry| {
        matches!(
            &entry.value,
            EntryValue::Message(Message::User(UserMessage { content }))
                if matches!(content.as_slice(), [UserPart::Text(text)] if text == "replacement transcript")
        )
    }));
    assert!(rebuilt.agent.session().entries().iter().all(|entry| {
        !matches!(
            &entry.value,
            EntryValue::Message(Message::User(UserMessage { content }))
                if matches!(content.as_slice(), [UserPart::Text(text)] if text == "authorized transcript")
        )
    }));
}

fn checkout_envelope(
    host: &OctetHost,
    session_id: &SessionId,
    generation: u64,
    command_id: &str,
    target: DurableEntryId,
) -> SessionCommandEnvelope {
    SessionCommandEnvelope::new(
        host.descriptor.id.clone(),
        DeviceId::new("device-worker-test").unwrap(),
        session_id.clone(),
        CommandId::new(command_id).unwrap(),
        1,
        Some(generation),
        SessionCommand::Checkout { entry_id: target },
    )
}

#[tokio::test]
async fn rejected_guarded_checkout_restores_durable_head_and_reopened_projection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("checkout-rollback.jsonl");
    let session_id = SessionId::new("checkout-rollback").unwrap();
    let model = ModelSelection {
        provider: "test".into(),
        model: "test-model".into(),
        reasoning: "off".into(),
    };
    let mut session = Session::create(&path).unwrap();
    let root = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("question".into())],
        })))
        .unwrap();
    let previous_head = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("answer".into())],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    let actor_seed = seed_from_session(
        &session,
        session_id.clone(),
        SessionSeedOptions {
            workspace: directory.path(),
            project_id: None,
            model: model.clone(),
            authority: AuthorityProfile::FullAccess,
            generation: 1,
            meta: None,
            attachment_store: None,
            resource_store: None,
        },
    )
    .unwrap();
    session.checkout(root.clone()).unwrap();
    drop(session);

    let mut invalid_replacement = actor_seed.clone();
    let wrong_session = SessionId::new("checkout-rollback-wrong").unwrap();
    invalid_replacement.summary.id = wrong_session.clone();
    invalid_replacement.snapshot.session_id = wrong_session;
    let (outcome, mut finalizer) = DriverCommandOutcome::guarded_replace(invalid_replacement);
    let restored_before_rejection = Arc::new(AtomicBool::new(false));
    let worker_restored = Arc::clone(&restored_before_rejection);
    let worker_path = path.clone();
    let worker_head = previous_head.clone();
    let worker = tokio::spawn(async move {
        assert_eq!(
            finalizer.decision().await.unwrap(),
            FinalizeDecision::Rollback
        );
        restore_session_head(&worker_path, worker_head.clone()).unwrap();
        let reopened = Session::open_read_only(&worker_path).unwrap();
        assert_eq!(reopened.head(), Some(worker_head));
        worker_restored.store(true, AtomicOrdering::Release);
        finalizer
            .complete(Ok(FinalizeCompletion::RolledBack))
            .unwrap();
    });

    let host_id = HostId::new("host-test").unwrap();
    let mut actor =
        SessionActorCore::new(host_id.clone(), actor_seed.clone(), ActorConfig::default()).unwrap();
    let command = SessionCommandEnvelope::new(
        host_id,
        DeviceId::new("device-test").unwrap(),
        session_id.clone(),
        CommandId::new("command-checkout-rollback").unwrap(),
        1,
        Some(1),
        SessionCommand::Checkout {
            entry_id: DurableEntryId::new(root.0).unwrap(),
        },
    );
    let admission = actor
        .admit_command(command, 10, |_| async { Ok(outcome) })
        .await
        .unwrap();
    assert!(matches!(
        admission.ack.disposition,
        AckDisposition::Rejected { .. }
    ));
    assert!(restored_before_rejection.load(AtomicOrdering::Acquire));
    assert_eq!(actor.snapshot(), actor_seed.snapshot);
    worker.await.unwrap();

    let reopened = Session::open_read_only(&path).unwrap();
    assert_eq!(reopened.head(), Some(previous_head.clone()));
    let restored = seed_from_session(
        &reopened,
        session_id,
        SessionSeedOptions {
            workspace: directory.path(),
            project_id: None,
            model,
            authority: AuthorityProfile::FullAccess,
            generation: 1,
            meta: None,
            attachment_store: None,
            resource_store: None,
        },
    )
    .unwrap();
    assert_eq!(
        restored.snapshot.durable_head,
        Some(DurableEntryId::new(previous_head.0).unwrap())
    );
    assert!(restored.snapshot.items.iter().any(|item| {
        matches!(
            &item.payload,
            ItemPayload::AssistantMessage { text } if text == "answer"
        )
    }));
}

#[tokio::test]
async fn checkout_does_not_wait_for_pull_request_store_persistence() {
    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, _old_head, target, _path) =
        worker_checkout_fixture(directory.path(), "worker-pr-projection");
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
    let host = Arc::new(host);
    let supervisor = Arc::new(SessionSupervisor::new(
        Arc::clone(&host),
        SupervisorConfig::default(),
    ));
    let handle = supervisor.open_session(&session_id).await.unwrap();
    assert_eq!(
        handle.view().summary.pull_request,
        Some(PullRequestSummary {
            state: PullRequestState::Ready,
        })
    );

    let store_locked = Arc::new(std::sync::Barrier::new(2));
    let release_store = Arc::new(std::sync::Barrier::new(2));
    let pull_requests = Arc::clone(&host.pull_requests);
    let holder = {
        let store_locked = Arc::clone(&store_locked);
        let release_store = Arc::clone(&release_store);
        std::thread::spawn(move || {
            let _store = pull_requests.lock().unwrap();
            store_locked.wait();
            release_store.wait();
        })
    };
    store_locked.wait();

    let envelope = checkout_envelope(
        &host,
        &session_id,
        handle.view().snapshot.actor_generation,
        "command-worker-pr-projection",
        target,
    );
    let command = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.command(envelope, 10).await })
    };
    let admission = tokio::time::timeout(std::time::Duration::from_secs(2), command).await;
    release_store.wait();
    holder.join().unwrap();
    let admission = admission
        .expect("checkout must not contend with pull-request persistence")
        .unwrap()
        .unwrap();
    assert!(matches!(
        admission.ack.disposition,
        AckDisposition::Accepted { .. }
    ));
    assert_eq!(
        handle.view().summary.pull_request,
        Some(PullRequestSummary {
            state: PullRequestState::Ready,
        })
    );
}

#[tokio::test]
async fn production_worker_quarantines_reopen_until_late_rollback_settles() {
    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, old_head, target, path) =
        worker_checkout_fixture(directory.path(), "worker-quarantine");
    let gate = CheckoutRollbackGate {
        entered: Arc::new(tokio::sync::Barrier::new(2)),
        release: Arc::new(tokio::sync::Barrier::new(2)),
    };
    host.checkout_hooks
        .lock()
        .unwrap()
        .push_back(CheckoutTestHooks {
            rollback_gate: Some(gate.clone()),
            corrupt_replacement_identity: true,
            ..CheckoutTestHooks::default()
        });
    let host = Arc::new(host);
    let supervisor = Arc::new(SessionSupervisor::new(
        Arc::clone(&host),
        SupervisorConfig {
            actor: ActorConfig {
                finalize_timeout: std::time::Duration::from_millis(20),
                ..ActorConfig::default()
            },
            ..SupervisorConfig::default()
        },
    ));
    let original = supervisor.open_session(&session_id).await.unwrap();
    assert_eq!(host.open_count.load(AtomicOrdering::Relaxed), 1);
    assert_eq!(
        original.view().snapshot.durable_head,
        Some(old_head.clone())
    );
    let envelope = checkout_envelope(
        &host,
        &session_id,
        original.view().snapshot.actor_generation,
        "command-worker-quarantine",
        target,
    );
    let command = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.command(envelope, 10).await })
    };
    gate.entered.wait().await;
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), command)
            .await
            .expect("actor finalization timeout")
            .unwrap(),
        Err(SupervisorError::Actor(ActorError::Closed))
    ));

    let mut first_reopen = {
        let supervisor = Arc::clone(&supervisor);
        let session_id = session_id.clone();
        tokio::spawn(async move { supervisor.open_session(&session_id).await })
    };
    let mut second_reopen = {
        let supervisor = Arc::clone(&supervisor);
        let session_id = session_id.clone();
        tokio::spawn(async move { supervisor.open_session(&session_id).await })
    };
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(60), &mut first_reopen,)
            .await
            .is_err(),
        "the old durable writer must fence reopen"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(60), &mut second_reopen,)
            .await
            .is_err(),
        "concurrent reopen must join the same ownership quarantine"
    );
    assert_eq!(host.open_count.load(AtomicOrdering::Relaxed), 1);

    gate.release.wait().await;
    let (first_reopen, second_reopen) = tokio::join!(first_reopen, second_reopen);
    let first_reopen = first_reopen.unwrap().unwrap();
    let second_reopen = second_reopen.unwrap().unwrap();
    assert_eq!(host.open_count.load(AtomicOrdering::Relaxed), 2);
    assert_eq!(
        first_reopen.view().snapshot.durable_head,
        Some(old_head.clone())
    );
    assert_eq!(
        second_reopen.view().snapshot.durable_head,
        Some(old_head.clone())
    );
    assert_eq!(
        Session::open_read_only(path).unwrap().head(),
        Some(EntryId(old_head.as_str().to_owned()))
    );
}

#[tokio::test]
async fn production_worker_rolls_back_injected_seed_failure_before_rejection() {
    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, old_head, target, path) =
        worker_checkout_fixture(directory.path(), "worker-seed-rollback");
    host.checkout_hooks
        .lock()
        .unwrap()
        .push_back(CheckoutTestHooks {
            fail_seed_after_checkout: true,
            ..CheckoutTestHooks::default()
        });
    let host = Arc::new(host);
    let supervisor = SessionSupervisor::new(Arc::clone(&host), SupervisorConfig::default());
    let handle = supervisor.open_session(&session_id).await.unwrap();
    let envelope = checkout_envelope(
        &host,
        &session_id,
        handle.view().snapshot.actor_generation,
        "command-worker-seed-rollback",
        target,
    );
    let admission = supervisor.command(envelope, 10).await.unwrap();
    assert!(matches!(
        admission.ack.disposition,
        AckDisposition::Rejected { .. }
    ));
    assert_eq!(handle.view().snapshot.durable_head, Some(old_head.clone()));
    assert_eq!(
        Session::open_read_only(path).unwrap().head(),
        Some(EntryId(old_head.as_str().to_owned()))
    );
}

#[tokio::test]
async fn production_worker_rollback_failure_retires_owner_without_ack() {
    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, old_head, target, path) =
        worker_checkout_fixture(directory.path(), "worker-rollback-loss");
    host.checkout_hooks
        .lock()
        .unwrap()
        .push_back(CheckoutTestHooks {
            fail_seed_after_checkout: true,
            fail_rollback: true,
            ..CheckoutTestHooks::default()
        });
    let host = Arc::new(host);
    let supervisor = SessionSupervisor::new(Arc::clone(&host), SupervisorConfig::default());
    let handle = supervisor.open_session(&session_id).await.unwrap();
    let envelope = checkout_envelope(
        &host,
        &session_id,
        handle.view().snapshot.actor_generation,
        "command-worker-rollback-loss",
        target.clone(),
    );
    assert!(matches!(
        supervisor.command(envelope, 10).await,
        Err(SupervisorError::Actor(ActorError::Closed))
    ));

    let reopened = supervisor.open_session(&session_id).await.unwrap();
    let final_disk_head = Session::open_read_only(path).unwrap().head().unwrap();
    let final_durable_head = DurableEntryId::new(final_disk_head.0).unwrap();
    assert_eq!(host.open_count.load(AtomicOrdering::Relaxed), 2);
    assert_eq!(
        reopened.view().snapshot.durable_head,
        Some(final_durable_head.clone())
    );
    assert_ne!(final_durable_head, old_head);
}

#[test]
fn failed_pre_guard_checkout_rollback_is_fatal_owner_loss() {
    let directory = tempfile::tempdir().unwrap();
    let rollback = restore_session_head(
        &directory.path().join("missing-session.jsonl"),
        EntryId("previous-head".into()),
    );
    assert_eq!(
        checkout_rejection_after_rollback(rollback, ServiceError::InvalidSeed),
        Err(ServiceError::OwnerLost)
    );
}
