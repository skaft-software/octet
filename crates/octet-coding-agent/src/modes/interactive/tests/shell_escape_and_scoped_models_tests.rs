//! Shell-escape context policy, scoped model inventory mutations, the read-only
//! report surfaces, and the model-picker admission rules.
//! Separate because these own what may enter the next provider request.

use super::*;

use super::support::*;

#[test]
fn app_shell_policy_is_explicit_and_checkpoints_survive_rebuilds() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    #[cfg(any(unix, windows))]
    assert!(app.agent.partial_output_checkpoint_stats().is_some());
    assert!(!app.config.tool_available("powershell"));
    app.config.tools = crate::config::ToolPolicy::only(["powershell".into()]).unwrap();
    assert_eq!(app.config.tool_available("powershell"), cfg!(windows));
    app.config.sandbox.allow_process = false;
    assert!(!app.config.tool_available("powershell"));
    app.config.sandbox.allow_process = true;
    app.config.sandbox.allow_shell = false;
    assert!(!app.config.tool_available("powershell"));
    // Restore fixture policy before checking the rebuild, not an unavailable
    // Windows-only explicit request on this platform.
    app.config.tools = Default::default();
    app.config.sandbox.allow_shell = true;
    app = rebuild_app(app, None, None, None, None).unwrap();
    #[cfg(any(unix, windows))]
    assert!(app.agent.partial_output_checkpoint_stats().is_some());
}

/// 2d.11 — `!command` results enter model context; `!!command` results are
/// durably recorded but explicitly excluded from it.
#[test]
fn shell_escape_records_are_explicitly_included_or_excluded_from_context() {
    let (_directory, mut app) = fast_test_app(scripted_codex_model("http://127.0.0.1:1"));
    let included = commands::ShellEscapeRecord::new("printf hi", "hi", 0, false);
    record_shell_escape(&mut app, &included).unwrap();
    let context = serde_json::to_string(&app.agent.session().context().unwrap()).unwrap();
    assert!(context.contains("printf hi"), "{context}");
    assert!(context.contains("exit 0"), "{context}");

    let excluded = commands::ShellEscapeRecord::new("printf secret", "secret-value", 1, true);
    record_shell_escape(&mut app, &excluded).unwrap();
    let context = serde_json::to_string(&app.agent.session().context().unwrap()).unwrap();
    assert!(!context.contains("secret-value"), "{context}");
    assert!(!context.contains("printf secret"), "{context}");
    // The excluded execution is still durably accounted for, with an
    // explicit exclusion marker and its exact command and output.
    let head = app.agent.session().head().unwrap();
    let entry = app.agent.session().entry(&head).unwrap();
    let display = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.display_text.as_deref())
        .expect("the excluded record retains its presentation text");
    assert!(
        display.contains("[excluded from model context]"),
        "{display}"
    );
    assert!(display.contains("printf secret"), "{display}");
    assert!(display.contains("secret-value"), "{display}");
}

/// A pending tool call must never be split from its result by a shell
/// record, even for the context-including `!` form.
#[test]
fn an_unresolved_tool_call_keeps_an_included_record_out_of_context() {
    let (_directory, mut app) = fast_test_app(scripted_codex_model("http://127.0.0.1:1"));
    let model = app.model.spec.id.clone();
    app.agent
        .session_mut()
        .append(octet_agent::EntryValue::Message(
            octet_ai::Message::Assistant(octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                    async_execution: false,
                    id: ToolCallId("call-1".into()),
                    name: "bash".into(),
                    arguments_json: "{\"command\":\"ls\"}".into(),
                    argument_error: None,
                })],
                model,
                protocol: octet_ai::Protocol::AnthropicMessages,
            }),
        ))
        .unwrap();
    assert!(session_has_unresolved_tool_calls(app.agent.session()));
    record_shell_escape(
        &mut app,
        &commands::ShellEscapeRecord::new("printf hi", "hi", 0, false),
    )
    .unwrap();
    let context = serde_json::to_string(&app.agent.session().context().unwrap()).unwrap();
    assert!(!context.contains("printf hi"), "{context}");
}

