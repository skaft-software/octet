//! Deterministic, non-TTY qualification for the terminal ownership boundary.
//!
//! PTY and physical-terminal cells remain in `docs/qualification/` and the
//! dedicated PTY suites. These tests intentionally do not claim Terminal.app,
//! Ghostty, or SSH observations.

use std::process::Command;

#[test]
fn non_tty_help_does_not_enter_interactive_terminal_mode() {
    let output = Command::new(env!("CARGO_BIN_EXE_octet"))
        .env("TERM", "dumb")
        .env_remove("COLORTERM")
        .env("NO_COLOR", "1")
        .arg("--help")
        .output()
        .expect("run octet help without a terminal");
    assert!(output.status.success());
    let mut bytes = output.stdout;
    bytes.extend_from_slice(&output.stderr);
    for sequence in [
        b"\x1b[?1049h".as_slice(),
        b"\x1b[?25l".as_slice(),
        b"\x1b[?1000h".as_slice(),
        b"\x1b[?2026h".as_slice(),
    ] {
        assert!(
            !bytes
                .windows(sequence.len())
                .any(|window| window == sequence),
            "help path emitted interactive control {sequence:?}"
        );
    }
}
