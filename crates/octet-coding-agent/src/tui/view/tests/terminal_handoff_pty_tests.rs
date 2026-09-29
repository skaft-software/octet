//! The unix-only pty handoff fixture and its contract probe: handing the terminal to a child
//! process, resuming raw mode, and never letting the ceded side consume the ceded bytes.
//! Separate from the picker suites because the only thing these need is a real pty, and they
//! are the slowest probes in the view suite.

#[cfg(unix)]
use super::*;

/// PTY-driven proof that suspending the real renderer restores the process
/// terminal, that resuming re-enters raw mode and returns, and that a ceded
/// input stream never consumes the bytes the ceded side owns.
///
/// Inert without `OCTET_TEST_TERMINAL_HANDOFF`; the sibling
/// `terminal_handoff_pty_contract` test drives it through a pty.
#[cfg(unix)]
#[tokio::test]
async fn terminal_handoff_pty_fixture() {
    use futures_util::StreamExt as _;

    let Ok(directory) = std::env::var("OCTET_TEST_TERMINAL_HANDOFF") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let signal = |name: &str| {
        std::fs::write(directory.join(name), b"1").expect("handoff marker");
    };

    let theme = crate::tui::theme::test_theme();
    let size = Arc::new(Mutex::new(crossterm::terminal::size().unwrap_or((80, 24))));
    let mut shell =
        InteractiveShell::enter_with_mouse(theme, size, false).expect("enter raw terminal");
    let mut input =
        crate::tui::terminal::TerminalInput::new().with_cede_flag(shell.terminal_input_parking());
    signal("ready");

    // Arm the real host reader before cancellation and cede. Merely parking
    // before the first poll misses EventStream's detached background reader.
    assert!(
        tokio::time::timeout(Duration::from_millis(150), input.next())
            .await
            .is_err()
    );

    // Cede exactly as the arbiter does, then hold the terminal.
    shell.cede_terminal_input();
    shell.suspend();
    signal("ceded");

    // A parked stream never polls its source, so a ceded byte stays unread.
    let parked = tokio::time::timeout(Duration::from_millis(400), input.next()).await;
    assert!(
        parked.is_err(),
        "a ceded input stream must not consume terminal bytes"
    );

    // A separate process is the terminal holder; it must receive the bytes,
    // not merely find them buffered in the host after the grant ends.
    let holder = tokio::time::timeout(Duration::from_secs(3),
        tokio::process::Command::new("python3")
            .args(["-c", "import os,select; assert select.select([0],[],[],2)[0]; assert os.read(0,2)==b'X\\n'"])
            .stdin(std::process::Stdio::inherit()).output()).await
        .expect("terminal holder completed").expect("terminal holder started");
    assert!(
        holder.status.success(),
        "holder lost its input: {:?}",
        holder
    );

    signal("resuming");
    shell.release_terminal_input();
    shell.resume().expect("resume the renderer");
    let first = tokio::time::timeout(Duration::from_secs(10), input.next())
        .await
        .expect("released input is polled again")
        .expect("a terminal event")
        .expect("a decoded event");
    assert!(
        matches!(first, crossterm::event::Event::Key(key)
            if key.code == crossterm::event::KeyCode::Char('R')),
        "host input resumed after the holder consumed its bytes: {first:?}"
    );

    shell.leave();
}

/// Drives the fixture above through a pty and asserts termios restoration at
/// every handoff boundary. Skips nothing: `python3` is the same offline tool the
/// updater's pty lane already depends on.
#[cfg(unix)]
#[test]
fn terminal_handoff_pty_contract() {
    let output = std::process::Command::new("python3")
        .arg("-c")
        .arg(include_str!("../../terminal/handoff_pty.py"))
        .arg(std::env::current_exe().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
