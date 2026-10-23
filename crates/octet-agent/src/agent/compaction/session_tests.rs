use super::*;
use crate::compaction::{SessionOperationFuture, SessionOperationInvocation};
use std::sync::atomic::AtomicUsize;

struct NativeReply(Option<SessionOperationDecision>);
impl SessionOperationInvocation for NativeReply {
    fn take_future(&mut self) -> SessionOperationFuture {
        let decision = self.0.take().unwrap();
        Box::pin(async move { Ok(decision) })
    }
}
#[derive(Clone, Copy)]
enum Action {
    Cancel,
    Replace,
    RewindOnce,
    Observe,
}
struct Interceptor {
    action: Action,
    seen: Arc<Mutex<Vec<SessionOperation>>>,
}
impl SessionOperationHook for Interceptor {
    fn begin(
        &self,
        session: &Session,
        operation: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        self.seen.lock().unwrap().push(operation.clone());
        let decision = match operation {
            SessionOperation::BeforeCompact { first_kept, .. } => match self.action {
                Action::Cancel => SessionOperationDecision::Cancel,
                Action::Replace => SessionOperationDecision::ReplaceCompaction {
                    replacement: SessionCompactionReplacement {
                        summary: "real extension handoff".into(),
                        first_kept: first_kept.clone(),
                    },
                },
                Action::RewindOnce
                    if self
                        .seen
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|operation| {
                            matches!(operation, SessionOperation::BeforeCompact { .. })
                        })
                        .count()
                        == 1 =>
                {
                    SessionOperationDecision::ReplaceCompaction {
                        replacement: SessionCompactionReplacement {
                            summary: "earlier extension handoff".into(),
                            first_kept: turn_starts(session)[0].clone(),
                        },
                    }
                }
                Action::RewindOnce | Action::Observe => SessionOperationDecision::Continue,
            },
            SessionOperation::BeforeTree { .. } if matches!(self.action, Action::Cancel) => {
                SessionOperationDecision::Cancel
            }
            SessionOperation::Compacted { entry, .. } => {
                assert_eq!(session.head(), Some(entry.id.clone()));
                assert!(matches!(
                    session.entry(&entry.id).unwrap().value,
                    EntryValue::Compaction { .. }
                ));
                SessionOperationDecision::Continue
            }
            SessionOperation::Tree { new_head, .. } => {
                assert_eq!(&session.head(), new_head);
                SessionOperationDecision::Continue
            }
            _ => SessionOperationDecision::Continue,
        };
        Ok(Some(Box::new(NativeReply(Some(decision)))))
    }
}
struct LocalProvider(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for LocalProvider {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        _: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text("local summary".into())],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: Some(octet_ai::Cost::default()),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}
fn agent(
    action: Action,
) -> (
    Agent,
    Arc<Mutex<Vec<SessionOperation>>>,
    Arc<AtomicUsize>,
    tempfile::TempDir,
) {
    let workspace = tempfile::tempdir().unwrap();
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).limits.context_window = 20_000;
    Arc::make_mut(&mut model.spec).limits.max_output_tokens = 16_384;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = AiClient::new();
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(LocalProvider(calls.clone())),
    );
    let mut host = ExtensionHost::new();
    host.session_operation_hook(Interceptor {
        action,
        seen: seen.clone(),
    });
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(workspace.path().join("session.jsonl")).unwrap(),
        extensions: host,
        system: "test system".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("session-operation-test".into()),
    })
    .unwrap();
    agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, 1)
        .unwrap();
    for text in ["older history", "recent history"] {
        agent
            .session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text(text.into())],
            })))
            .unwrap();
    }
    (agent, seen, calls, workspace)
}

