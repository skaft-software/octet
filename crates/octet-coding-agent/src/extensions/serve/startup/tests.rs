use super::*;

#[test]
fn serve_host_lock_is_exclusive_for_the_session_root_lifetime() {
    let directory = tempfile::tempdir().unwrap();
    let first = ServeHostLock::acquire_at(directory.path()).unwrap();
    assert!(ServeHostLock::acquire_at(directory.path()).is_err());
    drop(first);
    ServeHostLock::acquire_at(directory.path()).unwrap();
}

#[test]
fn startup_name_is_optional_trimmed_and_bounded_by_characters() {
    assert_eq!(normalize_startup_session_name(None).unwrap(), None);
    assert_eq!(
        normalize_startup_session_name(Some("  \t\n".into())).unwrap(),
        None
    );
    assert_eq!(
        normalize_startup_session_name(Some("  release review  ".into())).unwrap(),
        Some("release review".into())
    );
    let boundary = "界".repeat(MAX_STARTUP_SESSION_NAME_CHARS);
    assert_eq!(
        normalize_startup_session_name(Some(boundary.clone())).unwrap(),
        Some(boundary)
    );
    assert!(
        normalize_startup_session_name(Some("界".repeat(MAX_STARTUP_SESSION_NAME_CHARS + 1)))
            .is_err()
    );
    for name in ["release\nreview", "release\treview", "release\u{7f}review"] {
        assert!(normalize_startup_session_name(Some(name.into())).is_err());
    }
}
