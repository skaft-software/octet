//! Windows-side bash tool regressions: shell-path resolution rules.
//!
//! Separate from `bash.rs` (and from the Unix cases next to this file)
//! because the Windows-only shell-selection rules are a distinct concern:
//! on this platform the implicit-fallback ban is the whole point.
use super::*;
use std::path::Path;

#[test]
fn legacy_wsl_bash_paths_are_not_implicit_candidates() {
    assert!(is_legacy_wsl_bash_path(Path::new(
        r"C:\Windows\System32\bash.exe"
    )));
    assert!(is_legacy_wsl_bash_path(Path::new(
        r"C:/Windows/Sysnative/bash.exe"
    )));
    assert!(!is_legacy_wsl_bash_path(Path::new(
        r"C:\Program Files\Git\bin\bash.exe"
    )));
}

#[test]
fn explicit_windows_shell_path_is_not_rewritten() {
    let path = Path::new(r"C:\Program Files\Git\bin\bash.exe");
    let resolved = resolve_windows_shell(Some(path)).expect("explicit shell path");
    assert_eq!(resolved.as_path(), path);
}