#[tokio::test]
async fn manual_veto_never_calls_provider_or_appends_checkpoint() {
    let (mut agent, seen, calls, _dir) = agent(Action::Cancel);
    let revision = SessionSourceRevision::capture(&agent.session).unwrap();
    let error = agent
        .compact_session_with_instructions(None, CancellationToken::default(), drop)
        .await
        .unwrap_err();
    assert!(is_compaction_veto(&error));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    revision.validate(&agent.session).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn replacement_commits_once_and_observes_real_checkpoint_then_reopens() {
    let (mut agent, seen, calls, dir) = agent(Action::Replace);
    let info = agent
        .compact_session_with_instructions(
            Some("retain the goal"),
            CancellationToken::default(),
            drop,
        )
        .await
        .unwrap();
    assert_eq!(info.summary, "real extension handoff");
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(agent.session.usage_records().is_empty());
    let entries: Vec<_> = agent
        .session
        .entries()
        .iter()
        .filter(|entry| matches!(entry.value, EntryValue::Compaction { .. }))
        .collect();
    assert_eq!(entries.len(), 1);
    let checkpoint = entries[0].id.clone();
    assert!(
        matches!(&seen.lock().unwrap()[0], SessionOperation::BeforeCompact {
        reason: SessionCompactionReason::Manual, custom_instructions: Some(text), .. } if text == "retain the goal")
    );
    assert!(
        matches!(&seen.lock().unwrap()[1], SessionOperation::Compacted { entry, from_extension: true, .. }
        if entry.id == checkpoint)
    );
    drop(agent);
    let reopened = Session::open(dir.path().join("session.jsonl")).unwrap();
    assert_eq!(reopened.head(), Some(checkpoint));
    assert!(serialize_conversation(&reopened.context().unwrap()).contains("real extension handoff"));
}

#[tokio::test]
async fn default_manual_summary_uses_existing_provider_accounting_once() {
    let (mut agent, seen, calls, _dir) = agent(Action::Observe);
    let info = agent
        .compact_session_with_instructions(None, CancellationToken::default(), drop)
        .await
        .unwrap();
    assert!(info.summary.contains("local summary"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(agent.session.usage_records().len(), 1);
    assert!(matches!(
        &seen.lock().unwrap()[1],
        SessionOperation::Compacted {
            from_extension: false,
            ..
        }
    ));
}

#[tokio::test]
async fn threshold_veto_allows_in_budget_request_without_reentering_hook() {
    let (mut agent, seen, calls, _dir) = agent(Action::Cancel);
    agent.complete("a short next prompt").await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let seen = seen.lock().unwrap();
    assert!(matches!(
        seen.as_slice(),
        [
            SessionOperation::ModelTurnStart { run_id: start, turn_index: 0, .. },
            SessionOperation::BeforeCompact {
                reason: SessionCompactionReason::Threshold,
                ..
            },
            SessionOperation::ModelTurnEnd {
                run_id: end,
                turn_index: 0,
                assistant_entry,
                tool_result_entries,
                ..
            },
        ] if start == end
            && tool_result_entries.is_empty()
            && serde_json::to_value(agent.session.entry(&assistant_entry.id).unwrap()).unwrap()
                == serde_json::to_value(assistant_entry).unwrap()
    ));
    assert!(!agent
        .session
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })));
}

#[tokio::test]
async fn overflow_veto_cannot_bypass_capacity_or_dispatch_provider() {
    let (mut agent, seen, calls, _dir) = agent(Action::Cancel);
    let error = agent
        .complete("oversized history ".repeat(6000))
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::ContextExceeded { .. }));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(matches!(
        seen.lock().unwrap().as_slice(),
        [
            SessionOperation::ModelTurnStart { turn_index: 0, .. },
            SessionOperation::BeforeCompact {
                reason: SessionCompactionReason::Overflow,
                ..
            },
        ]
    ));
    assert!(!agent
        .session
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })));
}