/// 2d.2 — the scope mutations materialize deterministically, toggle the
/// requested provider, reorder by request, and hand back the exact ordered
/// pattern list (with reasoning suffixes) the writer persists.
#[test]
fn scoped_models_mutations_materialize_toggle_reorder_and_persist() {
    let mut model = scripted_codex_model("http://127.0.0.1:1");
    std::sync::Arc::make_mut(&mut model.spec).id = ModelId("first".into());
    std::sync::Arc::make_mut(&mut model.spec).api_name = "first".into();
    let (_directory, mut app) = fast_test_app(model);
    let mut second = (*app.model.spec).clone();
    second.id = ModelId("second".into());
    second.api_name = "second".into();
    app.catalog.register_model(second).unwrap();

    // `all` builds the explicit ordered scope from the complete catalog in
    // a stable order and persists a re-selectable glob.
    let persistence = apply_scope_mutation(&mut app, &commands::ScopedModelsCommand::All).unwrap();
    assert_eq!(persistence, Some(Some("*".to_owned())));
    let all = app.model_cycle();
    assert!(all.contains(&"first".to_owned()), "{all:?}");
    assert!(all.contains(&"second".to_owned()), "{all:?}");
    assert!(
        all.windows(2).all(|pair| pair[0] < pair[1]),
        "`all` must materialize a stable order: {all:?}"
    );
    // `clear` removes the restriction and the persisted key; unrestricted
    // cycling follows the same stable catalog order.
    assert_eq!(
        apply_scope_mutation(&mut app, &commands::ScopedModelsCommand::Clear).unwrap(),
        Some(None)
    );
    assert_eq!(app.model_cycle(), all);

    // A narrow ordered scope exercises provider toggle and reorder exactly.
    app.set_model_scope_patterns(Some("first:low,second"))
        .unwrap();
    assert_eq!(app.model_cycle(), vec!["first", "second"]);
    // A provider toggle disables every model of that provider, then
    // restores them in stable order.
    assert_eq!(
        apply_scope_mutation(
            &mut app,
            &commands::ScopedModelsCommand::Toggle("test/*".into())
        )
        .unwrap(),
        Some(None)
    );
    assert!(app.model_cycle().is_empty());
    assert_eq!(
        apply_scope_mutation(
            &mut app,
            &commands::ScopedModelsCommand::Toggle("test/*".into())
        )
        .unwrap(),
        Some(Some("first,second".to_owned()))
    );
    // Reorder follows the requested move; the untouched entry keeps the
    // launch suffix in the persisted list.
    app.set_model_scope_patterns(Some("first:low,second"))
        .unwrap();
    assert_eq!(
        apply_scope_mutation(
            &mut app,
            &commands::ScopedModelsCommand::Move {
                model: "second".into(),
                direction: commands::ScopeMove::Top,
            }
        )
        .unwrap(),
        Some(Some("second,first:low".to_owned()))
    );
    assert_eq!(app.model_cycle(), vec!["second", "first"]);
    // Unmatched targets and boundary moves are explicit errors.
    assert!(apply_scope_mutation(
        &mut app,
        &commands::ScopedModelsCommand::Enable("nope/*".into())
    )
    .is_err());
    assert!(apply_scope_mutation(
        &mut app,
        &commands::ScopedModelsCommand::Move {
            model: "second".into(),
            direction: commands::ScopeMove::Up,
        }
    )
    .is_err());
}

