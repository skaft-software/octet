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

#[test]
fn thinking_cycle_stays_instant_during_lifecycle_waits_including_ultra() {
    use crossterm::event::KeyEvent;
    let choices = vec![
        ReasoningConfig::Off,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Max),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra),
    ];
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "test", "off");
    shell.prefill_editor("unsent draft".into());
    shell.set_thinking_cycle(
        "session:model".into(),
        choices.clone(),
        &ReasoningConfig::Off,
    );
    for index in 1..=257 {
        assert!(!handle_cancellable_wait_input(
            &mut shell,
            Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))
        ));
        let expected = &choices[index % choices.len()];
        assert_eq!(shell.deferred_thinking(), Some(expected));
        assert_eq!(
            shell.selected_identity().1,
            format!("{} (queued)", reasoning_label(expected))
        );
        assert_eq!(shell.pending(), "unsent draft");
        assert!(!shell
            .debug_snapshot()
            .contains("draft kept for the next prompt"));
        // A background status refresh must not rewind the cycle cursor.
        shell.set_thinking_cycle(
            "session:model".into(),
            choices.clone(),
            &ReasoningConfig::Off,
        );
    }
    assert_eq!(
        shell.take_deferred_thinking(),
        Some(choices[257 % choices.len()].clone())
    );
    assert!(shell.take_deferred_thinking().is_none());
    shell.cycle_thinking_during_wait();
    shell.set_thinking_cycle("new-session:model".into(), choices, &ReasoningConfig::Off);
    assert!(
        shell.deferred_thinking().is_none(),
        "selection must not cross a session switch"
    );
}

#[test]
fn idle_thinking_selections_coalesce_without_crossing_ordering_barriers() {
    let low = ThinkingLevel::Low;
    let high = ThinkingLevel::High;
    let mut queue = VecDeque::new();
    for _ in 0..1000 {
        assert!(stage_idle_thinking(&mut queue, low));
        assert!(stage_idle_thinking(&mut queue, high));
    }
    assert_eq!(
        queue,
        VecDeque::from([PendingIdleAction::ChangeThinkingLevel(high)])
    );
    queue.push_back(PendingIdleAction::NewSession);
    assert!(stage_idle_thinking(&mut queue, low));
    assert_eq!(queue.len(), 3);
    assert_eq!(queue[1], PendingIdleAction::NewSession);
    let mut full = VecDeque::from(vec![
        PendingIdleAction::NewSession;
        MAX_PENDING_IDLE_ACTIONS
    ]);
    assert!(!stage_idle_thinking(&mut full, low));
    full.pop_back();
    assert!(stage_idle_thinking(&mut full, low));
    assert!(
        stage_idle_thinking(&mut full, high),
        "a replacement fits a full queue"
    );
    assert_eq!(full.len(), MAX_PENDING_IDLE_ACTIONS);
}

#[test]
fn settling_active_thinking_preserves_a_newer_idle_selection_after_a_barrier() {
    let inspection = test_run_inspection();
    let mut shell = InteractiveShell::test_shell();
    let mut queue = VecDeque::from([
        PendingIdleAction::ActiveThinkingSelection(ReasoningConfig::Effort(
            octet_ai::ReasoningEffort::Low,
        )),
        PendingIdleAction::NewSession,
        PendingIdleAction::ChangeThinkingLevel(ThinkingLevel::High),
    ]);
    shell.set_identity("cerebras", "test-model", "high (queued)");

    settle_active_thinking(&mut shell, inspection, &mut queue);

    assert_eq!(shell.selected_identity().1, "high (queued)");
    assert!(matches!(queue.get(1), Some(PendingIdleAction::NewSession)));
}

#[tokio::test]
async fn deferred_thinking_revalidates_availability_at_the_safe_boundary() {
    use crossterm::event::KeyEvent;
    let (server, _started, _release) = HeldApi::start(fast_response()).await;
    let (_workspace, app) = fast_test_app(reasoning_control_model(&server.uri));
    assert!(!app.subagents_available());
    let original = app.reasoning.clone();
    let head = app.agent.session().head_ref().cloned();
    let mut shell = InteractiveShell::test_shell();
    shell.set_thinking_cycle(
        format!(
            "{}:{}:{}",
            app.agent.session().resource_owner_key(),
            app.model.endpoint.id.0,
            app.model.spec.id.0
        ),
        vec![
            original.clone(),
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra),
        ],
        &original,
    );
    handle_cancellable_wait_input(
        &mut shell,
        Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
    );
    assert_eq!(
        shell.deferred_thinking(),
        Some(&ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra))
    );
    let mut input = futures_util::stream::empty::<std::io::Result<Event>>();

    let app = apply_deferred_thinking(app, &mut shell, &mut input)
        .await
        .unwrap();

    assert_eq!(app.reasoning, original);
    assert_eq!(app.agent.session().head_ref(), head.as_ref());
    assert!(shell.take_deferred_thinking().is_none());
    assert_eq!(shell.selected_identity().1, reasoning_label(&app.reasoning));
    assert_eq!(
        shell.debug_error().as_deref(),
        Some("thinking unchanged: selected level is no longer available")
    );
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 0);
}

