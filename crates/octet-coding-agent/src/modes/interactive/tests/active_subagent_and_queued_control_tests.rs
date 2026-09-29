//! Active subagent stop ownership and the barrier that keeps a queued control from
//! changing the wire while a run holds the provider.
//! Separate because both probes exist to pin down who may cancel an in-flight run.

use super::*;

use super::support::*;

/// While the root provider is held, `/extensions` opens the live worker
/// list and Ctrl+X stops the selected worker through the owner-bound
/// first-party stop path; input and cancellation stay responsive while the
/// stop response is pending.
#[cfg(unix)]
#[tokio::test]
async fn active_subagent_stops_are_owned_and_survive_root_completion() {
    for (worker, owner_mode) in [
        ("running", "owned"),
        ("running", "slow"),
        ("running", "cancel"),
        ("running", "after-completion-abort"),
        ("running", "missing"),
        ("running", "wrong"),
        ("running", "impostor"),
        ("settled", "owned"),
    ] {
        let (server, started, release) = HeldApi::start(text_turn()).await;
        let (_agent_dir, mut agent) =
            scripted_agent_for_route(scripted_model(&server.uri), octet_ai::AiClient::new());
        let mut inspection = test_run_inspection().clone();
        inspection.session_path = agent.session().path().to_path_buf();
        let owner = agent.session().resource_owner_key();
        inspection.resource_owner = owner.clone();
        let fixture_dir = tempfile::tempdir().unwrap();
        let impostor = owner_mode == "impostor";
        let (mut extensions, process, log) =
            crate::extensions::ExecutableExtensions::test_subagent_stop_fixture(
                fixture_dir.path(),
                match owner_mode {
                    "missing" => None,
                    "wrong" => Some("another-session"),
                    _ => Some(&owner),
                },
                if impostor {
                    "other-extension"
                } else {
                    "octet-subagents"
                },
                match owner_mode {
                    "slow" => 6,
                    "after-completion-abort" => 2,
                    _ => 1,
                },
            )
            .await;
        extensions.test_publish_worker_roster(
            &process,
            &[(
                "worker:one",
                "worker-one",
                (worker == "running").then_some("worker-one"),
            )],
        );
        let key = |code, modifiers| -> std::io::Result<Event> {
            Ok(Event::Key(crossterm::event::KeyEvent::new(code, modifiers)))
        };
        // Another extension's roster never becomes the worker list, so the
        // menu waits for idle behind a report that the next key dismisses.
        let opening = vec![
            Ok(Event::Paste("/extensions".into())),
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Char('x'), KeyModifiers::CONTROL),
        ];
        let closing = if impostor {
            vec![key(KeyCode::Char('x'), KeyModifiers::NONE)]
        } else {
            vec![
                key(KeyCode::Esc, KeyModifiers::NONE),
                key(KeyCode::Char('x'), KeyModifiers::NONE),
            ]
        };
        let after_completion_abort = owner_mode == "after-completion-abort";
        let owned = worker == "running"
            && matches!(
                owner_mode,
                "owned" | "slow" | "cancel" | "after-completion-abort"
            );
        let (sender, receiver) = tokio::sync::mpsc::channel(128);
        let (handled_tx, handled) = tokio::sync::oneshot::channel();
        let mut input = ProbedInput {
            input: tokio_stream::wrappers::ReceiverStream::new(receiver),
            remaining: opening.len()
                + closing.len()
                + usize::from(owner_mode == "cancel")
                + 2 * usize::from(after_completion_abort),
            handled: Some(handled_tx),
        };
        let mut shell = InteractiveShell::test_shell();
        let run_id = shell.begin_run("test");
        let run_active = shell.test_run_active_probe();
        let mut run = agent.prompt("held turn").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut ticker = tokio::time::interval(Duration::from_millis(16));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut made_tool_call = false;
        let mut deadline = None;
        let driver = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut extensions,
            &mut made_tool_call,
            &inspection,
            &mut deadline,
        );
        let producer = async {
            let mut release = Some(release);
            started.await.unwrap();
            for event in opening {
                sender.send(event).await.unwrap();
            }
            if owned {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !log.exists() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("stop request reached the registered extension");
            }
            for event in closing {
                sender.send(event).await.unwrap();
            }
            if owner_mode == "cancel" {
                sender
                    .send(key(KeyCode::Esc, KeyModifiers::NONE))
                    .await
                    .unwrap();
            }
            if after_completion_abort {
                sender
                    .send(key(KeyCode::Enter, KeyModifiers::NONE))
                    .await
                    .unwrap();
                release.take().unwrap().send(true).unwrap();
                tokio::time::timeout(Duration::from_secs(1), async {
                    while run_active() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("root settled before stop response and Ctrl+C");
                sender
                    .send(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
                    .await
                    .unwrap();
            }
            tokio::time::timeout(Duration::from_millis(500), handled)
                .await
                .expect("input/cancel stays responsive while stop response is pending")
                .expect("input handler completed while waiting for the stop response");
            if let Some(release) = release {
                let _ = release.send(true);
            }
        };
        let (ended, ()) = tokio::time::timeout(Duration::from_secs(8), async {
            tokio::join!(driver, producer)
        })
        .await
        .expect("active run and stop should settle");
        drop(run);
        let case = format!("{worker} {owner_mode}");
        assert_eq!(
            ended.unwrap(),
            if owner_mode == "cancel" {
                HostRunOutcome::Aborted
            } else {
                HostRunOutcome::Completed
            },
            "{case}"
        );
        if after_completion_abort {
            assert!(shell.pending().is_empty());
            assert_eq!(shell.queued_follow_up_len(), 1);
            assert!(
                shell.take_ready_follow_up().is_none(),
                "Ctrl+C must revoke dispatch after root settlement"
            );
        } else {
            assert_eq!(shell.pending(), "x", "{case}");
        }
        assert!(!quit);
        assert_eq!(
            pending.len(),
            usize::from(impostor),
            "{case}: only an unanswered /extensions waits for idle"
        );
        // The open list polls read-only `status`; only stops matter here.
        let wire = std::fs::read_to_string(&log).unwrap_or_default();
        let commands: Vec<serde_json::Value> = wire
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|command| command["params"]["arguments"][0] != "status")
            .collect();
        if owned {
            assert_eq!(
                commands.len(),
                1,
                "{case}: {wire}; error={:?}; frame={:?}; transcript={}",
                shell.debug_error(),
                shell.dump_rendered_frame().await,
                shell.debug_snapshot()
            );
            assert_eq!(
                commands[0]["params"]["arguments"],
                serde_json::json!(["stop", "worker-one"])
            );
            assert_eq!(
                commands[0]["params"]["context"]["resource_owner"]["session_id"],
                owner
            );
            let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
            assert!(frame.contains("not settled"), "{case}: {frame}");
        } else {
            assert!(commands.is_empty(), "unexpected command for {case}: {wire}");
        }
        assert!(process.shutdown().await);
    }
}

