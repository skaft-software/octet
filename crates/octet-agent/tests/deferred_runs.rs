//! Durable suspended/effect-pending deferred-run lifecycle (row 4.12) and the
//! typed deferred stop reason consumer half (row 1e.1).
//!
//! Every case drives real session JSONL persistence and the public agent/store
//! surface: a parked run is one owner's generation-fenced permit, a crash
//! between "poll admitted" and "outcome known" is replaced under fresh reserved
//! ids, and a cancelled, settled, expired, or foreign poll never reaches the
//! provider.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use octet_agent::agent::{
    AiDeferredPollSource, DeferredPollReply, DeferredPollSource, DeferredResumeIntent,
    DeferredRunOutcome,
};
use octet_agent::events::AgentEvent;
use octet_agent::tools::deferred::{
    DeferredHandle, DeferredPhase, DeferredPollCompletion, DeferredPollOutcome,
    DeferredPollRefusalKind, DeferredResponseDeclaration, DeferredResumeStart, DeferredRunError,
    DeferredRunRecord, DeferredRunState, DeferredRunStore, DeferredStopReason,
    DeferredSuspendDecision, ModelIdentity, INVALID_DEFERRED_HANDLE_DIAGNOSTIC,
};
use octet_agent::{
    Agent, AgentConfig, AgentError, EffectBroker, ExtensionHost, FinishReason, SandboxConfig,
    Session,
};
use octet_ai::{
    AiClient, AiError, AssistantMessage, DeferredHandle as CodecDeferredHandle, Diagnostic,
    HostStreamModel, HostStreamTransport, Model, ModelCatalog, ModelId, Request, Response,
    ResponseStream, StopReason, StreamEvent, Usage,
};

fn test_model() -> Model {
    ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap()
}

fn identity_for(model: &Model) -> ModelIdentity {
    ModelIdentity::new(model.endpoint.id.0.clone(), model.spec.id.0.clone())
}

fn local_handle(model: &Model, id: impl Into<String>) -> DeferredHandle {
    DeferredHandle::new(
        model.endpoint.id.0.clone(),
        model.spec.id.0.clone(),
        "test-api",
        id,
    )
}

fn codec_handle(model: &Model, id: &str) -> CodecDeferredHandle {
    CodecDeferredHandle::new(
        model.endpoint.id.0.clone(),
        model.spec.id.0.clone(),
        "test-api",
        id,
    )
}

fn parked_response(model: &Model, handle: CodecDeferredHandle) -> Response {
    Response {
        message: AssistantMessage {
            content: Vec::new(),
            model: model.spec.id.clone(),
            protocol: model.spec.protocol,
        },
        stop_reason: StopReason::Deferred,
        usage: Usage::default(),
        cost: None,
        response_id: None,
        responses_output: None,
        deferred: Some(handle),
        diagnostics: Vec::new(),
    }
}

fn settled_response(model: &Model) -> Response {
    Response {
        message: AssistantMessage {
            content: vec![octet_ai::AssistantPart::Text("settled".into())],
            model: model.spec.id.clone(),
            protocol: model.spec.protocol,
        },
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 7,
            output_tokens: 3,
            total_tokens: 10,
            ..Usage::default()
        },
        cost: None,
        response_id: Some("polled-response".into()),
        responses_output: None,
        deferred: None,
        diagnostics: Vec::new(),
    }
}

fn response_stream(response: Response) -> ResponseStream {
    Box::pin(futures_util::stream::iter([
        Ok(StreamEvent::Started { response_id: None }),
        Ok(StreamEvent::Finished(response)),
    ]))
}

/// A host transport that parks the first request and answers polls from a
/// scripted queue of handles; every provider call is counted.
struct ParkedTransport {
    model: Model,
    handle: Mutex<CodecDeferredHandle>,
    stream_calls: AtomicUsize,
    fetch_calls: AtomicUsize,
    still_deferred: Mutex<VecDeque<String>>,
}

impl ParkedTransport {
    fn new(model: &Model, handle_id: &str) -> Self {
        Self {
            model: model.clone(),
            handle: Mutex::new(codec_handle(model, handle_id)),
            stream_calls: AtomicUsize::new(0),
            fetch_calls: AtomicUsize::new(0),
            still_deferred: Mutex::new(VecDeque::new()),
        }
    }
}

