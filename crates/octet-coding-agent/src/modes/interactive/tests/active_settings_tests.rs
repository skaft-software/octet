//! Preferences take effect the moment they are issued, mid-run or idle. The
//! delivery modes also reach the running turn; none waits for the idle boundary.
//! Checked TUI write receipts retain their order until the idle reload owner.

use super::support::{run_active_command, test_run_inspection};
use super::*;
use crate::cli::QueueKind;
use octet_agent::QueueDeliveryMode;

#[cfg(unix)]
#[tokio::test]
async fn tui_preferences_acknowledge_only_successful_idle_and_active_switches() {
    const CHILD: &str = "OCTET_TEST_TUI_PREFERENCE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "modes::interactive::tests::active_settings_tests::tui_preferences_acknowledge_only_successful_idle_and_active_switches",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HOME", home.path())
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
        return;
    }
    use crate::config::TuiMode::{Fullscreen, Inline};
    use crate::reload::{
        ReloadLayer, ReloadSettings, ReloadSupervisor, ReloadWatchSet, ReloadWatcher,
        SystemMetadata,
    };
    use std::sync::atomic::AtomicBool;

    let path = crate::cli::global_config_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "reload = true\ntui_mode = 'inline'\n").unwrap();
    let mut watches = ReloadWatchSet::new();
    watches.watch_path(ReloadLayer::Resources, &path);
    let watcher = ReloadWatcher::new(watches);
    let scan = || watcher.scan(&SystemMetadata, 16);
    let now = std::time::Instant::now();
    let mut reload = ReloadSupervisor::new(ReloadSettings::default());
    reload.observe(now, &scan());
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let mut shell = InteractiveShell::test_shell();
    // This default-type consumer never reads a physical terminal.
    let mut input = EventStream::new().with_cede_flag(Arc::new(AtomicBool::new(true)));

    apply_settings_command(
        &mut app,
        &mut shell,
        &mut input,
        commands::SettingsCommand::TuiMode(Some(Fullscreen)),
        &mut reload,
    )
    .await
    .unwrap();
    assert!(shell
        .debug_snapshot()
        .contains("interactive terminal mode saved as fullscreen"));
    assert!(shell.debug_error().is_none());
    assert!(
        !reload
            .observe(now + Duration::from_secs(1), &scan())
            .anything_changed(),
        "a successful idle switch is already applied, not a new hydration"
    );

    let mut queue = VecDeque::new();
    for mode in [Inline, Fullscreen] {
        let command = Command::Settings(commands::SettingsCommand::TuiMode(Some(mode)));
        assert!(active_command_needs_idle_slot(&command));
        let (actions, quit) = run_active_command(&mut shell, command, test_run_inspection()).await;
        assert!(!quit);
        assert_eq!(actions.len(), 1);
        for action in actions {
            assert!(matches!(
                action,
                PendingIdleAction::AcknowledgeTuiPreference(_)
            ));
            push_pending_action(&mut queue, action);
        }
    }
    assert_eq!(
        queue.len(),
        2,
        "checked before/after chains must never coalesce"
    );
    assert!(model_controls::queue_labels(&queue).is_empty());
    assert!(model_controls::can_apply_reasoning_at_response(&queue));
    let (
        PendingIdleAction::AcknowledgeTuiPreference(first),
        PendingIdleAction::AcknowledgeTuiPreference(second),
    ) = (&queue[0], &queue[1])
    else {
        unreachable!()
    };
    assert_eq!(first.after, second.before);
    assert_eq!(first.path, path);
    let mut deadline = None;
    let mut pending_reexec = None;
    app = Box::pin(apply_pending_actions(
        app,
        &mut shell,
        &mut input,
        &mut queue,
        &mut deadline,
        None,
        &mut pending_reexec,
        &mut reload,
    ))
    .await
    .unwrap();
    assert!(queue.is_empty());
    assert!(
        !reload
            .observe(now + Duration::from_secs(2), &scan())
            .anything_changed(),
        "the idle owner must acknowledge both active writes in order"
    );

    let receipt = save_tui_mode_with(&mut shell, &mut app.executable_extensions, Inline, |_| {
        Err(anyhow::anyhow!("renderer restart failed"))
    })
    .await;
    assert!(
        receipt.is_none(),
        "failed renderer switches cannot be acknowledged"
    );
    assert!(shell
        .debug_error()
        .unwrap()
        .contains("renderer restart failed"));
    assert!(
        reload
            .observe(now + Duration::from_secs(3), &scan())
            .anything_changed(),
        "the saved but unapplied preference must remain visible to reload"
    );

    // Queued external evidence survives even a later successful live switch.
    apply_settings_command(
        &mut app,
        &mut shell,
        &mut input,
        commands::SettingsCommand::TuiMode(Some(Fullscreen)),
        &mut reload,
    )
    .await
    .unwrap();
    assert!(reload.is_queued());
    let mut reload = ReloadSupervisor::new(ReloadSettings::default());
    reload.observe(now, &scan());
    apply_settings_command(
        &mut app,
        &mut shell,
        &mut input,
        commands::SettingsCommand::TuiMode(Some(Inline)),
        &mut reload,
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{content}\n# later external edit\n")).unwrap();
    assert!(
        reload
            .observe(now + Duration::from_secs(4), &scan())
            .anything_changed(),
        "no suppression window may hide the next external edit"
    );

    // Persistence failure must not switch the screen or produce a receipt.
    std::fs::write(&path, "broken = [").unwrap();
    assert!(save_tui_mode_with(
        &mut shell,
        &mut app.executable_extensions,
        Fullscreen,
        |_| panic!("a failed persistence must not switch the renderer"),
    )
    .await
    .is_none());
    assert!(shell.debug_error().unwrap().contains("failed to save"));
}

