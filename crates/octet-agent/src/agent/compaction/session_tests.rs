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
                Action::Observe => SessionOperationDecision::Continue,
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
    assert_eq!(seen.len(), 1);
    assert!(matches!(
        seen[0],
        SessionOperation::BeforeCompact {
            reason: SessionCompactionReason::Threshold,
            ..
        }
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
        seen.lock().unwrap()[0],
        SessionOperation::BeforeCompact {
            reason: SessionCompactionReason::Overflow,
            ..
        }
    ));
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
