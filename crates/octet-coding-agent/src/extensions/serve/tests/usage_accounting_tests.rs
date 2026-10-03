//! Durable provider-usage accounting as the serve host publishes it.
//! Covers both directions of the contract: a run must publish its usage, and the
//! unknown-usage case must publish uncertainty rather than an invented zero.
//! The startup backfill has to reconcile missing, corrupt, and archived sources
//! exactly once. Separate from run projection because the invariant is about the
//! accounting ledger, not about the transcript view.

use super::*;
use octet_ai::{AssistantMessage, Protocol, UserMessage};

use super::test_support::*;

#[tokio::test]
async fn unknown_usage_publishes_context_and_durable_accounting_before_completion() {
    let directory = tempfile::tempdir().unwrap();
    let plan = pull_request_worker_plan(directory.path(), "unknown-live");
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let run_id = RunId::new("unknown-live-run").unwrap();
    let mut projection = ProjectionState::new(7);
    let mut context = RunContextProjection::new(0, 0, 0);
    context.last_published = Some(ContextUsage::default());
    let (events, mut receiver) = mpsc::channel(8);
    for _ in 0..2 {
        assert!(project_agent_event(
            AgentEvent::ProviderUsageUncertain,
            &run_id,
            &plan,
            &model,
            &mut projection,
            &mut context,
            &events,
            &mut String::new()
        )
        .await
        .unwrap()
        .is_none());
        let event = receiver.try_recv().unwrap();
        let EventPayload::ContextUpdated { context } = event.payload else {
            panic!("expected live accounting, not a completed outcome");
        };
        assert!(context.usage_uncertain);
        let usage = plan.usage.lock().unwrap();
        assert!(usage.lifetime().usage_uncertain);
        assert_eq!(usage.lifetime().request_count, 0);
    }
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn unknown_usage_publishes_even_when_host_accounting_is_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let plan = pull_request_worker_plan(directory.path(), "unknown-unavailable");
    // The store latches failed persistence and rejects subsequent writes.
    assert!(plan.usage.lock().unwrap().record_uncertainty("").is_err());
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let mut projection = ProjectionState::new(0);
    let mut context = RunContextProjection::new(0, 0, 0);
    let (events, mut receiver) = mpsc::channel(8);
    assert!(project_agent_event(
        AgentEvent::ProviderUsageUncertain,
        &RunId::new("unknown-unavailable-run").unwrap(),
        &plan,
        &model,
        &mut projection,
        &mut context,
        &events,
        &mut String::new(),
    )
    .await
    .is_err());
    assert!(matches!(receiver.try_recv().unwrap().payload,
        EventPayload::ContextUpdated { context } if context.usage_uncertain));
    assert!(receiver.try_recv().is_err());
    assert!(projection.usage_uncertain);
    assert!(context.usage_uncertain);
    assert!(plan.usage.lock().unwrap().lifetime().usage_uncertain);
    assert_eq!(plan.usage.lock().unwrap().lifetime().request_count, 0);
}

#[tokio::test]
async fn idle_compaction_publishes_uncertainty_without_entries_or_fake_completion() {
    for succeeded in [false, true] {
        for unavailable in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut plan = pull_request_worker_plan(directory.path(), "idle-compact-unknown");
            plan.launch.model = ModelId("gpt-4o-mini".into());
            let mut app = build_worker_app(&mut plan).unwrap();
            app.agent
                .session_mut()
                .record_usage_uncertainty(
                    octet_ai::EndpointId("openai".into()),
                    ModelId("gpt-4o-mini".into()),
                    "compaction",
                )
                .unwrap();
            let mut projection = ProjectionState::new(app.agent.session().entries().len());
            if unavailable {
                assert!(plan.usage.lock().unwrap().record_uncertainty("").is_err());
            }
            let result = finish_idle_compaction(&app, &plan, &mut projection, succeeded);
            if succeeded && !unavailable {
                let SlashInvocationOutcome::Immediate(outcome) = result.unwrap() else {
                    panic!("idle compaction must not start a run");
                };
                assert!(outcome.events.is_empty());
            } else {
                assert!(result.is_err());
            }
            let (events, mut receiver) = mpsc::channel(8);
            publish_idle_accounting_context(&mut projection, &events)
                .await
                .unwrap();
            assert!(matches!(receiver.try_recv().unwrap().payload,
                EventPayload::ContextUpdated { context } if context.usage_uncertain));
            publish_idle_accounting_context(&mut projection, &events)
                .await
                .unwrap();
            assert!(receiver.try_recv().is_err());
            assert!(plan.usage.lock().unwrap().lifetime().usage_uncertain);
            assert_eq!(plan.usage.lock().unwrap().lifetime().request_count, 0);
        }
    }
}