/// A summary larger than the episode it replaces must not trigger another
/// interruption in the same capacity pass, even if another cut is available.
struct ExpandingSummaryProvider {
    summaries: Arc<AtomicUsize>,
    answers: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for ExpandingSummaryProvider {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: Request,
        diagnostics: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        if request.system.as_deref() == Some(SUMMARIZATION_SYSTEM_PROMPT) {
            // Fail deterministically rather than hanging if compaction repeats.
            // The assistant cut needs one history and one turn-prefix call.
            assert!(self.summaries.fetch_add(1, Ordering::Relaxed) < 2);
            let response = octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text("expanded handoff ".repeat(100))],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: Some(octet_ai::Cost::default()),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            };
            return Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::Started { response_id: None }),
                Ok(StreamEvent::Finished(response)),
            ])));
        }
        LocalProvider(self.answers.clone())
            .stream(model, request, diagnostics)
            .await
    }
}

fn append_completed_episode(agent: &mut Agent, text: &str) {
    agent
        .session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(text.into())],
        })))
        .unwrap();
    agent
        .session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("episode reply".into())],
            model: agent.model.spec.id.clone(),
            protocol: agent.model.spec.protocol,
        })))
        .unwrap();
}

#[tokio::test]
async fn expanding_summary_stops_threshold_compaction_and_continues() {
    let (mut agent, _, _, dir) = agent(Action::Observe);
    agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, u64::MAX)
        .unwrap();
    for _ in 0..3 {
        append_completed_episode(&mut agent, "small episode");
    }
    let summaries = Arc::new(AtomicUsize::new(0));
    let answers = Arc::new(AtomicUsize::new(0));
    agent.client.register_host_stream_transport(
        agent.model.endpoint.id.clone(),
        Arc::new(ExpandingSummaryProvider {
            summaries: summaries.clone(),
            answers: answers.clone(),
        }),
    );
    agent.complete("next prompt").await.unwrap();
    assert_eq!(summaries.load(Ordering::Relaxed), 2);
    assert_eq!(answers.load(Ordering::Relaxed), 1);
    assert_eq!(agent.session.usage_records().len(), 3);
    drop(agent);
    let reopened = Session::open(dir.path().join("session.jsonl")).unwrap();
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(entry.value, EntryValue::Compaction { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn expanding_summary_stops_hard_overflow_without_dispatching_answer() {
    let (mut agent, _, _, _dir) = agent(Action::Observe);
    agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, u64::MAX)
        .unwrap();
    for _ in 0..3 {
        append_completed_episode(&mut agent, "small episode");
    }
    let mut summary_model = agent.model.clone();
    Arc::make_mut(&mut summary_model.spec).limits.context_window = 128_000;
    agent.set_compaction_model(Some(summary_model));
    let summaries = Arc::new(AtomicUsize::new(0));
    let answers = Arc::new(AtomicUsize::new(0));
    agent.client.register_host_stream_transport(
        agent.model.endpoint.id.clone(),
        Arc::new(ExpandingSummaryProvider {
            summaries: summaries.clone(),
            answers: answers.clone(),
        }),
    );
    let error = agent
        .complete("oversized history ".repeat(6000))
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::ContextExceeded { estimate, budget } if estimate > budget));
    assert_eq!(summaries.load(Ordering::Relaxed), 2);
    assert_eq!(answers.load(Ordering::Relaxed), 0);
    assert_eq!(agent.session.usage_records().len(), 2);
}

