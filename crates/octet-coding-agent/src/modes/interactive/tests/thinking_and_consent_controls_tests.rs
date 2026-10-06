//! Thinking and model controls, modal ownership under escape/Ctrl-C, tool-consent
//! freshness, and ordered undelivered steering across cancellation.
//! Separate because these probes all assert an ordering guarantee between a control
//! request and the run it was issued during.

use super::*;

use super::support::*;

pub(super) fn reasoning_control_model(uri: &str) -> Model {
    let mut model = scripted_model(uri);
    let spec = Arc::make_mut(&mut model.spec);
    spec.protocol = octet_ai::Protocol::OpenAiResponses;
    spec.capabilities
        .responses_features
        .reasoning_effort_updates = true;
    spec.capabilities.reasoning = Some(octet_ai::ReasoningCapability {
        options: Some(octet_ai::types::ReasoningOptions {
            values: vec!["none".into(), "high".into(), "low".into()],
            default: Some("low".into()),
        }),
        control: octet_ai::ReasoningControl::Effort,
        exposes_text: true,
        preserves_state: true,
        effort_budgets: None,
        openai_chat_mode: octet_ai::OpenAiChatReasoningMode::Standard,
        min_effort: octet_ai::ReasoningEffort::Low,
        max_effort: octet_ai::ReasoningEffort::High,
    });
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_features
        .reasoning_effort_updates = true;
    model
}

/// The cycle gesture walks the advertised levels in ascending order and
/// wraps from the last one back to the first.
#[test]
fn thinking_cycle_walks_the_advertised_levels_in_ascending_order() {
    let model = reasoning_control_model("http://127.0.0.1:1");
    let levels = supported_levels_with_subagents(&model, false);
    assert_eq!(
        levels,
        vec![ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::High]
    );

    // From the first level the walk ascends one step per press and wraps.
    // `levels[0]` is the current level, so the first press yields
    // `levels[1]`, and the press after the last returns to `levels[0]`.
    let mut current = Some(octet_ai::ReasoningConfig::Off);
    let mut visited = Vec::new();
    for _ in 0..levels.len() {
        let level = next_thinking_level(&levels, current.as_ref(), &model).unwrap();
        visited.push(level);
        current = Some(requested_thinking_to_reasoning(level, &model, false).unwrap());
    }
    let mut expected = levels.clone();
    expected.rotate_left(1);
    assert_eq!(visited, expected, "each press advances ascending");
    // `visited` ends on the first level, so the walk wrapped from the last
    // advertised level rather than stalling at the top.
    assert_eq!(
        visited.last().copied(),
        Some(levels[0]),
        "the walk must wrap past the last level"
    );
}

/// A selection with no portable level is a start position, not a failure.
///
/// The active path used to compare the footer's display string, so a token
/// budget matched nothing and the press silently did nothing; the idle path
/// propagated the translation error out of the interactive loop.
#[test]
fn thinking_cycle_advances_from_a_selection_without_a_portable_level() {
    let model = reasoning_control_model("http://127.0.0.1:1");
    let levels = supported_levels_with_subagents(&model, false);

    // `Off` on an effort-only model has a level, so it advances normally.
    assert_eq!(
        next_thinking_level(&levels, Some(&octet_ai::ReasoningConfig::Off), &model).unwrap(),
        ThinkingLevel::Low
    );
    // An absent selection starts at the first advertised level.
    assert_eq!(
        next_thinking_level(&levels, None, &model).unwrap(),
        levels[0]
    );
    // A budget this model does not publish has no portable level. The press
    // must still land on an advertised level instead of erroring or
    // matching nothing.
    let budget = octet_ai::ReasoningConfig::Budget(999_999);
    assert_eq!(
        next_thinking_level(&levels, Some(&budget), &model).unwrap(),
        levels[0]
    );
    // A level the model no longer advertises behaves the same way.
    let unlisted = octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra);
    assert_eq!(
        next_thinking_level(&levels, Some(&unlisted), &model).unwrap(),
        levels[0]
    );
    // A model with no levels at all still reports the single honest error.
    assert!(next_thinking_level(&[], None, &model).is_err());
}