#[async_trait::async_trait]
impl HostStreamTransport for ParkedTransport {
    async fn stream(
        &self,
        _model: HostStreamModel,
        _request: Request,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        let handle = self.handle.lock().unwrap().clone();
        Ok(response_stream(parked_response(&self.model, handle)))
    }

    async fn fetch_deferred(
        &self,
        _model: HostStreamModel,
        handle: CodecDeferredHandle,
        _wait_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(handle.id, self.handle.lock().unwrap().id);
        if let Some(next) = self.still_deferred.lock().unwrap().pop_front() {
            let next = CodecDeferredHandle::new(
                handle.provider.clone(),
                handle.model_id.clone(),
                handle.api.clone(),
                next,
            );
            return Ok(response_stream(parked_response(&self.model, next)));
        }
        Ok(response_stream(settled_response(&self.model)))
    }
}

fn parked_agent(
    model: Model,
    transport: Arc<ParkedTransport>,
) -> (Agent, AiClient, tempfile::TempDir) {
    let client = AiClient::new();
    client.register_host_stream_transport(model.endpoint.id.clone(), transport);
    let workspace = tempfile::tempdir().unwrap();
    let agent = Agent::new(AgentConfig {
        client: client.clone(),
        model,
        session: Session::create(workspace.path().join("deferred.jsonl")).unwrap(),
        system: "system".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: Some(4),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    (agent, client, workspace)
}

async fn drive_park(agent: &mut Agent) -> (String, u64, u64) {
    let mut run = agent.prompt_without_tools("park this request").await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    drop(run);
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::RunFinished {
                reason:
                    FinishReason::Failed(AgentError::DeferredSuspended {
                        operation_id,
                        poll,
                        generation,
                    }),
                ..
            } => Some((operation_id.clone(), *poll, *generation)),
            _ => None,
        })
        .expect("the run must report a durable deferred suspension")
}

fn suspend_in_store(
    store: &DeferredRunStore,
    identity: &ModelIdentity,
    model: &Model,
    operation_id: &str,
    source_entry_id: &str,
) {
    let decision = store
        .suspend(
            identity,
            operation_id,
            source_entry_id,
            DeferredResponseDeclaration {
                stop_reason: DeferredStopReason::Deferred,
                api: "test-api".to_owned(),
                handle: Some(local_handle(model, format!("{operation_id}-handle"))),
            },
        )
        .unwrap();
    assert!(matches!(decision, DeferredSuspendDecision::Suspended(_)));
}

fn reserved_ids(
    preparation: &DeferredResumeStart,
) -> (String, String, Option<(String, String)>) {
    let DeferredResumeStart::Admitted(admitted) = preparation else {
        panic!("expected an admitted poll, got {preparation:?}")
    };
    let (response_id, usage_id) = match &admitted.intent.phase {
        DeferredPhase::EffectPending {
            response_id,
            usage_id,
        } => (response_id.clone(), usage_id.clone()),
        other => panic!("an admitted poll is effect pending, got {other:?}"),
    };
    let discarded = admitted.intent.discard_unknown_poll.as_ref().map(|replaced| {
        (
            replaced.abandoned_response_id.clone(),
            replaced.abandoned_usage_id.clone(),
        )
    });
    (response_id, usage_id, discarded)
}