#[tokio::test]
async fn a_delivery_mode_chosen_mid_run_reaches_the_running_turn() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    // The run is never polled: this is a local control test.
    let run = app.agent.prompt("work").await.unwrap();
    let control = run.control();
    let mut shell = InteractiveShell::test_shell();
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    save_queue_mode_with(
        &mut shell,
        &mut extensions,
        Some(&control),
        QueueKind::Steering,
        QueueDeliveryMode::All,
        || Ok(()),
    )
    .await;
    let shown = shell.debug_snapshot();
    assert!(shown.contains("mode saved as all"), "{shown}");
    assert!(
        shown.contains("the running turn uses it from its next delivery"),
        "{shown}"
    );
    assert_eq!(shell.debug_error(), None);
    drop(run);
}

#[tokio::test]
async fn a_delivery_mode_chosen_while_idle_applies_from_the_next_run() {
    let mut shell = InteractiveShell::test_shell();
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    save_queue_mode_with(
        &mut shell,
        &mut extensions,
        None,
        QueueKind::FollowUp,
        QueueDeliveryMode::OneAtATime,
        || Ok(()),
    )
    .await;
    let shown = shell.debug_snapshot();
    assert!(shown.contains("it applies from the next run"), "{shown}");
    assert!(!shown.contains("running turn"), "{shown}");
    assert_eq!(shell.debug_error(), None);
}

#[tokio::test]
async fn a_failed_write_reports_and_never_claims_the_mode_changed() {
    let mut shell = InteractiveShell::test_shell();
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    save_queue_mode_with(
        &mut shell,
        &mut extensions,
        None,
        QueueKind::Steering,
        QueueDeliveryMode::All,
        || Err(anyhow::anyhow!("disk full")),
    )
    .await;
    let error = shell.debug_error().expect("the failure is reported");
    assert!(error.contains("disk full"), "{error}");
    assert!(shell.debug_snapshot().is_empty());
}

#[tokio::test]
async fn invalid_settings_issued_mid_run_fail_at_once_instead_of_queueing() {
    for command in [
        commands::SettingsCommand::DefaultModel(Some("no/such-model".into())),
        commands::SettingsCommand::DefaultReasoning(Some("not-a-level".into())),
    ] {
        let mut shell = InteractiveShell::test_shell();
        let (queue, _) = run_active_command(
            &mut shell,
            Command::Settings(command.clone()),
            test_run_inspection(),
        )
        .await;
        assert!(queue.is_empty(), "{command:?} must not wait for idle");
        assert!(shell.debug_error().is_some(), "{command:?}");
        assert!(!shell.debug_snapshot().contains("queued"), "{command:?}");
    }
}

#[tokio::test]
async fn default_reports_issued_mid_run_answer_at_once() {
    for (command, expected) in [
        (
            commands::SettingsCommand::DefaultModel(None),
            "default model for new sessions",
        ),
        (
            commands::SettingsCommand::DefaultReasoning(None),
            "default reasoning",
        ),
    ] {
        let mut shell = InteractiveShell::test_shell();
        let (queue, _) = run_active_command(
            &mut shell,
            Command::Settings(command.clone()),
            test_run_inspection(),
        )
        .await;
        assert!(queue.is_empty(), "{command:?} must not wait for idle");
        let shown = shell.debug_snapshot();
        assert!(shown.contains(expected), "{command:?}: {shown}");
        assert!(!shown.contains("queued"), "{command:?}: {shown}");
    }
}