#[tokio::test]
async fn thinking_control_preserves_active_run_and_hands_off_wire_update() {
    // Exercise real preference persistence without modifying the developer HOME.
    const CHILD: &str = "OCTET_TEST_REASONING_CONTROL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "modes::interactive::tests::thinking_and_consent_controls_tests::thinking_control_preserves_active_run_and_hands_off_wire_update", "--nocapture"])
            .env(CHILD, "1")
            .env("HOME", home.path());
        // `dirs::home_dir()` ignores `HOME` on Windows (it reads
        // `USERPROFILE`), so isolate that too; otherwise the child
        // observes and mutates the developer's real profile.
        #[cfg(windows)]
        command.env("USERPROFILE", home.path());
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        return;
    }
    use crossterm::event::KeyEvent;
    for (requested, qualified) in [
        ("high", true),
        ("medium", true),
        ("ultra", true),
        ("high", false),
        ("cycle", true),
    ] {
        let accepted = (requested == "high" || requested == "cycle") && qualified;
        let (server, started, release) = HeldApi::start_with_repeat(fast_response(), true).await;
        let mut model = reasoning_control_model(&server.uri);
        Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_features
            .reasoning_effort_updates = qualified;
        let (_workspace, mut agent) =
            scripted_agent_for_route(model.clone(), octet_ai::AiClient::new());
        let mut inspection = test_run_inspection().clone();
        inspection.model = model;
        inspection.session_path = agent.session().path().to_path_buf();
        let mut shell = InteractiveShell::test_shell();
        shell.set_identity("test", "scripted", "off");
        let events: Vec<_> = if requested == "cycle" {
            (0..2)
                .map(|_| Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)))
                .collect()
        } else {
            format!("/thinking {requested}")
                .chars()
                .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
                .chain(std::iter::once(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))))
                .collect()
        };
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
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let id = shell.begin_run("test");
        let mut run = agent.prompt("keep the root alive").await.unwrap();
        let control = run.control();
        shell.set_awaiting_provider(id);
        let mut deadline = None;
        let mut made_tool_call = false;
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
        let stimulus = async move {
            started.await.unwrap();
            for event in events {
                sender.send(Ok(event)).await.unwrap();
            }
            handled.await.unwrap();
            release.send(true).unwrap();
            std::future::pending::<()>().await;
        };
        tokio::pin!(stimulus);
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! { result = driver => result.unwrap(), _ = &mut stimulus => unreachable!() }
    }).await.unwrap();
        assert_eq!(outcome, HostRunOutcome::Completed);
        drop(run);
        assert!(!quit);
        if qualified && accepted {
            assert_eq!(
                pending,
                VecDeque::from([PendingIdleAction::PersistThinkingPreference(
                    "high".to_owned()
                )]),
                "active updates defer only the final preference write until idle"
            );
            // The re-exec child isolates `HOME`, but on Windows
            // `dirs::home_dir()` resolves through `SHGetKnownFolderPath`,
            // which no environment variable redirects, so this check can
            // only observe an isolated home on Unix. Deferral itself is
            // asserted through `pending` on all platforms above.
            #[cfg(not(windows))]
            assert!(
                !crate::cli::global_config_path().unwrap().exists(),
                "active control handling must not persist before the idle boundary"
            );
        } else if qualified {
            assert!(pending.is_empty(), "rejected control must not persist");
        } else {
            assert_eq!(
                pending.front(),
                Some(&PendingIdleAction::ChangeThinkingLevel(ThinkingLevel::High))
            );
        }
        assert_eq!(agent.session().checkpoints().len(), 1);
        assert_eq!(
            agent.reasoning(),
            &if accepted {
                ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
            } else {
                ReasoningConfig::Off
            }
        );
        assert_eq!(
            shell.selected_identity().1,
            if accepted { "high" } else { "off" }
        );
        let bodies = server.bodies.lock().unwrap();
        if !accepted {
            assert_eq!(
                bodies.len(),
                1,
                "rejected effort must not create a model response"
            );
            assert!(!agent.session().entries().iter().any(|entry| matches!(
                &entry.value,
                EntryValue::ResponsesReasoning {
                    update: Some(_),
                    ..
                }
            )));
            if qualified {
                assert!(shell.debug_error().unwrap().contains("not supported"));
            } else {
                assert!(shell.debug_error().is_none());
                assert!(shell.debug_snapshot().contains("next idle boundary"));
            }
            continue;
        }
        if requested != "cycle" {
            assert!(shell
                .debug_snapshot()
                .contains("not provider acknowledgement"));
        }
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["reasoning"]["effort"], "none");
        assert_eq!(
            bodies[1]["reasoning"]["effort"], "none",
            "wire baseline stays pinned"
        );
        assert!(bodies[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "configuration_update"
                && item["reasoning"]["effort"] == "high"));
    }
}

