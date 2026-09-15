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
