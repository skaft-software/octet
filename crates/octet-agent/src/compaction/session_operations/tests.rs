use super::*;
use crate::extension_process::ExtensionResourceOwner;
use crate::session::{EntryValue, Session};
use crate::session_leaf::{SessionLeafBinding, SessionLeafConsumer};
use octet_ai::{Message, UserMessage, UserPart};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

fn user(session: &mut Session, text: &str) -> EntryId {
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(text.into())],
        })))
        .unwrap()
}
fn before(session: &Session, first_kept: &EntryId) -> SessionOperation {
    SessionOperation::BeforeCompact {
        reason: SessionCompactionReason::Threshold,
        first_kept: first_kept.clone(),
        preparation: crate::compaction::prepare_handoff(session, first_kept).unwrap(),
        branch_entries: session_operation_branch(session).unwrap(),
        custom_instructions: None,
    }
}

struct Immediate {
    decision: Option<SessionOperationDecision>,
    cancel: Option<CancellationToken>,
}
impl SessionOperationInvocation for Immediate {
    fn take_future(&mut self) -> SessionOperationFuture {
        let decision = self.decision.take().unwrap();
        let cancel = self.cancel.clone();
        Box::pin(async move {
            if let Some(cancel) = cancel {
                cancel.cancel();
            }
            Ok(decision)
        })
    }
}
struct DecisionHook {
    decision: SessionOperationDecision,
    calls: Arc<AtomicUsize>,
    cancel: Option<CancellationToken>,
}
impl SessionOperationHook for DecisionHook {
    fn begin(
        &self,
        _: &Session,
        _: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Some(Box::new(Immediate {
            decision: Some(self.decision.clone()),
            cancel: self.cancel.clone(),
        })))
    }
}
fn hook(
    decision: SessionOperationDecision,
    calls: &Arc<AtomicUsize>,
) -> Arc<dyn SessionOperationHook> {
    Arc::new(DecisionHook {
        decision,
        calls: calls.clone(),
        cancel: None,
    })
}

#[tokio::test]
async fn veto_stops_hooks_without_a_write_or_successful_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    user(&mut session, "older turn");
    let tail = user(&mut session, "current turn");
    let operation = before(&session, &tail);
    let revision = SessionSourceRevision::capture(&session).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let decision = run_session_operation_hooks(
        &mut session,
        &[
            hook(SessionOperationDecision::Cancel, &calls),
            hook(SessionOperationDecision::Continue, &calls),
        ],
        &operation,
        &CancellationToken::default(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(decision, SessionOperationDecision::Cancel);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    revision.validate(&session).unwrap();
    assert!(!session
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })));
}

#[tokio::test]
async fn replacement_is_preserved_across_later_no_op_and_validated_against_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    user(&mut session, "older turn");
    let tail = user(&mut session, "current turn");
    let operation = before(&session, &tail);
    let calls = Arc::new(AtomicUsize::new(0));
    let replacement = SessionOperationDecision::ReplaceCompaction {
        replacement: SessionCompactionReplacement {
            summary: "actual handoff".into(),
            first_kept: tail,
        },
    };
    let decision = run_session_operation_hooks(
        &mut session,
        &[
            hook(replacement.clone(), &calls),
            hook(SessionOperationDecision::Continue, &calls),
        ],
        &operation,
        &CancellationToken::default(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(decision, replacement);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    for (summary, first_kept) in [
        (" ".into(), EntryId("001".into())),
        ("valid".into(), EntryId("foreign".into())),
        (
            "x".repeat(MAX_COMPACTION_HANDOFF_BYTES + 1),
            EntryId("001".into()),
        ),
    ] {
        assert!(matches!(
            validate_decision(
                &operation,
                &SessionOperationDecision::ReplaceCompaction {
                    replacement: SessionCompactionReplacement {
                        summary,
                        first_kept
                    },
                }
            ),
            Err(SessionOperationError::InvalidDecision)
        ));
    }
}

#[tokio::test]
async fn cancellation_wins_callback_same_poll_completion() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let cancellation = CancellationToken::default();
    let revision = SessionSourceRevision::capture(&session).unwrap();
    let hook: Arc<dyn SessionOperationHook> = Arc::new(DecisionHook {
        decision: SessionOperationDecision::Continue,
        calls: Arc::new(AtomicUsize::new(0)),
        cancel: Some(cancellation.clone()),
    });
    let result = run_session_operation_hooks(
        &mut session,
        &[hook],
        &SessionOperation::BeforeTree {
            target_id: None,
            old_head: None,
        },
        &cancellation,
        Duration::from_secs(1),
    )
    .await;
    assert!(matches!(result, Err(SessionOperationError::Cancelled)));
    revision.validate(&session).unwrap();
}

struct Pending;
impl SessionOperationInvocation for Pending {
    fn take_future(&mut self) -> SessionOperationFuture {
        Box::pin(std::future::pending())
    }
}
impl SessionOperationHook for Pending {
    fn begin(
        &self,
        _: &Session,
        _: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        Ok(Some(Box::new(Pending)))
    }
}
#[tokio::test(start_paused = true)]
async fn pending_hook_times_out_without_native_fallback_or_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let revision = SessionSourceRevision::capture(&session).unwrap();
    assert!(matches!(
        run_session_operation_hooks(
            &mut session,
            &[Arc::new(Pending)],
            &SessionOperation::BeforeTree {
                target_id: None,
                old_head: None
            },
            &CancellationToken::default(),
            Duration::from_secs(3)
        )
        .await,
        Err(SessionOperationError::Deadline)
    ));
    revision.validate(&session).unwrap();
}

