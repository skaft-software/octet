//! Preferences take effect the moment they are issued, mid-run or idle. The
//! delivery modes also reach the running turn; none waits for the idle boundary.

use super::support::{run_active_command, test_run_inspection};
use super::*;
use crate::cli::QueueKind;
use octet_agent::QueueDeliveryMode;

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