#[test]
fn unknown_usage_rehydrates_idle_session_without_inventing_completion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unknown-idle.jsonl");
    let mut session = Session::create(&path).unwrap();
    let head = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("unfinished request".into())],
        })))
        .unwrap();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("openai".into()),
            ModelId("gpt-4o-mini".into()),
            "assistant_turn",
        )
        .unwrap();
    drop(session); // Crash before any completion/checkpoint/outcome record.
    let reopened = Session::open_read_only(&path).unwrap();
    let seed = seed_from_session(
        &reopened,
        SessionId::new("unknown-idle").unwrap(),
        SessionSeedOptions {
            workspace: directory.path(),
            project_id: None,
            model: ModelSelection {
                provider: "openai".into(),
                model: "gpt-4o-mini".into(),
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
    assert!(seed.snapshot.context.usage_uncertain);
    assert_eq!(seed.snapshot.live_state, SessionLiveState::Idle);
    assert!(seed.snapshot.active_run_id.is_none());
    assert_eq!(seed.snapshot.durable_head.unwrap().as_str(), head.0);
    assert_eq!(seed.snapshot.items.len(), 1);
    assert!(matches!(
        seed.snapshot.items[0].payload,
        ItemPayload::UserMessage { .. }
    ));
}

#[test]
fn accounting_backfill_distinguishes_missing_corrupt_and_archived_sources() {
    let directory = tempfile::tempdir().unwrap();
    let config = project_test_config(directory.path(), true);
    let state_dir = secure_serve_state_dir(&config.session_dir).unwrap();
    let mut projects = ProjectRegistry::open(state_dir.join("projects")).unwrap();
    let project = projects
        .import(config.workspace.canonicalize().unwrap(), None)
        .unwrap();
    projects
        .bind_session("backfill-source", &project.id)
        .unwrap();
    let root = projects.resolve_root(&project.id).unwrap();
    let sessions = SessionStore::new(&config.session_dir, root.as_path());
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let path = sessions.dir().join("backfill-source.jsonl");
    let mut missing = InferenceRequestStore::open(&state_dir).unwrap();
    backfill_usage_store(&config, &projects, &mut missing).unwrap();
    assert!(!missing.lifetime().usage_uncertain);
    assert!(missing.ensure_available().is_ok());
    drop(missing);

    std::fs::write(&path, b"{}\n").unwrap();
    let mut corrupt = InferenceRequestStore::open(&state_dir).unwrap();
    backfill_usage_store(&config, &projects, &mut corrupt).unwrap();
    assert!(corrupt.lifetime().usage_uncertain);
    assert!(corrupt.ensure_available().is_err());
    drop(corrupt);
    std::fs::remove_file(&path).unwrap();

    let mut session = Session::create(&path).unwrap();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("openai".into()),
            ModelId("gpt-4o-mini".into()),
            "assistant_turn",
        )
        .unwrap();
    drop(session);
    projects.archive(&project.id).unwrap();
    let mut archived = InferenceRequestStore::open(&state_dir).unwrap();
    backfill_usage_store(&config, &projects, &mut archived).unwrap();
    assert!(archived.lifetime().usage_uncertain);
    // This warning is now a successfully synced marker, not incomplete inspection.
    assert!(archived.ensure_available().is_ok());
    drop(archived);

    std::fs::rename(root.as_path(), directory.path().join("moved-workspace")).unwrap();
    let mut unavailable = InferenceRequestStore::open(&state_dir).unwrap();
    backfill_usage_store(&config, &projects, &mut unavailable).unwrap();
    assert!(unavailable.lifetime().usage_uncertain);
    assert!(unavailable.ensure_available().is_err());
}

#[tokio::test]
async fn host_backfills_durable_provider_usage_once_across_restarts() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = project_test_config(directory.path(), true);
    config.workspace = config.workspace.canonicalize().unwrap();
    config.invocation_cwd = config.workspace.clone();
    let sessions = SessionStore::new(&config.session_dir, &config.workspace);
    std::fs::create_dir_all(sessions.dir()).unwrap();
    let mut session = Session::create(sessions.dir().join("usage-backfill.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("measure usage".into())],
        })))
        .unwrap();
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
                cache_read_tokens: 10,
                cache_write_tokens: 5,
                cache_write_1h_tokens: 0,
                output_tokens: 20,
                reasoning_tokens: 4,
                total_tokens: 115,
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

    let host = OctetHost::new(config.clone()).unwrap();
    let lifetime = host.usage_lifetime().await.unwrap();
    assert!(lifetime.usage_uncertain);
    assert!(
        host.usage_stats(UsagePeriod::Daily)
            .await
            .unwrap()
            .usage_uncertain
    );
    assert_eq!(lifetime.prompt_tokens, 80);
    assert_eq!(lifetime.completion_tokens, 20);
    assert_eq!(lifetime.cache_read_tokens, 10);
    assert_eq!(lifetime.cache_write_tokens, 5);
    assert_eq!(lifetime.cache_write_1h_tokens, 0);
    assert_eq!(lifetime.reasoning_tokens, 4);
    assert_eq!(lifetime.total_tokens, 115);
    assert_eq!(lifetime.request_count, 1);
    assert_eq!(
        host.usage_stats(UsagePeriod::Daily)
            .await
            .unwrap()
            .request_count,
        1
    );
    drop(host);

    let reopened = OctetHost::new(config).unwrap();
    assert!(reopened.usage_lifetime().await.unwrap().usage_uncertain);
    assert_eq!(reopened.usage_lifetime().await.unwrap().request_count, 1);
    assert_eq!(reopened.usage_lifetime().await.unwrap().total_tokens, 115);
}