// These probes exercise real preference writes and executable lifecycle hooks.
// HOME isolation is only valid on Unix; Windows Known Folders are not redirected
// by HOME/USERPROFILE, so do not run the persistence probes there.
#[cfg(unix)]
mod thinking_rebuild_regressions {
    use super::*;
    use crossterm::event::KeyEvent;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn isolated_child(name: &str) -> bool {
        const CHILD: &str = "OCTET_TEST_THINKING_REBUILD_CHILD";
        if std::env::var(CHILD).as_deref() == Ok(name) {
            return true;
        }
        let home = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("modes::interactive::tests::thinking_and_consent_controls_tests::thinking_rebuild_regressions::{name}"),
                "--nocapture",
            ])
            .env(CHILD, name)
            .env("HOME", home.path())
            // This complete unoptimized idle-dispatch + lifecycle fixture nests
            // finite poll frames beyond libtest's 2 MiB stack (LLDB verified).
            // Provision only its isolated child; this is not native-stack QA.
            .env("RUST_MIN_STACK", "8388608")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        false
    }

    fn shift_tab() -> Event {
        Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))
    }

    // An API 0.4 peer negotiates the actual lifecycle channel and holds one
    // session_end request. select_thinking therefore really enters rebuild_app's
    // release_binding wait; the test does not substitute a bare wait helper.
    const LIFECYCLE_PROBE: &str = r#"import json, os, sys, time
log, gate = sys.argv[1:]

def reply(request, result):
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        offer = request['params']['protocol']
        assert 'lifecycle_events_v2' in offer['optional_features']
        reply(request, {'api_version': '0.4', 'tools': [], 'commands': [],
            'protocol': {'version': '0.4',
                'features': offer['required_features'] + ['lifecycle_events_v2'],
                'limits': {'max_concurrent_requests': 1}}})
    elif method == 'hook/run':
        if request['params']['hook'] == 'session_end' and os.path.exists(gate):
            os.unlink(gate)
            with open(gate + '.entered', 'w') as marker:
                marker.write('session_end entered')
            deadline = time.monotonic() + 5
            while not os.path.exists(gate + '.release'):
                assert time.monotonic() < deadline, 'held lifecycle was not released'
                time.sleep(0.005)
        reply(request, {})
    elif method == 'reasoning/selected':
        with open(log, 'a') as trace:
            trace.write(json.dumps(request['params']) + '\n')
    elif method == 'shutdown':
        reply(request, {})
        break
