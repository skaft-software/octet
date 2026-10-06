//! Held-request input liveness: manual compaction, model requests, and caller-driven
//! run settlement against a provider that never sends tokens.
//! Separate because a held request is the only state where the input owner and the
//! renderer can both believe they have control.

use super::*;

use super::support::*;

#[derive(Clone, Copy, Debug)]
enum HeldOutcome {
    Success,
    Timeout,
    TransportFailure,
    Cancel,
}

async fn held_api_input(
    started: tokio::sync::oneshot::Receiver<()>,
    sender: tokio::sync::mpsc::Sender<std::io::Result<Event>>,
    handled: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::oneshot::Sender<bool>,
    outcome: HeldOutcome,
    columns: u16,
) {
    use crossterm::event::KeyEvent;
    tokio::time::timeout(Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    sender.send(Ok(Event::Resize(columns, 8))).await.unwrap();
    sender
        .send(Ok(Event::Paste("draft while API waits".into())))
        .await
        .unwrap();
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        ))))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_millis(250), handled)
        .await
        .expect("input handling budget while response is held: 250 ms")
        .unwrap();
    match outcome {
        HeldOutcome::Success => {
            release.send(true).unwrap();
        }
        HeldOutcome::TransportFailure => {
            release.send(false).unwrap();
        }
        HeldOutcome::Cancel => {
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Esc,
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
            // Keep both gates alive until the driven operation has settled.
            std::future::pending::<()>().await;
        }
        HeldOutcome::Timeout => std::future::pending::<()>().await,
    }
    // An input EOF would itself abort an ordinary run, masking its result.
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn held_open_manual_compaction_keeps_input_live_and_settles_all_outcomes() {
    for outcome in [
        HeldOutcome::Success,
        HeldOutcome::Timeout,
        HeldOutcome::TransportFailure,
        HeldOutcome::Cancel,
    ] {
        for columns in [46, 80] {
            let (server, started, release) = HeldApi::start(text_turn()).await;
            let (_workspace, mut app) = crate::compaction::tests::app_for_estimate();
            // complete() drives the real streaming path even for a local
            // compaction summary. Only this fixture changes stream limits.
            app.client = octet_ai::AiClient::new()
                .with_stream_timeouts(Duration::from_millis(750), Duration::from_secs(2));
            // Summaries now use Agent's retry/accounting client. Rebuild it
            // with the fixture's bounded stream timeouts before starting.
            app = rebuild_app(app, None, None, None, None).unwrap();
            app.agent
                .set_compaction_model(Some(scripted_model(&server.uri)));
            seed_compaction_session(&mut app.agent);
            let force = columns == 46;
            let original_keep = if force { 99 } else { 1 };
            app.config.compaction.keep_recent_tokens = original_keep;
            let original_agent_policy = app.agent.compaction_token_policy();
            let before = app.agent.session().entries().len();
            let mut shell = InteractiveShell::test_shell();
            let was_verbose = shell.verbose_tools();
            let (sender, receiver) = tokio::sync::mpsc::channel(8);
            let (handled_tx, handled_rx) = tokio::sync::oneshot::channel();
            let mut input = ProbedInput {
                input: tokio_stream::wrappers::ReceiverStream::new(receiver),
                remaining: 3,
                handled: Some(handled_tx),
            };
            let stimulus = held_api_input(started, sender, handled_rx, release, outcome, columns);
            tokio::pin!(stimulus);
            // 750ms initial body timeout + the shared summary policy's
            // 500ms first backoff + an immediate terminal HTTP 400 on the
            // replacement fits the original bound. Do not hide a blocked
            // fixture accept loop by extending this deadline.
            tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    result = compact_interactively(&mut app, &mut shell, &mut input, force, None) => result,
                    _ = &mut stimulus => unreachable!(),
                }
            }).await.unwrap_or_else(|error| panic!(
                "held compaction must settle: outcome={outcome:?}, columns={columns}, requests={}, error={error}",
                server.requests.load(std::sync::atomic::Ordering::SeqCst),
            ));
            let snapshot = shell.debug_snapshot();
            match outcome {
                HeldOutcome::Success => {
                    assert!(snapshot.contains("Context compacted"), "{snapshot}")
                }
                HeldOutcome::Timeout | HeldOutcome::TransportFailure => {
                    assert!(snapshot.contains("compaction skipped"), "{snapshot}")
                }
                HeldOutcome::Cancel => {
                    assert!(snapshot.contains("compaction cancelled"), "{snapshot}")
                }
            }
            assert_eq!(app.config.compaction.keep_recent_tokens, original_keep);
            assert_eq!(app.agent.compaction_token_policy(), original_agent_policy);
            assert_eq!(shell.pending(), "draft while API waits");
            assert_ne!(shell.verbose_tools(), was_verbose);
            let expected_requests = match outcome {
                HeldOutcome::Timeout | HeldOutcome::TransportFailure => 2,
                HeldOutcome::Success | HeldOutcome::Cancel => 1,
            };
            assert_eq!(
                server.requests.load(std::sync::atomic::Ordering::SeqCst),
                expected_requests,
                "the first summary replacement must hit the fixture's terminal rejection: {outcome:?}",
            );
            if !matches!(outcome, HeldOutcome::Success) {
                assert!(app.agent.session().has_uncertain_usage(), "{outcome:?}");
                assert!(app.agent.session().usage_records().is_empty(),
                    "failed/abandoned summaries have unknown usage, not invented successful receipts");
                assert_eq!(
                    app.agent.session().entries().len(),
                    before,
                    "unsettled compaction must not append a summary"
                );
            }
        }
    }
}

