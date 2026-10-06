use super::*;

#[test]
fn conventional_exit_status_is_signal_compatible() {
    assert_eq!(conventional_signal_exit_code(2), 130);
    assert_eq!(conventional_signal_exit_code(15), 143);
}

#[test]
fn shutdown_rejects_non_positive_signal_numbers_before_global_state_changes() {
    assert!(request_coordinated_shutdown(0).is_err());
    assert!(request_coordinated_shutdown(-1).is_err());
    assert_eq!(received_shutdown_signal(), None);
}

#[test]
fn windows_console_control_events_map_to_conventional_signals() {
    assert_eq!(windows_control_signal(0), Some(2), "Ctrl-C");
    assert_eq!(windows_control_signal(1), Some(21), "Ctrl-Break");
    assert_eq!(windows_control_signal(2), Some(1), "console closed");
    assert_eq!(windows_control_signal(5), Some(15), "logoff");
    assert_eq!(windows_control_signal(6), Some(15), "shutdown");
    // Reserved and future events keep the default handler.
    assert_eq!(windows_control_signal(3), None);
    assert_eq!(windows_control_signal(4), None);
    assert_eq!(windows_control_signal(7), None);
}