"#;

    fn lifecycle_app(qualified: bool) -> (tempfile::TempDir, App, PathBuf, PathBuf) {
        let mut model = reasoning_control_model("http://127.0.0.1:1");
        Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_features
            .reasoning_effort_updates = qualified;
        let (directory, mut app) = fast_test_app(model);
        let root = directory.path().join("extensions");
        let extension = root.join("thinking-lifecycle-probe");
        std::fs::create_dir_all(&extension).unwrap();
        let script = extension.join("probe.py");
        let log = directory.path().join("reasoning-selected.jsonl");
        let gate = directory.path().join("hold-session-end");
        std::fs::write(&script, LIFECYCLE_PROBE).unwrap();
        std::fs::write(
            extension.join("extension.toml"),
            format!(
                r#"name = "thinking-lifecycle-probe"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{script}, {log}, {gate}]
[contributes]
hooks = ["session_start", "session_end"]
"#,
                script = serde_json::to_string(&script).unwrap(),
                log = serde_json::to_string(&log).unwrap(),
                gate = serde_json::to_string(&gate).unwrap(),
            ),
        )
        .unwrap();
        app.config.extension_paths = vec![root];
        app.config.enabled_extensions = vec!["thinking-lifecycle-probe".into()];
        app.config.invocation_trusted_extensions = vec!["thinking-lifecycle-probe".into()];
        app.config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
        app.config.sandbox.allow_process = true;
        app.config.sandbox.allow_shell = true;
        let mut host = octet_agent::ExtensionHost::new();
        let mut extensions = crate::extensions::ExecutableExtensions::discover_and_start(
            &app.config,
            app.agent.session(),
            &app.model,
            &app.reasoning,
            &app.sessions,
            &mut host,
        );
        assert!(
            extensions
                .summaries()
                .iter()
                .any(|summary| { summary.name == "thinking-lifecycle-probe" && summary.running }),
            "{}",
            extensions.inspect_text()
        );
        app.executable_extensions = extensions;
        (directory, app, log, gate)
    }

    fn selected_notifications(log: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_shift_tab_during_real_thinking_rebuild_advances_from_low_to_high() {
        if !isolated_child(
            "second_shift_tab_during_real_thinking_rebuild_advances_from_low_to_high",
        ) {
            return;
        }
        let (_directory, app, log, gate) = lifecycle_app(false);
        assert_eq!(app.reasoning, ReasoningConfig::Off);
        let mut shell = InteractiveShell::test_shell();
        update_status(&mut shell, &app);
        shell.prefill_editor("draft survives the rebuild".into());
        assert!(matches!(
            shell.translate_input(Some(shift_tab()), false),
            InputAction::CycleThinking
        ));
        // Match the real Idle::CycleThinking dispatch, including its effective
        // first selection. Only the following press belongs to the wait owner.
        let first =
            next_thinking_level(&app_thinking_levels(&app), Some(&app.reasoning), &app.model)
                .unwrap();
        assert_eq!(first, ThinkingLevel::Low);
        let low =
            requested_thinking_to_reasoning(first, &app.model, app.subagents_available()).unwrap();
        std::fs::write(&gate, "hold the next session_end").unwrap();
        let entered = gate.with_extension("entered");
        let release = gate.with_extension("release");
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        let (handled_tx, handled) = tokio::sync::oneshot::channel();
        let mut input = ProbedInput {
            input: tokio_stream::wrappers::ReceiverStream::new(receiver),
            remaining: 1,
            handled: Some(handled_tx),
        };
        let stimulus = async move {
            while !entered.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            sender.send(Ok(shift_tab())).await.unwrap();
            let processed = tokio::time::timeout(Duration::from_millis(250), handled).await;
            // Release even if the input-liveness assertion failed.
            std::fs::write(release, "release").unwrap();
            processed
                .expect("Shift+Tab must be handled while session_end is held")
                .unwrap();
            std::future::pending::<()>().await;
        };
        let driver = Box::pin(select_thinking(
            app,
            &mut shell,
            &mut input,
            low.clone(),
            None,
        ));
        let mut app = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                result = driver => result.unwrap(),
                _ = stimulus => unreachable!(),
            }
        })
        .await
        .expect("held thinking rebuild must settle");
        let high = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
        assert_eq!(app.reasoning, low);
        assert_eq!(shell.deferred_thinking(), Some(&high), "not Low again");
        assert_eq!(shell.selected_identity().1, "high (queued)");
        assert_eq!(shell.pending(), "draft survives the rebuild");
        let mut idle_input = futures_util::stream::pending::<std::io::Result<Event>>();
        app = Box::pin(apply_deferred_thinking(app, &mut shell, &mut idle_input))
            .await
            .unwrap();
        assert_eq!(app.reasoning, high);
        assert_eq!(app.agent.reasoning(), &high);
        assert!(shell.deferred_thinking().is_none());
        assert_eq!(shell.selected_identity().1, "high");
        app.executable_extensions.shutdown().await;
        assert_eq!(
            selected_notifications(&log),
            vec![
                serde_json::json!({"reasoning":"low"}),
                serde_json::json!({"reasoning":"high"})
            ],
            "each rebuild-backed selection must notify exactly once"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn qualified_idle_thinking_selection_notifies_exactly_once() {
        if !isolated_child("qualified_idle_thinking_selection_notifies_exactly_once") {
            return;
        }
        let (_directory, app, log, _gate) = lifecycle_app(true);
        let mut shell = InteractiveShell::test_shell();
        update_status(&mut shell, &app);
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        let low = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low);
        let mut app = select_thinking(app, &mut shell, &mut input, low.clone(), None)
            .await
            .unwrap();
        assert_eq!(app.reasoning, low);
        assert_eq!(shell.selected_identity().1, "low");
        app.executable_extensions.shutdown().await;
        assert_eq!(
            selected_notifications(&log).len(),
            1,
            "the qualified in-place path must notify exactly once"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn active_budget_cycle_resolves_after_queued_effort_model_switch() {
        if !isolated_child("active_budget_cycle_resolves_after_queued_effort_model_switch") {
            return;
        }
        budget_cycle_scenario().await;
    }

    // Keep the complete, two-run fixture's stored state on the heap as well
    // as provisioning the child thread for its unoptimized nested poll frames.
    fn budget_cycle_scenario() -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(async {
            // One press is portable Low; two presses select Medium, unavailable on
            // the target effort model. Both must let the following action drain.
            for presses in [1, 2] {
                let (server, started, release) = HeldApi::start(text_turn()).await;
                let mut budget = scripted_model(&server.uri);
                Arc::make_mut(&mut budget.spec).limits.context_window = 64_000;
                Arc::make_mut(&mut budget.spec).limits.max_output_tokens = 40_000;
                Arc::make_mut(&mut budget.spec).capabilities.reasoning =
                    Some(octet_ai::ReasoningCapability {
                        options: None,
                        control: octet_ai::ReasoningControl::TokenBudget,
                        exposes_text: true,
                        preserves_state: false,
                        effort_budgets: Some(octet_ai::ReasoningEffortBudgets {
                            minimal: 1024,
                            low: 2048,
                            medium: 4096,
                            high: 8192,
                            xhigh: 16384,
                            max: 32768,
                        }),
                        openai_chat_mode: octet_ai::OpenAiChatReasoningMode::Standard,
                        min_effort: octet_ai::ReasoningEffort::Low,
                        max_effort: octet_ai::ReasoningEffort::High,
                    });
                let (_directory, mut app) = fast_test_app(budget);
                let mut effort = reasoning_control_model(&server.uri);
                Arc::make_mut(&mut effort.spec).id = ModelId("effort-scripted".into());
                app.catalog.register_model((*effort.spec).clone()).unwrap();
                let mut shell = InteractiveShell::test_shell();
                update_status(&mut shell, &app);
                let inspection = ActiveRunInspection::capture(&app);
                let events: Vec<_> = "/model effort-scripted"
                    .chars()
                    .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
                    .chain(std::iter::once(Event::Key(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE,
                    ))))
                    .chain((0..presses).map(|_| shift_tab()))
                    .collect();
                let (sender, receiver) = tokio::sync::mpsc::channel(32);
                let (handled_tx, handled) = tokio::sync::oneshot::channel();
                let mut input = ProbedInput {
                    input: tokio_stream::wrappers::ReceiverStream::new(receiver),
                    remaining: events.len(),
                    handled: Some(handled_tx),
                };
                let mut queue = VecDeque::new();
                let mut ticker = tokio::time::interval(Duration::from_millis(1));
                let mut quit = false;
                let mut made_tool_call = false;
                let mut deadline = None;
                let id = shell.begin_run("held budget model");
                shell.set_awaiting_provider(id);
                let mut run = app
                    .agent
                    .prompt("keep the budget model alive")
                    .await
                    .unwrap();
                let control = run.control();
                let driver = Box::pin(drive_active_run(
                    &mut run,
                    &control,
                    &mut shell,
                    &mut input,
                    &mut ticker,
                    &mut queue,
                    &mut quit,
                    None,
                    None,
                    &mut app.executable_extensions,
                    &mut made_tool_call,
                    &inspection,
                    &mut deadline,
                ));
                let stimulus = async move {
                    started.await.unwrap();
                    for event in events {
                        sender.send(Ok(event)).await.unwrap();
                    }
                    handled.await.unwrap();
                    release.send(true).unwrap();
                    std::future::pending::<()>().await;
                };
                let outcome = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! { result = driver => result.unwrap(), _ = stimulus => unreachable!() }
            })
            .await
            .unwrap();
                drop(run);
                assert_eq!(outcome, HostRunOutcome::Completed);
                assert!(!quit);
                let level = if presses == 1 {
                    ThinkingLevel::Low
                } else {
                    ThinkingLevel::Medium
                };
                assert_eq!(
                    queue,
                    VecDeque::from([
                        PendingIdleAction::ChangeModel(effort.spec.id.clone()),
                        PendingIdleAction::ChangeThinkingLevel(level),
                    ])
                );
                assert_eq!(
                    shell.selected_identity().1,
                    if presses == 1 {
                        "budget=2048 (queued)"
                    } else {
                        "budget=4096 (queued)"
                    }
                );
                assert_eq!(app.reasoning, ReasoningConfig::Off);
                queue.push_back(PendingIdleAction::SyncImages(true));
                // No real tty is read by the default-type idle consumer. The cede
                // flag belongs to this stream, not the shell's terminal grant.
                let mut idle_input =
                    EventStream::new().with_cede_flag(Arc::new(AtomicBool::new(true)));
                let mut reload =
                    crate::reload::ReloadSupervisor::new(crate::reload::ReloadSettings::disabled());
                let mut pending_reexec = None;
                app = Box::pin(apply_pending_actions(
                    app,
                    &mut shell,
                    &mut idle_input,
                    &mut queue,
                    &mut deadline,
                    None,
                    &mut pending_reexec,
                    &mut reload,
                ))
                .await
                .expect("an unavailable queued level must not terminate the idle consumer");
                assert!(queue.is_empty());
                assert!(app.config.show_images, "the action after thinking must run");
                assert_eq!(app.model.spec.id, effort.spec.id);
                let expected = if presses == 1 {
                    ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low)
                } else {
                    ReasoningConfig::Off
                };
                assert_eq!(app.reasoning, expected);
                assert_eq!(app.agent.reasoning(), &expected);
                if presses == 1 {
                    assert!(shell.debug_error().is_none());
                    assert_eq!(shell.selected_identity().1, "low");
                } else {
                    assert!(shell.debug_error().unwrap().contains("thinking unchanged:"));
                    assert_eq!(shell.selected_identity().1, "off");
                }
                assert_eq!(
                    server.requests.load(Ordering::SeqCst),
                    1,
                    "queue application must not send a provider request"
                );
            }
        })
    }
}