/// 2d.1 / 2d.2 — the active-run path renders both reports immediately and
/// queues the mutations it cannot own mid-run.
#[tokio::test]
async fn settings_and_scoped_models_render_mid_run_and_queue_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let inspection = test_run_inspection_with_session(directory.path());
    let mut shell = InteractiveShell::test_shell();
    shell.set_runtime_config(terminal_theme_test_config(directory.path().to_owned()));
    shell.begin_run("test");

    let (queue, quit) = run_active_command(
        &mut shell,
        Command::Settings(commands::SettingsCommand::Show),
        &inspection,
    )
    .await;
    assert!(queue.is_empty());
    assert!(!quit);
    assert!(shell.has_overlay());
    shell.close_overlay();

    let (queue, quit) = run_active_command(
        &mut shell,
        Command::Settings(commands::SettingsCommand::Images(Some(true))),
        &inspection,
    )
    .await;
    assert!(!quit);
    assert!(matches!(
        queue.back(),
        Some(PendingIdleAction::SyncImages(true))
    ));
    assert!(shell.runtime_config().unwrap().show_images);

    let (queue, quit) = run_active_command(
        &mut shell,
        Command::ScopedModels(commands::ScopedModelsCommand::Show),
        &inspection,
    )
    .await;
    assert!(queue.is_empty());
    assert!(!quit);
    assert!(shell.has_overlay());
    shell.close_overlay();

    let (queue, quit) = run_active_command(
        &mut shell,
        Command::ScopedModels(commands::ScopedModelsCommand::All),
        &inspection,
    )
    .await;
    assert!(!quit);
    assert!(matches!(
        queue.back(),
        Some(PendingIdleAction::ScopedModels(
            commands::ScopedModelsCommand::All
        ))
    ));
}

/// 2d.11 active path: an approved escape runs immediately under the captured
/// sandbox, and its explicit context decision is queued for the idle owner
/// that owns the session writer.
#[cfg(unix)]
#[tokio::test]
async fn active_shell_escape_runs_and_queues_its_context_decision() {
    let directory = tempfile::tempdir().unwrap();
    let inspection = test_run_inspection_with_session(directory.path());
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("test");
    let (queue, quit) = run_active_command(
        &mut shell,
        Command::Bash(commands::BashEscape {
            command: "printf octet-active-shell".into(),
            excluded: false,
        }),
        &inspection,
    )
    .await;
    assert!(!quit);
    let Some(PendingIdleAction::RecordShellEscape(record)) = queue.back() else {
        panic!("the active escape must queue its durable record: {queue:?}");
    };
    assert!(!record.excluded());
    assert_eq!(record.exit_code(), 0);
    assert!(record.output().contains("octet-active-shell"), "{record:?}");

    // `!!` fixes the opposite decision without touching execution.
    let (queue, quit) = run_active_command(
        &mut shell,
        Command::Bash(commands::BashEscape {
            command: "printf octet-excluded".into(),
            excluded: true,
        }),
        &inspection,
    )
    .await;
    assert!(!quit);
    let Some(PendingIdleAction::RecordShellEscape(record)) = queue.back() else {
        panic!("the excluded escape must still be accounted for: {queue:?}");
    };
    assert!(record.excluded());
}

/// The shared local-shell execution path keeps bounded capture, an exit
/// status, and no refusal for an ordinary command.
#[cfg(unix)]
#[tokio::test]
async fn local_shell_escape_runs_through_the_bounded_capture_path() {
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    let outcome = run_local_shell(
        &mut shell,
        &mut input,
        Path::new("/"),
        &SandboxPolicy::default(),
        "printf octet-shell-test",
    )
    .await
    .unwrap();
    assert_eq!(outcome.exit_code, 0);
    assert!(
        outcome.output.contains("octet-shell-test"),
        "{}",
        outcome.output
    );
    assert!(outcome.refusal.is_none());
    assert!(!outcome.stopped);
    assert!(!outcome.shutting_down);
}