#[tokio::test]
async fn idle_thinking_rejection_preserves_session_and_startup_preference() {
    const CHILD: &str = "OCTET_TEST_IDLE_THINKING_PREFERENCE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "modes::interactive::tests::thinking_and_consent_controls_tests::idle_thinking_rejection_preserves_session_and_startup_preference", "--nocapture"])
            .env(CHILD, "1").env("HOME", home.path()).output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        return;
    }
    let mut model = reasoning_control_model("http://127.0.0.1:1");
    let capabilities = &mut Arc::make_mut(&mut model.spec).capabilities;
    capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
    let capability = capabilities.reasoning.as_mut().unwrap();
    capability.max_effort = octet_ai::ReasoningEffort::Ultra;
    capability
        .options
        .as_mut()
        .unwrap()
        .values
        .extend(["max".into(), "ultra".into()]);
    // Metadata alone cannot authorize Ultra: the observation runtime must
    // be installed before a selection is committed.
    let ultra = requested_thinking_to_reasoning(ThinkingLevel::Ultra, &model, true).unwrap();
    let (_workspace, mut app) = fast_test_app(model);
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending();
    app = select_thinking(
        app,
        &mut shell,
        &mut input,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        None,
    )
    .await
    .unwrap();
    let preference = crate::cli::global_config_path().unwrap();
    let config_before = std::fs::read(&preference).unwrap();
    assert!(String::from_utf8_lossy(&config_before).contains("high"));
    let session = app.agent.session().path().to_path_buf();
    let session_before = std::fs::read(&session).unwrap();
    let identity = shell.selected_identity();
    // None is the slash/shortcut path; Some(Standard) is picker selection.
    for mode in [None, Some((ReasoningMode::Standard, ThinkingLevel::Ultra))] {
        shell.clear_error();
        app = select_thinking(app, &mut shell, &mut input, ultra.clone(), mode)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&preference).unwrap(), config_before);
        assert_eq!(std::fs::read(&session).unwrap(), session_before);
        assert_eq!(shell.selected_identity(), identity);
        assert_eq!(
            app.reasoning,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
        );
        assert_eq!(app.agent.reasoning(), &app.reasoning);
        let error = shell.debug_error().unwrap();
        assert!(
            error.contains("thinking unchanged") && error.contains("observation runtime"),
            "{error}"
        );
    }
    app.agent
        .enable_v2_delegation_extension_only(octet_agent::DelegationConfig::new(
            _workspace.path().join("delegation"),
        ))
        .unwrap();
    let team = app.agent.delegation_team_directory().unwrap().to_path_buf();
    for level in [
        ThinkingLevel::Max,
        ThinkingLevel::Ultra,
        ThinkingLevel::Off,
        ThinkingLevel::Low,
        ThinkingLevel::Max,
        ThinkingLevel::Ultra,
        ThinkingLevel::Low,
    ] {
        let reasoning = requested_thinking_to_reasoning(level, &app.model, true).unwrap();
        shell.clear_error();
        app = select_thinking(app, &mut shell, &mut input, reasoning.clone(), None)
            .await
            .unwrap();
        assert!(shell.debug_error().is_none(), "{:?}", shell.debug_error());
        assert_eq!(app.reasoning, reasoning);
        assert_eq!(app.agent.reasoning(), &reasoning);
        assert_eq!(app.agent.session().path(), session);
        assert_eq!(app.agent.delegation_team_directory(), Some(team.as_path()));
        assert!(std::fs::read_to_string(&preference)
            .unwrap()
            .contains(level.label()));
    }
}