#[tokio::test]
async fn unqualified_active_thinking_cycles_immediately_without_wire_mutation() {
    use crossterm::event::KeyEvent;
    for presses in [1, 2, 3, 257] {
        let (server, started, release) = HeldApi::start(fast_response()).await;
        let mut model = reasoning_control_model(&server.uri);
        Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_features
            .reasoning_effort_updates = false;
        let (_workspace, mut agent) =
            scripted_agent_for_route(model.clone(), octet_ai::AiClient::new());
        let mut inspection = test_run_inspection().clone();
        inspection.model = model;
        inspection.session_path = agent.session().path().to_path_buf();
        let mut shell = InteractiveShell::test_shell();
        shell.set_identity("test", "scripted", "off");
        shell.prefill_editor("draft remains".into());
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        let (handled_tx, handled) = tokio::sync::oneshot::channel();
        let mut input = ProbedInput {
            input: tokio_stream::wrappers::ReceiverStream::new(receiver),
            remaining: presses,
            handled: Some(handled_tx),
        };
        let mut pending = VecDeque::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let mut made_tool_call = false;
        let mut deadline = None;
        let id = shell.begin_run("background work");
        shell.set_awaiting_provider(id);
        let mut run = agent.prompt("hold the root").await.unwrap();
        let control = run.control();
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
            for _ in 0..presses {
                sender
                    .send(Ok(Event::Key(KeyEvent::new(
                        KeyCode::BackTab,
                        KeyModifiers::SHIFT,
                    ))))
                    .await
                    .unwrap();
            }
            handled.await.unwrap();
            release.send(true).unwrap();
            std::future::pending::<()>().await;
        };
        tokio::pin!(stimulus);
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! { result = driver => result.unwrap(), _ = &mut stimulus => unreachable!() }
        }).await.unwrap();
        drop(run);
        assert_eq!(outcome, HostRunOutcome::Completed);
        let choices = [
            ReasoningConfig::Off,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low),
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        ];
        let expected = &choices[presses % choices.len()];
        assert_eq!(
            pending,
            VecDeque::from([PendingIdleAction::ChangeThinkingLevel(
                [ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::High]
                    [presses % choices.len()]
            )])
        );
        assert_eq!(
            shell.selected_identity().1,
            format!("{} (queued)", reasoning_label(expected))
        );
        assert_eq!(shell.pending(), "draft remains");
        assert!(shell.debug_error().is_none());
        assert_eq!(agent.reasoning(), &ReasoningConfig::Off);
        assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!quit);
    }
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
            if accepted {
                "high"
            } else if !qualified {
                "high (queued)"
            } else {
                "off"
            }
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
