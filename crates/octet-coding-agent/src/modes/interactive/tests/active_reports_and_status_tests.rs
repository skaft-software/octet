//! Read-only reports and status transitions while a run is active: changelog, session
//! inspection, foreground title, and the model/thinking status notices.
//! Separate because these all assert that an active run is never suspended or
//! interrupted by a host-owned surface.

use super::*;

use super::support::*;

#[tokio::test]
async fn active_changelog_is_read_only_and_does_not_queue_or_interrupt() {
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("test");
    let before = shell.debug_snapshot();
    let (queue, quit_requested) =
        run_active_command(&mut shell, Command::Changelog, test_run_inspection()).await;
    assert!(shell.has_overlay());
    assert!(queue.is_empty());
    assert!(!quit_requested);
    assert_eq!(
        shell.debug_snapshot(),
        before,
        "release notes are not conversation"
    );
    assert_eq!(
        shell.overlay_input(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ))),
        OverlayInputResult::Closed
    );
    assert_eq!(
        shell.debug_snapshot(),
        before,
        "escape closes the report, not the run"
    );
}

#[tokio::test]
async fn active_inspection_reports_render_without_waiting_for_the_idle_boundary() {
    let session_dir = tempfile::tempdir().expect("inspection fixture");
    let inspection = test_run_inspection_with_session(session_dir.path());
    for command in [
        Command::Help(None),
        Command::Context,
        Command::Cost,
        Command::Cache,
    ] {
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("test");
        let (queue, quit_requested) =
            run_active_command(&mut shell, command.clone(), &inspection).await;

        assert!(shell.has_overlay(), "{command:?} did not render a report");
        assert!(queue.is_empty(), "{command:?} must not wait for idle");
        assert!(!quit_requested);
        assert_eq!(shell.debug_error(), None, "{command:?} reported an error");
    }
}

#[tokio::test]
async fn active_session_commands_report_through_the_read_only_session() {
    let session_dir = tempfile::tempdir().expect("inspection fixture");
    let inspection = test_run_inspection_with_session(session_dir.path());
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("test");

    // `/name` without an argument reports the name stored beside the live
    // session file; `/export` writes a portable copy of the same records.
    let (queue, quit_requested) =
        run_active_command(&mut shell, Command::Name(None), &inspection).await;
    assert!(queue.is_empty());
    assert!(!quit_requested);
    assert!(
        shell.debug_snapshot().contains("session name:"),
        "transcript: {}",
        shell.debug_snapshot()
    );

    let output = session_dir.path().join("exported.json");
    let (queue, quit_requested) = run_active_command(
        &mut shell,
        Command::Export(Some(output.display().to_string())),
        &inspection,
    )
    .await;
    assert!(queue.is_empty());
    assert!(!quit_requested);
    assert!(shell.has_overlay(), "export did not render a report");
    assert!(output.exists(), "export did not write {}", output.display());
    assert_eq!(shell.debug_error(), None);
}

#[tokio::test]
async fn active_name_updates_the_foreground_title_without_waiting_for_run_settlement() {
    let session_dir = tempfile::tempdir().unwrap();
    let inspection = test_run_inspection_with_session(session_dir.path());
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("test");
    let (queue, quit_requested) = run_active_command(
        &mut shell,
        Command::Name(Some("Release audit".into())),
        &inspection,
    )
    .await;
    assert!(queue.is_empty());
    assert!(!quit_requested);
    assert_eq!(shell.debug_session_name().as_deref(), Some("Release audit"));

    // Read-only `/name` does not rename, and a refused rename cannot
    // replace the title already shown for the current session.
    run_active_command(&mut shell, Command::Name(None), &inspection).await;
    assert_eq!(shell.debug_session_name().as_deref(), Some("Release audit"));
    run_active_command(&mut shell, Command::Name(Some("\x07".into())), &inspection).await;
    assert_eq!(shell.debug_session_name().as_deref(), Some("Release audit"));
    assert_eq!(
        inspection
            .sessions
            .load_metadata("session")
            .unwrap()
            .name
            .as_deref(),
        Some("Release audit")
    );
}