/// Input is acknowledged while the first response is held by a gate. The
/// running request keeps its tier; only a subsequent run sees the change.
#[tokio::test]
async fn fast_active_commands_wait_for_ownership_before_changing_wire_requests() {
    use crossterm::event::KeyEvent;
    for initially_on in [false, true] {
        let (server, started, release) = HeldApi::start_with_repeat(fast_response(), true).await;
        let (_directory, mut app) = fast_test_app(scripted_codex_model(&server.uri));
        let mut shell = InteractiveShell::test_shell();
        if initially_on {
            app = fast_idle(app, &mut shell, "/fast on").await;
        }
        update_status(&mut shell, &app);
        let inspection = ActiveRunInspection::capture(&app);
        let command = if initially_on {
            "/fast off"
        } else {
            "/fast on"
        };
        let events: Vec<_> = [command, "/fast"]
            .into_iter()
            .flat_map(|text| {
                text.chars()
                    .map(|character| KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                    .chain(std::iter::once(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE,
                    )))
            })
            .collect();
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        let (handled_tx, handled) = tokio::sync::oneshot::channel();
        let mut input = ProbedInput {
            input: tokio_stream::wrappers::ReceiverStream::new(receiver),
            remaining: events.len(),
            handled: Some(handled_tx),
        };
        let mut pending = VecDeque::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut quit = false;
        let mut made_tool_call = false;
        let mut deadline = None;
        let run_id = shell.begin_run("test");
        let mut run = app.agent.prompt("held first turn").await.unwrap();
        let control = run.control();
        shell.set_awaiting_provider(run_id);
        let driver = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut app.executable_extensions,
            &mut made_tool_call,
            &inspection,
            &mut deadline,
        );
        let producer = async {
            started.await.unwrap();
            for event in events {
                sender.send(Ok(Event::Key(event))).await.unwrap();
            }
            handled
                .await
                .expect("slash commands handled before releasing response");
            assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
            release.send(true).unwrap();
        };
        let (ended, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(driver, producer)
        })
        .await
        .unwrap();
        drop(run);
        assert_eq!(ended.unwrap(), HostRunOutcome::Completed);
        assert!(!quit);
        assert_eq!(
            app.agent.service_tier(),
            initially_on.then_some(octet_ai::ServiceTier::Priority)
        );
        assert_eq!(
            pending.len(),
            1,
            "read-only status must not enqueue another toggle"
        );
        assert!(shell
            .debug_snapshot()
            .contains("queued for the next idle boundary"));
        let PendingIdleAction::Fast(enabled) = pending.pop_front().unwrap() else {
            panic!("wrong action")
        };
        assert_eq!(enabled, !initially_on);
        // The production idle queue uses this same handler after Run drops.
        apply_fast_command(&mut app, &mut shell, Some(enabled));
        assert!(pending.is_empty());
        app.agent
            .complete("second turn after idle change")
            .await
            .unwrap();
        let bodies = server.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        for (body, priority) in bodies.iter().zip([initially_on, !initially_on]) {
            if priority {
                assert_eq!(body["service_tier"], "priority");
            } else {
                assert!(body.get("service_tier").is_none());
            }
            assert!(!body.to_string().contains("/fast"));
        }
    }
}
