//! Native idle compaction outcomes, after-hook append identity and cancellation.
use super::support::*;
use super::*;
use octet_agent::compaction::{
    SessionCompactionReplacement, SessionOperation, SessionOperationDecision,
    SessionOperationFuture, SessionOperationHook, SessionOperationInvocation,
};
use std::sync::atomic::{AtomicBool, Ordering};

struct CompactHook {
    veto: bool,
    fail_after: bool,
    after_settled: Arc<AtomicBool>,
}

struct CompactReply {
    decision: Option<SessionOperationDecision>,
    append: bool,
    completed: Option<tokio::sync::oneshot::Sender<()>>,
    completion: Option<tokio::sync::oneshot::Receiver<()>>,
    after_settled: Arc<AtomicBool>,
    fail_after: bool,
}

impl SessionOperationInvocation for CompactReply {
    fn take_future(&mut self) -> SessionOperationFuture {
        let decision = self.decision.take().unwrap();
        let completion = self.completion.take();
        let settled = self.after_settled.clone();
        let fail = self.fail_after;
        Box::pin(async move {
            if let Some(completion) = completion {
                completion.await.unwrap();
                settled.store(true, Ordering::Release);
                if fail {
                    return Err("after-hook failure".into());
                }
            }
            Ok(decision)
        })
    }

    fn ready(&self) -> Pin<Box<dyn std::future::Future<Output = bool> + Send + 'static>> {
        if self.append {
            Box::pin(async { true })
        } else {
            Box::pin(std::future::pending())
        }
    }

    fn consume_next(&mut self, session: &mut Session) -> Result<(), String> {
        session
            .append(EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            })
            .map_err(|error| error.to_string())?;
        self.append = false;
        self.completed.take().unwrap().send(()).unwrap();
        Ok(())
    }
}

impl SessionOperationHook for CompactHook {
    fn begin(
        &self,
        session: &Session,
        operation: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        let (decision, after) = match operation {
            SessionOperation::BeforeCompact {
                first_kept,
                custom_instructions,
                ..
            } => {
                assert_eq!(custom_instructions.as_deref(), Some("keep the goal"));
                (
                    if self.veto {
                        SessionOperationDecision::Cancel
                    } else {
                        SessionOperationDecision::ReplaceCompaction {
                            replacement: SessionCompactionReplacement {
                                summary: "real idle handoff".into(),
                                first_kept: first_kept.clone(),
                            },
                        }
                    },
                    false,
                )
            }
            SessionOperation::Compacted { entry, .. } => {
                assert!(
                    Session::open_read_only(session.path())
                        .unwrap()
                        .entry(&entry.id)
                        .is_some(),
                    "after-hook sees synced checkpoint"
                );
                (SessionOperationDecision::Continue, true)
            }
            _ => return Ok(None),
        };
        let (completed, completion) = tokio::sync::oneshot::channel();
        Ok(Some(Box::new(CompactReply {
            decision: Some(decision),
            append: after,
            completed: after.then_some(completed),
            completion: after.then_some(completion),
            after_settled: self.after_settled.clone(),
            fail_after: self.fail_after,
        })))
    }
}

#[tokio::test]
async fn idle_compact_returns_the_new_checkpoint_not_after_hook_metadata_and_never_replays_failure()
{
    for (veto, fail_after) in [(false, false), (true, false), (false, true)] {
        let after_settled = Arc::new(AtomicBool::new(false));
        let (_directory, mut app) =
            crate::compaction::tests::app_for_session_operation(CompactHook {
                veto,
                fail_after,
                after_settled: after_settled.clone(),
            });
        seed_compaction_session(&mut app.agent);
        app.agent
            .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, 1)
            .unwrap();
        let before = app.agent.session().entries().len();
        let path = app.agent.session().path().to_owned();
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        let result = compact_extension_session(
            &mut app,
            &mut shell,
            &mut input,
            Some("keep the goal"),
            octet_agent::CancellationToken::default(),
        )
        .await;
        let committed = app.agent.session().entries()[before..]
            .iter()
            .filter(|entry| matches!(entry.value, EntryValue::Compaction { .. }))
            .collect::<Vec<_>>();
        assert!(
            app.agent.session().usage_records().is_empty(),
            "replacement has no fabricated provider metrics"
        );
        if veto {
            assert!(result.unwrap_err().contains("cancelled by extension"));
            assert!(committed.is_empty());
            assert!(!after_settled.load(Ordering::Acquire));
        } else {
            assert_eq!(committed.len(), 1);
            assert!(
                after_settled.load(Ordering::Acquire),
                "response follows after-hook settlement"
            );
            let checkpoint = committed[0].id.clone();
            assert_ne!(app.agent.session().head(), Some(checkpoint.clone()));
            if fail_after {
                let error = result.unwrap_err();
                assert!(error.contains("do not retry"), "{error}");
                assert!(error.contains(&checkpoint.0), "{error}");
            } else {
                let result = result.unwrap();
                assert_eq!(result.entry_id, checkpoint.0);
                assert_eq!(result.summary, "real idle handoff");
                let EntryValue::Compaction { first_kept, .. } = &committed[0].value else {
                    unreachable!()
                };
                assert_eq!(result.first_kept, first_kept.0);
            }
            drop(app);
            let reopened = Session::open_read_only(path).unwrap();
            assert!(
                reopened.entry(&checkpoint).is_some(),
                "success and post-commit errors retain one durable checkpoint"
            );
            assert_eq!(
                reopened
                    .entries()
                    .iter()
                    .filter(|entry| matches!(entry.value, EntryValue::Compaction { .. }))
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn idle_compact_native_token_cancellation_settles_uncertain_provider_accounting() {
    let (server, started, _release) = HeldApi::start(text_turn()).await;
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    app.agent
        .set_compaction_model(Some(scripted_model(&server.uri)));
    seed_compaction_session(&mut app.agent);
    app.agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, 1)
        .unwrap();
    let before = app.agent.session().entries().len();
    let token = octet_agent::CancellationToken::default();
    let cancellation = token.clone();
    let cancel = async move {
        started.await.unwrap();
        cancellation.cancel();
    };
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            compact_extension_session(&mut app, &mut shell, &mut input, None, token),
            cancel
        )
    })
    .await
    .unwrap();
    assert!(result.is_err());
    assert_eq!(app.agent.session().entries().len(), before);
    assert!(app.agent.session().has_uncertain_usage());
    assert!(app.agent.session().usage_records().is_empty());
    assert_eq!(
        server.requests.load(Ordering::SeqCst),
        1,
        "cancelled summary is never replayed"
    );
}