#[test]
fn scoped_model_order_suffixes_and_rebuilds_preserve_launch_defaults() {
    let mut model = scripted_codex_model("http://127.0.0.1:1");
    Arc::make_mut(&mut model.spec).capabilities.reasoning = Some(octet_ai::ReasoningCapability {
        options: None,
        control: octet_ai::ReasoningControl::Effort,
        exposes_text: true,
        preserves_state: false,
        effort_budgets: None,
        openai_chat_mode: Default::default(),
        min_effort: octet_ai::ReasoningEffort::Minimal,
        max_effort: octet_ai::ReasoningEffort::High,
    });
    let (_directory, mut app) = fast_test_app(model);
    let mut second = (*app.model.spec).clone();
    second.id = ModelId("second".into());
    app.catalog.register_model(second).unwrap();
    app.set_model_scope_patterns(Some("second:low,scripted:high"))
        .unwrap();
    assert_eq!(app.model_cycle(), vec!["second", "scripted"]);
    assert_eq!(app.model.spec.id.0, "scripted");
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Off,
        "scope handoff must not override launch defaults"
    );
    app = crate::app::apply_reconfig(app, Reconfig::Model(ModelId("second".into()))).unwrap();
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low)
    );
    app = crate::app::apply_reconfig(app, Reconfig::Model(ModelId("scripted".into()))).unwrap();
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    app = rebuild_app(app, None, None, None, None).unwrap();
    assert_eq!(app.model_cycle(), vec!["second", "scripted"]);
    // Removing an available route narrows, rather than widens, the scope.
    app.catalog
        .remove_model_if_endpoint(&ModelId("second".into()), &app.model.endpoint.id);
    assert_eq!(app.model_cycle(), vec!["scripted"]);
    app.set_model_scope_patterns(None).unwrap();
    let cycle = app.model_cycle();
    assert!(cycle.windows(2).all(|pair| pair[0] < pair[1]));
}

#[tokio::test]
async fn session_and_hotkeys_reports_remain_read_only_during_runs() {
    let directory = tempfile::tempdir().unwrap();
    let inspection = test_run_inspection_with_session(directory.path());
    let before = std::fs::read(&inspection.session_path).unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("test");
    for (command, expected) in [
        ("/session info", "Active-branch messages: 1"),
        ("/hotkeys", "| `Ctrl+P` | Cycle to next model |"),
    ] {
        let (queue, quit) =
            run_active_command(&mut shell, commands::parse(command), &inspection).await;
        assert!(queue.is_empty());
        assert!(!quit);
        assert!(shell.has_overlay());
        let text = if command == "/hotkeys" {
            shell.hotkeys_markdown()
        } else {
            commands::session_text(&inspection.read_only_session().unwrap())
        };
        assert!(text.contains(expected), "{text}");
        shell.close_overlay();
    }
    let (queue, quit) = run_active_command(&mut shell, Command::Copy, &inspection).await;
    assert!(queue.is_empty());
    assert!(!quit);
    assert!(shell
        .debug_error()
        .unwrap()
        .contains("no assistant message"));
    assert_eq!(std::fs::read(&inspection.session_path).unwrap(), before);
}

/// A narrowed launch opens `/model` without waiting for fleet discovery.
/// Cancelling before completion cannot apply a late catalog to the app.
#[tokio::test]
async fn model_picker_cancel_keeps_the_narrowed_launch_catalog() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let active = app.model.spec.id.clone();
    let narrowed = app.catalog.models().count();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
    assert!(ActiveRunInspection::capture(&app).is_narrowed());
    let mut shell = InteractiveShell::test_shell();
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    sender
        .send(Ok(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
    assert!(open_model_picker(&mut app, &mut shell, &mut input)
        .await
        .unwrap()
        .is_none());
    assert!(!shell.has_panel());
    assert!(!app.readiness.is_fleet());
    assert_eq!(app.catalog.models().count(), narrowed);
    assert_eq!(app.model.spec.id, active);
}

#[test]
fn picker_catalog_completion_keeps_the_active_route() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let active = app.model.spec.id.clone();
    let narrowed = app.catalog.models().count();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
    let (catalog, notes) = crate::app::bootstrap::model_catalog_for_readiness(
        app.config.offline,
        &crate::app::bootstrap::CatalogReadiness::Fleet,
    )
    .unwrap();
    assert!(app.apply_picker_catalog(&active, catalog, notes).unwrap());
    assert!(app.readiness.is_fleet());
    assert!(app.catalog.models().count() >= narrowed);
    assert!(app.catalog.resolve(&active).is_ok());
    assert!(!ActiveRunInspection::capture(&app).is_narrowed());
}