#[tokio::test]
async fn held_open_model_request_keeps_input_live_and_settles_once() {
    for outcome in [
        HeldOutcome::Success,
        HeldOutcome::Timeout,
        HeldOutcome::TransportFailure,
        HeldOutcome::Cancel,
    ] {
        let (server, started, release) = HeldApi::start(text_turn()).await;
        let client = octet_ai::AiClient::new()
            .with_stream_timeouts(Duration::from_millis(750), Duration::from_secs(2));
        let (_workspace, mut agent) = scripted_agent_for_route(scripted_model(&server.uri), client);
        let mut shell = InteractiveShell::test_shell();
        let was_verbose = shell.verbose_tools();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let (handled_tx, handled_rx) = tokio::sync::oneshot::channel();
        let mut input = ProbedInput {
            input: tokio_stream::wrappers::ReceiverStream::new(receiver),
            remaining: 3,
            handled: Some(handled_tx),
        };
        let mut ticker = tokio::time::interval(Duration::from_millis(16));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut made_tool_call = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let stimulus = held_api_input(started, sender, handled_rx, release, outcome, 80);
        tokio::pin!(stimulus);
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            let mut goal_deadline = None;
            tokio::select! {
                result = drive_active_run(&mut run, &control, &mut shell, &mut input,
                    &mut ticker, &mut pending, &mut quit, None, None, &mut extensions, &mut made_tool_call,
                    test_run_inspection(), &mut goal_deadline) => result.unwrap(),
                _ = &mut stimulus => unreachable!(),
            }
        }).await.expect("held model request must settle");
        assert!(run.next().await.is_none(), "exactly one terminal event");
        drop(run);
        match outcome {
            HeldOutcome::Success => assert_eq!(ended, HostRunOutcome::Completed),
            HeldOutcome::Cancel => assert_eq!(ended, HostRunOutcome::Aborted),
            HeldOutcome::Timeout | HeldOutcome::TransportFailure => {
                assert!(matches!(ended, HostRunOutcome::Failed(_)))
            }
        }
        assert_eq!(shell.pending(), "draft while API waits");
        assert_ne!(shell.verbose_tools(), was_verbose);
        assert_eq!(agent.session().checkpoints().len(), 1);
        if !matches!(outcome, HeldOutcome::TransportFailure) {
            assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
        assert!(!quit);
    }
}

#[tokio::test]
async fn caller_driven_run_settles_and_cancels_while_real_renderer_is_gated() {
    // The independent watchdog releases a stalled renderer even when an
    // inline render/held mutex prevents this Tokio thread polling a timeout.
    struct ReleaseGate(Box<dyn Fn()>);
    impl Drop for ReleaseGate {
        fn drop(&mut self) {
            (self.0)();
        }
    }
    for cancel in [false, true] {
        let (server, started, release_response) = HeldApi::start(text_turn()).await;
        let (_workspace, mut agent) =
            scripted_agent_for_route(scripted_model(&server.uri), octet_ai::AiClient::new());
        let (mut shell, gate) = InteractiveShell::test_blocked_renderer();
        // Declared after shell: always releases before shell's join on unwind.
        let release_gate = gate.clone();
        let _release = ReleaseGate(Box::new(move || release_gate.release()));
        let (settled_tx, settled_rx) = std::sync::mpsc::channel();
        let watchdog_gate = gate.clone();
        let watchdog = std::thread::spawn(move || {
            let settled_while_blocked = settled_rx.recv_timeout(Duration::from_secs(5)).is_ok();
            watchdog_gate.release();
            settled_while_blocked
        });
        assert!(gate.wait_until_entered(Duration::from_secs(3)));
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("settle independently of paint").await.unwrap();
        let control = run.control();
        shell.set_awaiting_provider(run_id);
        let stimulus = async move {
            started.await.unwrap();
            if cancel {
                // Empty-draft Ctrl+C must reach the real Agent control and
                // the caller must continue polling through RunFinished.
                sender.send(Ok(ctrl_key('c'))).await.unwrap();
                let _keep_response_held = release_response;
                std::future::pending::<()>().await;
            } else {
                release_response.send(true).unwrap();
                let _keep_input_open = sender;
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(stimulus);
        let mut made_tool_call = false;
        let mut deadline = None;
        let result = tokio::select! {
            result = drive_active_run(
                &mut run, &control, &mut shell, &mut input, &mut ticker,
                &mut pending, &mut quit, None, None, &mut extensions,
                &mut made_tool_call, test_run_inspection(), &mut deadline,
            ) => result.unwrap(),
            _ = &mut stimulus => unreachable!(),
        };
        assert_eq!(
            result,
            if cancel {
                HostRunOutcome::Aborted
            } else {
                HostRunOutcome::Completed
            }
        );
        assert!(
            run.next().await.is_none(),
            "driver must consume the terminal outcome"
        );
        drop(run);
        assert_eq!(agent.session().checkpoints().len(), 1);
        assert!(pending.is_empty());
        assert!(!quit);
        settled_tx.send(()).ok();
        assert!(
            watchdog.join().unwrap(),
            "Run only settled after the renderer watchdog released layout: cancel={cancel}"
        );
    }
}