#[tokio::test]
async fn reducing_summaries_can_advance_successive_distinct_boundaries() {
    let (mut agent, seen, calls, _dir) = agent(Action::Observe);
    agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, u64::MAX)
        .unwrap();
    for _ in 0..4 {
        append_completed_episode(&mut agent, &"older detail ".repeat(200));
    }
    // Leave room for the complete summary transcript on its separate route.
    let mut summary_model = agent.model.clone();
    Arc::make_mut(&mut summary_model.spec).limits.context_window = 128_000;
    agent.set_compaction_model(Some(summary_model));
    agent.complete("next prompt").await.unwrap();
    let boundaries: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|operation| match operation {
            SessionOperation::Compacted { entry, .. } => match &entry.value {
                EntryValue::Compaction { first_kept, .. } => Some(first_kept.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(boundaries.len(), 3);
    assert_eq!(boundaries.iter().collect::<HashSet<_>>().len(), 3);
    assert_eq!(calls.load(Ordering::Relaxed), 7); // history + prefix per cut, then answer
}

#[tokio::test]
async fn rewritten_boundary_fallback_searches_past_attempted_candidate() {
    let (mut agent, seen, _, _dir) = agent(Action::RewindOnce);
    agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, u64::MAX)
        .unwrap();
    for _ in 0..4 {
        append_completed_episode(&mut agent, &"older detail ".repeat(200));
    }
    let original_boundaries = turn_starts(&agent.session);
    let mut summary_model = agent.model.clone();
    Arc::make_mut(&mut summary_model.spec).limits.context_window = 128_000;
    agent.set_compaction_model(Some(summary_model));
    agent.complete("next prompt").await.unwrap();
    let seen = seen.lock().unwrap();
    let requested: Vec<_> = seen
        .iter()
        .filter_map(|operation| match operation {
            SessionOperation::BeforeCompact { first_kept, .. } => Some(first_kept),
            _ => None,
        })
        .collect();
    assert_eq!(requested[0], &original_boundaries[1]);
    assert!(
        requested.len() > 1,
        "must scan beyond an attempted fallback"
    );
    assert_eq!(requested[1], &original_boundaries[2]);
}

struct FixedProjection(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl crate::extension::ProviderContextHook for FixedProjection {
    async fn project_context(
        &self,
        _: &Request,
        _: &crate::extension::ProviderContextProjectionContext,
    ) -> Result<Option<crate::extension::ProviderContextProjection>, String> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(Some(crate::extension::ProviderContextProjection {
            system: None,
            tools: None,
            messages: vec![Message::User(UserMessage {
                content: vec![UserPart::Text("fixed projected request".into())],
            })],
        }))
    }
}

#[tokio::test]
async fn unchanged_projection_stops_even_when_canonical_history_shrinks() {
    let (mut agent, seen, calls, _dir) = agent(Action::Observe);
    agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, u64::MAX)
        .unwrap();
    for _ in 0..4 {
        append_completed_episode(&mut agent, &"older detail ".repeat(200));
    }
    let mut summary_model = agent.model.clone();
    Arc::make_mut(&mut summary_model.spec).limits.context_window = 128_000;
    agent.set_compaction_model(Some(summary_model));
    let projections = Arc::new(AtomicUsize::new(0));
    agent
        .extensions
        .provider_context_hook(FixedProjection(projections.clone()));
    agent.complete("next prompt").await.unwrap();
    assert_eq!(projections.load(Ordering::Relaxed), 2);
    assert_eq!(calls.load(Ordering::Relaxed), 3); // history, prefix, answer
    assert_eq!(
        seen.lock()
            .unwrap()
            .iter()
            .filter(|operation| matches!(operation, SessionOperation::Compacted { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn tree_veto_preserves_head_and_no_after_event() {
    let (mut agent, seen, calls, _dir) = agent(Action::Cancel);
    let revision = SessionSourceRevision::capture(&agent.session).unwrap();
    assert!(agent
        .navigate_session_tree(None, CancellationToken::default())
        .await
        .is_err());
    revision.validate(&agent.session).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn tree_observation_follows_real_synced_checkout_and_survives_reopen() {
    let (mut agent, seen, calls, dir) = agent(Action::Observe);
    let target = agent.session.entries()[0].id.clone();
    agent
        .navigate_session_tree(Some(target.clone()), CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(agent.session.head(), Some(target.clone()));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(seen.lock().unwrap().len(), 2);
    drop(agent);
    assert_eq!(
        Session::open(dir.path().join("session.jsonl"))
            .unwrap()
            .head(),
        Some(target)
    );
}