#[test]
fn picker_catalog_rejects_an_obsolete_selection() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
    let before = app.catalog.models().count();
    let (catalog, notes) = crate::app::bootstrap::model_catalog_for_readiness(
        app.config.offline,
        &crate::app::bootstrap::CatalogReadiness::Fleet,
    )
    .unwrap();
    assert!(!app
        .apply_picker_catalog(&ModelId("obsolete".into()), catalog, notes)
        .unwrap());
    assert!(!app.readiness.is_fleet());
    assert_eq!(app.catalog.models().count(), before);
}

#[test]
fn picker_catalog_rejects_a_withdrawn_active_route() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let active = app.model.spec.id.clone();
    let endpoint = app.model.endpoint.id.clone();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
    let (mut catalog, notes) = crate::app::bootstrap::model_catalog_for_readiness(
        app.config.offline,
        &crate::app::bootstrap::CatalogReadiness::Fleet,
    )
    .unwrap();
    assert!(catalog.remove_model_if_endpoint(&active, &endpoint));
    assert!(app.apply_picker_catalog(&active, catalog, notes).is_err());
    assert!(!app.readiness.is_fleet());
    assert!(app.catalog.resolve(&active).is_ok());
}

#[test]
fn model_scope_does_not_override_an_existing_explicit_session() {
    for existing in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut config = terminal_theme_test_config(directory.path().to_owned());
        config.session_dir = directory.path().join("sessions");
        let store = crate::session_store::SessionStore::new(&config.session_dir, &config.workspace);
        store.write_workspace_marker().unwrap();
        if existing {
            let mut session = Session::create(store.dir().join("chosen.jsonl")).unwrap();
            session
                .append(EntryValue::Config {
                    model: Some("saved-model".into()),
                    reasoning: None,
                    reasoning_mode: None,
                })
                .unwrap();
        }
        let options = crate::cli::parity::ParityOptions {
            session_id: Some("chosen".into()),
            models: Some("gpt-5.4-mini-responses:high".into()),
            ..Default::default()
        };
        options
            .resolve_models_in_catalog(&mut config, &octet_ai::ModelCatalog::builtin().unwrap())
            .unwrap();
        assert_eq!(config.model.is_none(), existing);
        assert_eq!(config.model_explicit, !existing);
        options.select_session(&mut config).unwrap();
        assert!(
            matches!(config.resume, crate::config::ResumeSelector::Resume(Some(ref id)) if id == "chosen")
        );
    }
}

#[tokio::test]
async fn direct_model_selection_enriches_and_unknown_models_preserve_the_app() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
    let target = ModelId("gpt-5.4-mini-responses".into());
    let target_model = app.catalog.resolve(&target).unwrap();
    app.catalog
        .remove_model_if_endpoint(&target, &target_model.endpoint.id);
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending();
    let mut app = transition(app, &mut shell, &mut input, Reconfig::Model(target.clone()))
        .await
        .unwrap();
    assert!(app.readiness.is_fleet());
    assert_eq!(app.model.spec.id, target);
    let path = app.agent.session().path().to_owned();
    app = transition(
        app,
        &mut shell,
        &mut input,
        Reconfig::Model(ModelId("nonexistent/route".into())),
    )
    .await
    .unwrap();
    assert_eq!(app.model.spec.id, target);
    assert_eq!(app.agent.session().path(), path);
    assert!(shell.debug_error().unwrap().contains("Unknown model"));
}