#[tokio::test]
async fn foreground_title_tracks_idle_name_new_resume_fork_and_clear() {
    let (_workspace, app) = crate::compaction::tests::app_for_estimate();
    let mut shell = InteractiveShell::test_shell();
    let mut input = EventStream::from_stream(futures_util::stream::pending());
    let mut app = transition(app, &mut shell, &mut input, Reconfig::NewSession)
        .await
        .unwrap();
    assert_eq!(shell.debug_session_name(), None);
    let named_path = app.agent.session().path().to_owned();
    let named_id = named_path.file_stem().unwrap().to_str().unwrap().to_owned();
    let mut goal_deadline = None;
    let mut idle_input = EventStream::new();
    let mut reload =
        crate::reload::ReloadSupervisor::new(crate::reload::ReloadSettings::disabled());
    let outcome = run_idle_command(
        app,
        &mut shell,
        &mut idle_input,
        Command::Name(Some("Release audit".into())),
        &mut goal_deadline,
        None,
        &mut reload,
    )
    .await
    .unwrap();
    let IdleCommandOutcome::Continue(next) = outcome else {
        panic!("name command must keep the current app");
    };
    app = *next;
    assert_eq!(shell.debug_session_name().as_deref(), Some("Release audit"));

    app = transition(app, &mut shell, &mut input, Reconfig::NewSession)
        .await
        .unwrap();
    assert_eq!(
        shell.debug_session_name(),
        None,
        "a new session must clear the old name"
    );
    app = transition(
        app,
        &mut shell,
        &mut input,
        Reconfig::Resume(named_path.clone()),
    )
    .await
    .unwrap();
    assert_eq!(shell.debug_session_name().as_deref(), Some("Release audit"));

    let head = app.agent.session().head();
    let fork_path = app.sessions.new_path("fork-title");
    fork_active_session(&app.sessions, &named_path, fork_path.clone(), head.as_ref()).unwrap();
    app = transition(app, &mut shell, &mut input, Reconfig::Resume(fork_path))
        .await
        .unwrap();
    assert_eq!(
        shell.debug_session_name(),
        None,
        "forks do not inherit the name"
    );

    app.sessions.rename(&named_id, "").unwrap();
    let _ = transition(app, &mut shell, &mut input, Reconfig::Resume(named_path))
        .await
        .unwrap();
    assert_eq!(
        shell.debug_session_name(),
        None,
        "a cleared name restores the default"
    );
}

#[tokio::test]
async fn model_and_thinking_transitions_update_status_without_success_notices() {
    let (_workspace, mut app) = crate::compaction::tests::app_for_estimate();
    let mut shell = InteractiveShell::test_shell();
    update_status(&mut shell, &app);
    let mut input = futures_util::stream::pending();
    let model = ModelId("claude-sonnet-4-5".into());
    assert_ne!(shell.selected_identity().0, model.0);
    app = transition(app, &mut shell, &mut input, Reconfig::Model(model.clone()))
        .await
        .unwrap();
    assert_eq!(shell.selected_identity().0, model.0);
    assert!(shell.status_detail().contains(&model.0));
    assert!(shell.debug_snapshot().is_empty());

    for level in [ThinkingLevel::High, ThinkingLevel::Low, ThinkingLevel::High] {
        let reasoning =
            requested_thinking_to_reasoning(level, &app.model, app.subagents_available()).unwrap();
        let label = reasoning_label(&reasoning);
        let previous = shell.selected_identity();
        app = transition(app, &mut shell, &mut input, Reconfig::Thinking(reasoning))
            .await
            .unwrap();
        assert_ne!(shell.selected_identity(), previous);
        assert_eq!(shell.selected_identity(), (model.0.clone(), label.clone()));
        assert!(shell.status_detail().contains(&label));
        assert!(
            shell.debug_snapshot().is_empty(),
            "success notices must not accumulate"
        );
        assert_eq!(shell.debug_error(), None);
        assert!(
            app.agent.session().entries().iter().any(|entry| matches!(
                &entry.value,
                EntryValue::Config { reasoning: Some(value), .. } if value == &label
            )),
            "configuration provenance must remain durable"
        );
    }
}

#[tokio::test]
async fn failed_model_transition_returns_diagnostic_without_changing_status() {
    let (_workspace, app) = crate::compaction::tests::app_for_estimate();
    let mut shell = InteractiveShell::test_shell();
    update_status(&mut shell, &app);
    let identity = shell.selected_identity();
    let mut input = futures_util::stream::pending();
    let app = transition(
        app,
        &mut shell,
        &mut input,
        Reconfig::Model(ModelId("missing-release-test-model".into())),
    )
    .await
    .expect("an unresolved model must preserve the usable app");
    let error = shell
        .debug_error()
        .expect("a diagnostic must remain visible");
    assert!(
        error.to_string().contains("missing-release-test-model"),
        "{error}"
    );
    assert_eq!(shell.selected_identity(), identity);
    assert_eq!(
        app.config.model.as_ref().map(|id| id.0.as_str()),
        Some(identity.0.as_str())
    );
    assert!(shell.debug_snapshot().is_empty());
}

#[tokio::test]
async fn queued_setting_changes_retain_acknowledgements_and_invalid_values_retain_errors() {
    for command in [
        Command::Model(Some("gpt-4o-mini".into())),
        Command::Thinking(Some("high".into())),
    ] {
        let mut shell = InteractiveShell::test_shell();
        let (queue, _) = run_active_command(&mut shell, command, test_run_inspection()).await;
        assert_eq!(queue.len(), 1);
        assert!(shell
            .debug_snapshot()
            .contains("command queued for the next idle boundary"));
        assert_eq!(shell.debug_error(), None);
    }
    let mut shell = InteractiveShell::test_shell();
    let (queue, _) = run_active_command(
        &mut shell,
        Command::Thinking(Some("invalid-effort".into())),
        test_run_inspection(),
    )
    .await;
    assert!(queue.is_empty());
    assert!(shell.debug_error().is_some());
    assert!(shell.debug_snapshot().is_empty());
}

#[test]
fn starting_a_new_prompt_clears_the_previous_error() {
    let mut shell = InteractiveShell::test_shell();
    shell.error("old failure".to_string());
    assert_eq!(shell.debug_error().as_deref(), Some("old failure"));

    prepare_prompt(&mut shell);

    assert_eq!(shell.debug_error(), None);
}