#[test]
fn source_revision_rejects_head_aba_and_reopened_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let root = user(&mut session, "root");
    let leaf = user(&mut session, "leaf");
    let revision = SessionSourceRevision::capture(&session).unwrap();
    session.checkout(root).unwrap();
    session.checkout(leaf).unwrap();
    assert!(matches!(
        revision.validate(&session),
        Err(SessionOperationError::StaleSource)
    ));
    let revision = SessionSourceRevision::capture(&session).unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(matches!(
        revision.validate(&reopened),
        Err(SessionOperationError::StaleSource)
    ));
}

#[test]
fn model_turn_observations_serialize_real_entries_and_only_accept_continue() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let root = user(&mut session, "real user");
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("real answer".into())],
                model: octet_ai::ModelId("local".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    let end = SessionOperation::ModelTurnEnd {
        run_id: format!("run:{}", root.0),
        turn_index: 7,
        timestamp_ms: 123,
        assistant_entry: session.entry(&assistant).unwrap().clone(),
        tool_result_entries: Vec::new(),
    };
    let value = serde_json::to_value(&end).unwrap();
    assert_eq!(value["kind"], "model_turn_end");
    assert_eq!(value["turn_index"], 7);
    assert_eq!(value["timestamp_ms"], 123);
    assert_eq!(
        value["assistant_entry"],
        serde_json::to_value(session.entry(&assistant).unwrap()).unwrap()
    );
    assert_eq!(value["tool_result_entries"], serde_json::json!([]));
    let start = SessionOperation::ModelTurnStart {
        run_id: format!("run:{}", root.0),
        turn_index: 7,
        timestamp_ms: 122,
    };
    assert_eq!(
        serde_json::to_value(&start).unwrap(),
        serde_json::json!({
            "kind":"model_turn_start", "run_id":format!("run:{}", root.0),
            "turn_index":7, "timestamp_ms":122,
        })
    );
    for operation in [&start, &end] {
        validate_decision(operation, &SessionOperationDecision::Continue).unwrap();
        for decision in [
            SessionOperationDecision::Cancel,
            SessionOperationDecision::ReplaceCompaction {
                replacement: SessionCompactionReplacement {
                    summary: "not authority".into(),
                    first_kept: root.clone(),
                },
            },
        ] {
            assert!(matches!(
                validate_decision(operation, &decision),
                Err(SessionOperationError::InvalidDecision)
            ));
        }
    }
}

#[test]
fn observations_cannot_veto_or_replace_already_committed_work() {
    let event = SessionOperation::Tree {
        old_head: None,
        new_head: None,
    };
    assert!(matches!(
        validate_decision(&event, &SessionOperationDecision::Cancel),
        Err(SessionOperationError::InvalidDecision)
    ));
    assert!(serde_json::from_value::<SessionOperationDecision>(serde_json::json!({
        "action":"replace_compaction", "replacement":{"summary":"x", "first_kept":"001", "unknown":true}
    })).is_err());
}

struct AppendHook {
    committed: Arc<Mutex<Option<EntryId>>>,
}
struct AppendInvocation {
    consumer: SessionLeafConsumer,
    binding: SessionLeafBinding,
    future: Option<SessionOperationFuture>,
}
impl SessionOperationInvocation for AppendInvocation {
    fn take_future(&mut self) -> SessionOperationFuture {
        self.future.take().unwrap()
    }
    fn ready(&self) -> Pin<Box<dyn Future<Output = bool> + Send + 'static>> {
        Box::pin(self.consumer.ready())
    }
    fn consume_next(&mut self, session: &mut Session) -> Result<(), String> {
        self.consumer
            .consume_next(session, &self.binding)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
impl SessionOperationHook for AppendHook {
    fn begin(
        &self,
        session: &Session,
        _: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        let binding = SessionLeafBinding {
            activation_epoch: 1,
            owner: ExtensionResourceOwner {
                session_id: session.resource_owner_key(),
                extension_instance_id: "native-test".into(),
                process_generation: 3,
            },
            namespace: "octet.test".into(),
            operation_id: "session-compact:1".into(),
        };
        let (consumer, producer, grant) =
            SessionLeafConsumer::new(session, binding.clone()).unwrap();
        let committed = self.committed.clone();
        let future = Box::pin(async move {
            let receipt = producer
                .try_append(
                    grant.id(),
                    grant.binding(),
                    "projection".into(),
                    serde_json::json!({"revision":2,"text":"Goal\nNext steps"}),
                )
                .unwrap();
            let result = receipt.wait().await.unwrap();
            assert_eq!(result.entry_id, result.head);
            *committed.lock().unwrap() = Some(result.entry_id);
            Ok(SessionOperationDecision::Continue)
        });
        Ok(Some(Box::new(AppendInvocation {
            consumer,
            binding,
            future: Some(future),
        })))
    }
}
#[tokio::test]
async fn callback_can_wait_for_durable_private_append_on_same_session_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let committed = Arc::new(Mutex::new(None));
    let result = run_session_operation_hooks(
        &mut session,
        &[Arc::new(AppendHook {
            committed: committed.clone(),
        })],
        &SessionOperation::Tree {
            old_head: None,
            new_head: None,
        },
        &CancellationToken::default(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(result, SessionOperationDecision::Continue);
    let id = committed.lock().unwrap().clone().unwrap();
    assert_eq!(session.head(), Some(id.clone()));
    assert!(session.context().unwrap().is_empty());
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.head(), Some(id.clone()));
    assert_eq!(
        reopened.extension_entry(&id, "octet.test").unwrap().data["revision"],
        2
    );
}
