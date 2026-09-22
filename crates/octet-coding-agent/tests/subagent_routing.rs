//! Exercise the real Python bundle and host wire, not two independently mocked
//! interpretations of policy.resolved_model. Inference is loopback-only and the
//! child process receives an isolated HOME with no ambient provider credentials.
#![cfg(unix)]

#[test]
fn configured_worker_route_crosses_the_real_extension_boundary() {
    let output = std::process::Command::new("python3")
        .arg("-I")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/subagent_routing_smoke.py"
        ))
        .arg(env!("CARGO_BIN_EXE_octet"))
        .output()
        .expect("Python 3 is required by the subagents bundle");
    assert!(
        output.status.success(),
        "local routing smoke failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