#[tokio::test]
async fn thinking_control_idle_is_durable_and_rejected_effort_leaves_session_unchanged() {
    let model = reasoning_control_model("http://127.0.0.1:1");
    let (_workspace, mut app) = fast_test_app(model.clone());
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending();
    let session = app.agent.session().path().to_path_buf();
    app = transition(
        app,
        &mut shell,
        &mut input,
        Reconfig::Thinking(ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)),
    )
    .await
    .unwrap();
    assert_eq!(app.agent.session().path(), session);
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    assert_eq!(shell.selected_identity().1, "high");
    let before = std::fs::read(&session).unwrap();
    for level in [ThinkingLevel::Medium, ThinkingLevel::Ultra] {
        assert!(requested_thinking_to_reasoning(level, &app.model, false).is_err());
    }
    app = transition(
        app,
        &mut shell,
        &mut input,
        Reconfig::Thinking(ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium)),
    )
    .await
    .unwrap();
    assert_eq!(app.model.spec.id, model.spec.id);
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    assert_eq!(std::fs::read(&session).unwrap(), before);
    assert!(shell.debug_error().unwrap().contains("thinking unchanged"));
    let resumed = Session::open_read_only(&session).unwrap();
    assert_eq!(
        resumed
            .responses_reasoning(&model.endpoint.id, &model.spec.id)
            .unwrap()
            .unwrap()
            .1,
        app.reasoning
    );
    let mut codex = model;
    Arc::make_mut(&mut codex.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap()
        .options
        .as_mut()
        .unwrap()
        .values
        .remove(0);
    assert!(requested_thinking_to_reasoning(ThinkingLevel::Off, &codex, false).is_err());
    Arc::make_mut(&mut codex.endpoint)
        .runtime
        .responses_features = Default::default();
    assert!(
        !codex.responses_features().reasoning_effort_updates,
        "unknown routes keep selector fallback"
    );
}

#[tokio::test]
async fn active_model_and_thinking_panels_do_not_suspend_run() {
    use crossterm::event::KeyEvent;
    for command in ["/model", "/thinking"] {
        let (server, started, release) = HeldApi::start(text_turn()).await;
        let (_workspace, mut agent) =
            scripted_agent_for_route(scripted_model(&server.uri), octet_ai::AiClient::new());
        let mut inspection = test_run_inspection().clone();
        inspection
            .catalog
            .register_endpoint((*inspection.model.endpoint).clone())
            .unwrap();
        inspection
            .catalog
            .register_model((*inspection.model.spec).clone())
            .unwrap();
        let mut shell = InteractiveShell::test_shell();
        let events: Vec<_> = command
            .chars()
            .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
            .chain(std::iter::once(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
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
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent
            .prompt("complete while the picker remains open")
            .await
            .unwrap();
        let control = run.control();
        shell.set_awaiting_provider(run_id);
        let mut deadline = None;
        let mut made_tool_call = false;
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
        let stimulus = async move {
            started.await.unwrap();
            for event in events {
                sender.send(Ok(event)).await.unwrap();
            }
            handled.await.unwrap();
            // No Escape, Enter, or EOF follows opening the panel. This
            // failed previously because the modal stopped polling Run.
            release.send(true).unwrap();
            std::future::pending::<()>().await;
        };
        tokio::pin!(stimulus);
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! { result = driver => result.unwrap(), _ = &mut stimulus => unreachable!() }
        }).await.expect("an open picker must not stall settlement");
        assert_eq!(result, HostRunOutcome::Completed, "{command}");
        assert!(run.next().await.is_none());
        drop(run);
        assert_eq!(agent.session().checkpoints().len(), 1);
        assert!(
            pending.is_empty(),
            "opening or settlement must not imply selection"
        );
        assert!(!quit);
        assert!(
            !shell.has_panel(),
            "settlement cannot leave a driverless modal"
        );
        assert!(shell.debug_snapshot().contains("done"));
    }
}

