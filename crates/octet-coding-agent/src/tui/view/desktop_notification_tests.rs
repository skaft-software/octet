use super::*;
use octet_agent::extension_process::ExtensionRequestFailure;

#[test]
fn desktop_notification_refuses_a_disconnected_renderer_without_retaining_an_intent() {
    let mut shell = InteractiveShell::test_shell();
    shell.tui.take();
    let (tx, rx) = mpsc::sync_channel(1);
    *shell.render_tx.lock().unwrap() = Some(tx);
    drop(rx);
    assert_eq!(
        shell
            .queue_desktop_notification("title".into(), "body".into())
            .unwrap_err()
            .0,
        ExtensionRequestFailure::NotForegroundOwner
    );
    assert!(shell.state.borrow().desktop_notifications.is_empty());
}

#[test]
fn desktop_notification_refuses_ceded_or_absent_renderer_and_bounds_pending_intents() {
    let mut shell = InteractiveShell::test_shell();
    shell.cede_terminal_input();
    assert_eq!(
        shell
            .queue_desktop_notification("title".into(), "body".into())
            .unwrap_err()
            .0,
        ExtensionRequestFailure::NotForegroundOwner
    );
    shell.release_terminal_input();
    shell.tui.take();
    assert_eq!(
        shell
            .queue_desktop_notification("title".into(), "body".into())
            .unwrap_err()
            .0,
        ExtensionRequestFailure::NotForegroundOwner
    );
    let (tx, rx) = mpsc::sync_channel(1);
    *shell.render_tx.lock().unwrap() = Some(tx);
    for _ in 0..16 {
        shell
            .queue_desktop_notification("title".into(), "body".into())
            .unwrap();
    }
    assert_eq!(shell.state.borrow().desktop_notifications.len(), 16);
    assert_eq!(
        shell
            .queue_desktop_notification("title".into(), "body".into())
            .unwrap_err()
            .0,
        ExtensionRequestFailure::BoundsExceeded
    );
    drop(rx); // No renderer is running in this bounded-mailbox fixture.
}
