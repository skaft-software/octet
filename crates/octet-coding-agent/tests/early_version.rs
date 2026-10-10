//! Real-process acceptance coverage for the zero-runtime version path.

#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
#[test]
fn version_prints_without_starting_runtime_threads() {
    let trace = tempfile::NamedTempFile::new().expect("create syscall trace");
    let output = Command::new("strace")
        .args(["-f", "-e", "trace=clone,clone3", "-o"])
        .arg(trace.path())
        .arg(env!("CARGO_BIN_EXE_octet"))
        .arg("--version")
        .output()
        .expect("run octet under strace");

    assert!(
        output.status.success(),
        "octet --version failed: {output:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("octet {}", env!("CARGO_PKG_VERSION"))
    );
    let trace = std::fs::read_to_string(trace.path()).expect("read syscall trace");
    assert!(
        !trace
            .lines()
            .any(|line| line.contains("clone(") || line.contains("clone3(")),
        "--version started threads before exiting:\n{trace}"
    );
}