#[tokio::test]
async fn active_modal_escape_ctrl_c_and_close_keep_their_owners() {
    use crossterm::event::KeyEvent;
    for (draft, keys, expected, closing) in [
        (
            "draft",
            vec![
                ctrl_key('c'),
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ],
            HostRunOutcome::Completed,
            false,
        ),
        ("", vec![ctrl_key('c')], HostRunOutcome::Aborted, false),
        ("draft", vec![ctrl_key('d')], HostRunOutcome::Aborted, true),
    ] {
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_millis(100)).await;
        let mut shell = InteractiveShell::test_shell();
        shell.extension_set_editor(draft.into());
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender.send(Ok(ctrl_key('l'))).await.unwrap();
        for key in keys {
            sender.send(Ok(key)).await.unwrap();
        }
        let _sender = sender;
        let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        let control = run.control();
        shell.set_awaiting_provider(run_id);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            drive_active_run(
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
                &mut false,
                test_run_inspection(),
                &mut None,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, expected);
        assert_eq!(quit, closing);
        assert_eq!(shell.pending(), if closing { draft } else { "" });
        assert!(pending.is_empty());
    }
}

#[tokio::test]
async fn active_tool_consent_requires_a_visible_fresh_confirmation_and_drop_denies() {
    use crossterm::event::{KeyEvent, KeyEventKind};
    let (sink, mut progress) = ToolProgressSink::bounded_channel();
    let answer = tokio::spawn(async move {
        sink.confirmation(
            "Approve fixture effect?".into(),
            Some("Consequence retained".into()),
            true,
            true,
        )
        .await
    });
    let ToolProgress::Confirmation(request) = progress.recv().await.unwrap() else {
        panic!("confirmation")
    };
    let mut interaction = ActiveToolInteraction {
        id: ToolCallId("fixture".into()),
        tool: Some("write".into()),
        request: ActiveToolRequest::Confirmation(request),
    };
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    interaction.open(&mut shell);
    assert!(!interaction.input(
        &mut shell,
        &Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Repeat
        ))
    ));
    assert!(!answer.is_finished(), "repeat must not approve");
    shell.set_size(1, 1);
    assert!(!interaction.input(
        &mut shell,
        &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    ));
    assert!(!answer.is_finished(), "invisible action must not approve");
    drop(interaction);
    assert!(
        !answer.await.unwrap(),
        "cancel/settlement/error must deny unanswered requests"
    );
}

#[tokio::test]
async fn cancellation_retains_answer_draft_and_ordered_undelivered_steering() {
    use crossterm::event::KeyEvent;
    let (_server, _workspace, mut agent) = scripted_agent_with_delay(Duration::from_secs(2)).await;
    let mut shell = InteractiveShell::test_shell();
    let events = [
        Event::Paste("first queued".into()),
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Event::Paste("second queued".into()),
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Event::Paste("/answer preserve this instruction".into()),
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        // Repeated close/submit keys must not duplicate or drain the draft.
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )),
    ];
    let mut input =
        tokio_stream::iter(events.into_iter().map(Ok)).chain(futures_util::stream::pending());
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut ticker = tokio::time::interval(Duration::from_millis(16));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let mut goal_deadline = None;
    let ended = tokio::time::timeout(
        Duration::from_secs(1),
        drive_active_run(
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
            &mut false,
            test_run_inspection(),
            &mut goal_deadline,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    drop(run);
    assert_eq!(ended, HostRunOutcome::Aborted);
    assert_eq!(
        shell.pending(),
        "first queued\n\nsecond queued\n\n/answer preserve this instruction"
    );
    assert!(!shell.debug_snapshot().contains("Steering:"));
    assert_eq!(agent.session().checkpoints().len(), 1);
    assert_eq!(
        agent.session().context().unwrap().len(),
        1,
        "undelivered input is not durable or replayed"
    );
}