#[tokio::test]
async fn a_deferred_provider_response_parks_the_run_and_one_permitted_poll_settles_it() {
    let model = test_model();
    let transport = Arc::new(ParkedTransport::new(&model, "parked-1"));
    let (mut agent, client, _workspace) = parked_agent(model.clone(), Arc::clone(&transport));

    let (operation_id, poll, generation) = drive_park(&mut agent).await;
    assert_eq!((poll, generation), (0, 0));
    assert_eq!(transport.stream_calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.fetch_calls.load(Ordering::SeqCst), 0);

    let parked = agent.parked_deferred_runs();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].operation_id, operation_id);
    assert_eq!(parked[0].state_label(), "suspended");
    assert_eq!(parked[0].generation, 0);

    let source = AiDeferredPollSource::new(client, model.clone());
    let outcome = agent
        .resume_deferred_run(&operation_id, "pass-1", DeferredResumeIntent::Poll, &source)
        .await
        .unwrap();
    match outcome {
        DeferredRunOutcome::Settled {
            response,
            response_id,
            usage_id,
        } => {
            assert_eq!(response.stop_reason, StopReason::EndTurn);
            // The ids the outcome reports are the reserved commit slots the
            // durable tombstone actually settled, not a second reservation.
            match &agent.deferred_run(&operation_id).unwrap().state {
                DeferredRunState::Settled {
                    response_id: durable_response,
                    usage_id: durable_usage,
                } => assert_eq!((durable_response, durable_usage), (&response_id, &usage_id)),
                other => panic!("a settled poll must leave a settled tombstone, got {other:?}"),
            }
            assert!(!response_id.is_empty());
            assert!(!usage_id.is_empty());
        }
        other => panic!("expected a settled poll, got {other:?}"),
    }
    assert_eq!(transport.fetch_calls.load(Ordering::SeqCst), 1);
    assert_eq!(agent.deferred_run(&operation_id).unwrap().state_label(), "settled");
    assert!(!agent.session().has_uncertain_usage());

    // The durable tombstone makes a second poll impossible: the run is terminal
    // and no provider work happens again.
    let again = agent
        .resume_deferred_run(&operation_id, "pass-2", DeferredResumeIntent::Poll, &source)
        .await
        .unwrap();
    assert!(matches!(
        again,
        DeferredRunOutcome::Finished {
            state: "settled",
            ..
        }
    ));
    assert_eq!(transport.fetch_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_still_deferred_poll_keeps_the_poll_number_and_bumps_the_generation() {
    let model = test_model();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store.jsonl");
    let session = Session::create(&path).unwrap();
    let store = session.deferred_run_store();
    let identity = identity_for(&model);
    suspend_in_store(&store, &identity, &model, "op-1", "entry-1");

    let observe = store
        .begin_pass("op-1", "observe", DeferredResumeIntent::Observe, 0)
        .unwrap();
    assert!(matches!(observe, DeferredResumeStart::Waiting(_)));
    assert_eq!(store.record("op-1").unwrap().generation, 0);

    let first = store
        .begin_pass("op-1", "pass-1", DeferredResumeIntent::Poll, 0)
        .unwrap();
    let (first_response, first_usage, discarded) = reserved_ids(&first);
    assert!(discarded.is_none());
    assert_eq!(store.record("op-1").unwrap().generation, 1);

    let completion = store
        .complete_pass(
            match &first {
                DeferredResumeStart::Admitted(admitted) => admitted,
                _ => unreachable!(),
            },
            DeferredPollOutcome::StillDeferred(local_handle(&model, "parked-2")),
        )
        .unwrap();
    match completion {
        DeferredPollCompletion::Suspended(observation) => assert_eq!(observation.poll, 1),
        other => panic!("expected a re-suspended leaf, got {other:?}"),
    }
    let record = store.record("op-1").unwrap();
    assert_eq!(record.generation, 2);
    assert_eq!(record.state_label(), "suspended");
    assert_eq!(record.leaf().unwrap().poll, 1);

    // One permit settles one generation: replaying the completed pass against
    // the moved leaf is fenced, so the same billable poll can never be merged
    // into the run a second time.
    assert!(matches!(
        store.complete_pass(
            match &first {
                DeferredResumeStart::Admitted(admitted) => admitted,
                _ => unreachable!(),
            },
            DeferredPollOutcome::Settled,
        ),
        Err(DeferredRunError::StaleGeneration { .. })
    ));
    assert_eq!(store.record("op-1").unwrap().generation, 2);

    // A replacement pass at the bumped generation reserves fresh ids, so the
    // abandoned pair is never reused.
    let second = store
        .begin_pass("op-1", "pass-2", DeferredResumeIntent::Poll, 0)
        .unwrap();
    let (second_response, second_usage, discarded) = reserved_ids(&second);
    assert!(discarded.is_none(), "a suspended leaf has no abandoned poll");
    assert_ne!(
        (first_response, first_usage),
        (second_response, second_usage)
    );

    // The permit was minted for generation 1 and the leaf moved to 2, so a
    // stale cancellation is fenced rather than silently admitted. Cancelling
    // the admitted generation 3 leaf reports its unknown-outcome poll.
    assert!(matches!(
        store.cancel("op-1", 1),
        Err(DeferredRunError::StaleGeneration { .. })
    ));
    let cancelled = store.cancel("op-1", 3).unwrap();
    assert!(cancelled.abandoned_unknown_poll());
    assert!(matches!(
        store.begin_pass("op-1", "pass-3", DeferredResumeIntent::Poll, 0),
        Ok(DeferredResumeStart::Finished(record)) if record.state_label() == "cancelled"
    ));
    assert!(matches!(
        store.cancel("op-1", 4),
        Err(DeferredRunError::OutcomeKnown(_))
    ));
}

#[tokio::test]
async fn an_admitted_poll_replays_as_effect_pending_and_is_replaced_with_fresh_ids() {
    let model = test_model();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("crash.jsonl");
    let session = Session::create(&path).unwrap();
    let store = session.deferred_run_store();
    let identity = identity_for(&model);
    suspend_in_store(&store, &identity, &model, "run:1:deferred:1", "entry-1");

    let admitted = store
        .begin_pass("run:1:deferred:1", "pass-a", DeferredResumeIntent::Poll, 0)
        .unwrap();
    let (abandoned_response, abandoned_usage, _) = reserved_ids(&admitted);
    // Simulate a crash between the durable intent and any outcome.
    drop(session);

    let reopened = Session::open(&path).unwrap();
    let store = reopened.deferred_run_store();
    let replayed: DeferredRunRecord = store.record("run:1:deferred:1").unwrap();
    assert_eq!(replayed.state_label(), "effect_pending");
    let DeferredRunState::EffectPending { leaf } = &replayed.state else {
        panic!("the effect-pending leaf must replay")
    };
    assert_eq!(
        match &leaf.phase {
            DeferredPhase::EffectPending {
                response_id,
                usage_id,
            } => (response_id.clone(), usage_id.clone()),
            other => panic!("replayed phase must stay effect pending, got {other:?}"),
        },
        (abandoned_response.clone(), abandoned_usage.clone())
    );

    // A plain permitted pass may not spend a second billable poll on an
    // unknown outcome: the leaf stays parked and the refusal writes nothing.
    let refused = store
        .begin_pass(
            "run:1:deferred:1",
            "pass-b",
            DeferredResumeIntent::Poll,
            0,
        )
        .unwrap();
    match refused {
        DeferredResumeStart::Refused(refusal) => {
            assert_eq!(
                refusal.kind,
                DeferredPollRefusalKind::UnknownPollOutcome { poll: 1 }
            );
        }
        other => panic!("a plain poll must not auto-replace an unknown outcome, got {other:?}"),
    }
    assert_eq!(
        store.record("run:1:deferred:1").unwrap().state_label(),
        "effect_pending",
        "a refused replacement must leave the unknown-outcome leaf durable"
    );

    // Only the explicit replacement intent admits the recovery poll, under a
    // fresh permit at the same poll number.
    let replacement = store
        .begin_pass(
            "run:1:deferred:1",
            "pass-c",
            DeferredResumeIntent::ReplaceUnknownPoll,
            0,
        )
        .unwrap();
    let (replacement_response, replacement_usage, discarded) = reserved_ids(&replacement);
    assert_eq!(
        discarded,
        Some((abandoned_response.clone(), abandoned_usage.clone())),
        "the replacement must report the abandoned reservation"
    );
    assert_ne!(replacement_response, abandoned_response);
    assert_ne!(replacement_usage, abandoned_usage);
    let DeferredResumeStart::Admitted(replacement) = replacement else {
        panic!("recovery must be admitted under a fresh permit")
    };
    let completion = store
        .complete_pass(&replacement, DeferredPollOutcome::Settled)
        .unwrap();
    assert!(matches!(completion, DeferredPollCompletion::Settled { .. }));
    assert_eq!(
        store.record("run:1:deferred:1").unwrap().state_label(),
        "settled"
    );
}

#[tokio::test]
async fn an_expired_or_foreign_handle_is_refused_without_provider_work() {
    let model = test_model();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("refusals.jsonl");
    let session = Session::create(&path).unwrap();
    let store = session.deferred_run_store();
    let identity = identity_for(&model);

    // Expired: the leaf stays parked and a refused poll writes nothing.
    let mut expired = local_handle(&model, "expired-1");
    expired.expires_at_ms = Some(10);
    let _ = store.suspend(
        &identity,
        "op-expired",
        "entry-1",
        DeferredResponseDeclaration {
            stop_reason: DeferredStopReason::Deferred,
            api: "test-api".to_owned(),
            handle: Some(expired),
        },
    );
    let refusal = store
        .begin_pass("op-expired", "pass-x", DeferredResumeIntent::Poll, 100)
        .unwrap();
    assert!(matches!(
        refusal,
        DeferredResumeStart::Refused(refusal)
            if matches!(refusal.kind, DeferredPollRefusalKind::ExpiredHandle { .. })
    ));
    assert_eq!(store.record("op-expired").unwrap().generation, 0);

    // A still-deferred poll that switches identity fails closed instead of
    // parking on an untrusted handle.
    suspend_in_store(&store, &identity, &model, "op-foreign", "entry-2");
    let DeferredResumeStart::Admitted(admitted) = store
        .begin_pass("op-foreign", "pass-y", DeferredResumeIntent::Poll, 0)
        .unwrap()
    else {
        panic!("the pass must be admitted")
    };
    let completion = store
        .complete_pass(
            &admitted,
            DeferredPollOutcome::StillDeferred(DeferredHandle::new(
                "other-provider",
                model.spec.id.0.clone(),
                "test-api",
                "foreign-1",
            )),
        )
        .unwrap();
    match completion {
        DeferredPollCompletion::Failed(failure) => assert!(failure
            .diagnostic
            .starts_with(INVALID_DEFERRED_HANDLE_DIAGNOSTIC)),
        other => panic!("a foreign still-deferred handle must fail closed, got {other:?}"),
    }
    assert_eq!(store.record("op-foreign").unwrap().state_label(), "failed");

    // An invalid handle at suspend time never parks a run.
    let decision = store
        .suspend(
            &identity,
            "op-invalid",
            "entry-3",
            DeferredResponseDeclaration {
                stop_reason: DeferredStopReason::Deferred,
                api: "test-api".to_owned(),
                handle: None,
            },
        )
        .unwrap();
    assert!(matches!(decision, DeferredSuspendDecision::Failed(_)));
    assert!(store.record("op-invalid").is_none());
}

/// A scripted poll source that answers each admitted poll in order and counts
/// provider calls, so a duplicate poll is observable.
struct ScriptedPollSource {
    replies: Mutex<VecDeque<DeferredPollReply>>,
    polls: AtomicUsize,
}

impl ScriptedPollSource {
    fn new(replies: Vec<DeferredPollReply>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            polls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl DeferredPollSource for ScriptedPollSource {
    async fn poll_deferred(
        &self,
        _handle: &DeferredHandle,
        _permit: octet_ai::DeferredPollPermit,
        _leaf_generation: u64,
    ) -> DeferredPollReply {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra poll")
    }
}

#[tokio::test]
async fn cancelling_an_effect_pending_leaf_records_exposure_once_without_repolling() {
    let model = test_model();
    let transport = Arc::new(ParkedTransport::new(&model, "parked-cancel"));
    let (mut agent, _client, _workspace) = parked_agent(model.clone(), transport.clone());

    let (operation_id, _, _) = drive_park(&mut agent).await;

    // Admit the poll directly through the session store, then cancel the
    // unknown-outcome leaf. The cancelled attempt must not be re-polled and its
    // unknown usage must be sticky exposure.
    let store = agent.session().deferred_run_store();
    let admitted = store
        .begin_pass(&operation_id, "pass-cancel", DeferredResumeIntent::Poll, 0)
        .unwrap();
    assert!(matches!(admitted, DeferredResumeStart::Admitted(_)));
    let cancelled = agent
        .cancel_deferred_run(&operation_id, 1)
        .expect("the current generation can be cancelled");
    assert!(cancelled.abandoned_unknown_poll());
    assert!(agent.session().has_uncertain_usage());

    let source = ScriptedPollSource::new(Vec::new());
    let outcome = agent
        .resume_deferred_run(&operation_id, "pass-after", DeferredResumeIntent::Poll, &source)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        DeferredRunOutcome::Finished {
            state: "cancelled",
            ..
        }
    ));
    assert_eq!(source.polls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.fetch_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_refused_before_dispatch_poll_is_replaceable_under_fresh_ids_and_bills_nothing() {
    let model = test_model();
    let transport = Arc::new(ParkedTransport::new(&model, "parked-refused"));
    let (mut agent, _client, _workspace) = parked_agent(model.clone(), transport);

    let (operation_id, _, _) = drive_park(&mut agent).await;

    let source = ScriptedPollSource::new(vec![DeferredPollReply::Refused(
        "transport permit refused".to_owned(),
    )]);
    let outcome = agent
        .resume_deferred_run(
            &operation_id,
            "pass-refused",
            DeferredResumeIntent::Poll,
            &source,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, DeferredRunOutcome::PollRefused(_)));
    assert_eq!(source.polls.load(Ordering::SeqCst), 1);
    // A refusal before dispatch was never billed, so it creates no exposure...
    assert!(!agent.session().has_uncertain_usage());
    // ...and it leaves exactly the reservation it admitted durable, so the leaf
    // stays replaceable.
    assert_eq!(
        agent.deferred_run(&operation_id).unwrap().state_label(),
        "effect_pending"
    );
    let store = agent.session().deferred_run_store();
    let refused_reservation = match &store.record(&operation_id).unwrap().state {
        DeferredRunState::EffectPending { leaf } => match &leaf.phase {
            DeferredPhase::EffectPending {
                response_id,
                usage_id,
            } => (response_id.clone(), usage_id.clone()),
            other => panic!("an admitted poll must stay effect pending, got {other:?}"),
        },
        other => panic!("the refused poll must leave the leaf replaceable, got {other:?}"),
    };

    // A later pass may replace the abandoned reservation under fresh ids
    // instead of reusing the pair the refused attempt reserved, but only after
    // the explicit replacement decision: the durable leaf alone cannot prove
    // the refused poll was never accepted.
    let replacement_source = ScriptedPollSource::new(vec![DeferredPollReply::Settled(Box::new(
        settled_response(&model),
    ))]);
    let refused_without_consent = agent
        .resume_deferred_run(
            &operation_id,
            "pass-replacement-refused",
            DeferredResumeIntent::Poll,
            &replacement_source,
        )
        .await
        .unwrap();
    assert!(matches!(
        refused_without_consent,
        DeferredRunOutcome::Refused(refusal)
            if matches!(
                refusal.kind,
                DeferredPollRefusalKind::UnknownPollOutcome { .. }
            )
    ));
    assert_eq!(replacement_source.polls.load(Ordering::SeqCst), 0);
    let replacement = agent
        .resume_deferred_run(
            &operation_id,
            "pass-replacement",
            DeferredResumeIntent::ReplaceUnknownPoll,
            &replacement_source,
        )
        .await
        .unwrap();
    match replacement {
        DeferredRunOutcome::Settled {
            response,
            response_id,
            usage_id,
        } => {
            assert_ne!((response_id, usage_id), refused_reservation);
            // The replacement poll commits a real provider response, not an
            // empty placeholder: the abandoned attempt's exposure is recorded
            // elsewhere, and this run's own result must still arrive intact.
            assert!(!response.message.content.is_empty());
        }
        other => panic!("the replacement poll must settle, got {other:?}"),
    }
    assert_eq!(replacement_source.polls.load(Ordering::SeqCst), 1);
    // Replacing an unknown-outcome poll is sticky exposure: the abandoned
    // attempt may have been accepted and billed, and a later success never
    // clears that uncertainty.
    assert!(agent.session().has_uncertain_usage());
    assert_eq!(
        agent.deferred_run(&operation_id).unwrap().state_label(),
        "settled"
    );
}
