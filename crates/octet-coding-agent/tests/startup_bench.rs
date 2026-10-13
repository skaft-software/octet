//! Real-process coverage for the opt-in `OCTET_BENCH` startup boundary.

use std::process::Command;

fn octet(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("OCTET_BENCH", "1")
        .current_dir(home);
    command
}

#[test]
fn bench_exits_at_interactive_dispatch_without_side_effects() {
    let home = tempfile::tempdir().expect("create home");
    let output = octet(home.path()).output().expect("run octet");
    assert!(output.status.success(), "bench run failed: {output:?}");
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "{output:?}"
    );
    let written = std::fs::read_dir(home.path()).expect("read home").count();
    assert_eq!(written, 0, "bench run wrote to HOME");
}

#[test]
fn bench_never_short_circuits_an_invocation_with_arguments() {
    let home = tempfile::tempdir().expect("create home");
    let output = octet(home.path())
        .arg("--help")
        .output()
        .expect("run octet");
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Usage"),
        "{output:?}"
    );
}
