//! Screen-local Kitty keyboard state and modified-Enter coverage over a PTY.
use super::*;

/// Model the two independent keyboard stacks of a Kitty-protocol terminal.
/// The PTY is only a byte transport and does not implement these stacks itself.
fn keyboard_state(output: &[u8]) -> (bool, Vec<u8>, Vec<u8>) {
    let mut fullscreen = false;
    let mut primary = Vec::new();
    let mut alternate = Vec::new();
    let mut index = 0;
    while index + 2 < output.len() {
        if &output[index..index + 2] != b"\x1b[" {
            index += 1;
            continue;
        }
        let start = index + 2;
        let Some(end) = (start..output.len()).find(|&i| (0x40..=0x7e).contains(&output[i])) else {
            break;
        };
        let sequence = &output[start..=end];
        match sequence {
            b"?1049h" => fullscreen = true,
            b"?1049l" => {
                assert!(
                    alternate.is_empty(),
                    "alternate keyboard stack was not popped before screen exit: {alternate:?}"
                );
                fullscreen = false;
                alternate.clear();
            }
            _ if sequence.starts_with(b">") && sequence.ends_with(b"u") => {
                if let Ok(flags) = std::str::from_utf8(&sequence[1..sequence.len() - 1])
                    .unwrap()
                    .parse::<u8>()
                {
                    if fullscreen {
                        &mut alternate
                    } else {
                        &mut primary
                    }
                    .push(flags);
                }
            }
            b"<u" | b"<1u" => {
                let stack = if fullscreen {
                    &mut alternate
                } else {
                    &mut primary
                };
                assert!(
                    stack.pop().is_some(),
                    "popped an empty keyboard stack on fullscreen={fullscreen}"
                );
            }
            _ => {}
        }
        index = end + 1;
    }
    (fullscreen, primary, alternate)
}

#[test]
fn fullscreen_requests_shift_enter_on_the_active_keyboard_stack() {
    let _lock = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut octet = PtyOctet::spawn_default_fullscreen(
        Path::new(env!("CARGO_BIN_EXE_octet")),
        MouseMode::Default,
        StartupFixture::Model("probe"),
    );
    let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 0);
    let mut consumed = 0;
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "custom/probe",
        STARTUP_TIMEOUT,
    );

    let (fullscreen, primary, alternate) = keyboard_state(&octet.pty.output);
    assert!(fullscreen);
    assert!(primary.last().is_some_and(|flags| flags & 1 != 0));
    assert!(
        alternate.last().is_some_and(|flags| flags & 1 != 0),
        "Shift+Enter is indistinguishable from Enter on the active screen: primary={primary:?}, alternate={alternate:?}"
    );

    octet.pty.write_input(b"FIRST655\x1b[13;2uSECOND655");
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "SECOND655",
        STARTUP_TIMEOUT,
    );
    let contents = parser.screen().contents();
    let lines: Vec<_> = contents.lines().collect();
    let first = lines
        .iter()
        .position(|line| line.contains("FIRST655"))
        .unwrap();
    assert!(lines[first + 1].contains("SECOND655"), "{contents}");

    let capture = octet.shutdown();
    assert!(capture.status.success());
    assert!(capture.termios_restored);
    let (fullscreen, primary, alternate) = keyboard_state(&capture.output);
    assert!(!fullscreen);
    assert!(
        primary.is_empty(),
        "primary keyboard stack was not restored: {primary:?}"
    );
    assert!(
        alternate.is_empty(),
        "alternate keyboard stack was not restored: {alternate:?}"
    );
}

#[test]
fn modified_enter_survives_live_screen_switches_and_inline() {
    let _lock = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut octet = PtyOctet::spawn(Path::new(env!("CARGO_BIN_EXE_octet")), MouseMode::Default);
    let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 0);
    let mut consumed = 0;
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "custom/probe",
        STARTUP_TIMEOUT,
    );

    for (step, fullscreen) in [false, true, false, true].into_iter().enumerate() {
        if step > 0 {
            let before = octet.pty.output.len();
            let mode = if fullscreen { "fullscreen" } else { "inline" };
            octet
                .pty
                .write_input(format!("/settings tui {mode}\r").as_bytes());
            let deadline = Instant::now() + STARTUP_TIMEOUT;
            loop {
                octet.pty.read_available();
                parser.process(&octet.pty.output[consumed..]);
                consumed = octet.pty.output.len();
                let (active, primary, alternate) = keyboard_state(&octet.pty.output);
                if active == fullscreen
                    && parser.screen().alternate_screen() == fullscreen
                    && primary.last().is_some_and(|flags| flags & 1 != 0)
                    && (!fullscreen || alternate.last().is_some_and(|flags| flags & 1 != 0))
                    && super::mouse::renderer_reentered(&octet.pty.output[before..])
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "screen/keyboard transition to {mode} timed out"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        let (active, primary, alternate) = keyboard_state(&octet.pty.output);
        assert_eq!(active, fullscreen);
        assert!(primary.last().is_some_and(|flags| flags & 1 != 0));
        if fullscreen {
            assert!(alternate.last().is_some_and(|flags| flags & 1 != 0));
        } else {
            assert!(alternate.is_empty());
        }

        for (index, newline) in ["\x1b[13;2u", "\x1b[27;2;13~", "\n"]
            .into_iter()
            .enumerate()
        {
            let first = format!("FIRST{step}{index}X");
            let second = format!("SECOND{step}{index}X");
            octet
                .pty
                .write_input(format!("{first}{newline}{second}").as_bytes());
            await_screen(
                &mut octet,
                &mut parser,
                &mut consumed,
                &second,
                STARTUP_TIMEOUT,
            );
            let contents = parser.screen().contents();
            let lines: Vec<_> = contents.lines().collect();
            let row = lines.iter().position(|line| line.contains(&first)).unwrap();
            assert!(
                lines[row + 1].contains(&second),
                "{mode:?} {newline:?}: {contents}",
                mode = fullscreen
            );
            assert_eq!(
                contents.matches(&first).count(),
                1,
                "draft was submitted: {contents}"
            );
            octet.pty.write_input(b"\x03");
            let deadline = Instant::now() + STARTUP_TIMEOUT;
            loop {
                octet.pty.read_available();
                parser.process(&octet.pty.output[consumed..]);
                consumed = octet.pty.output.len();
                if !parser.screen().contents().contains(&second) {
                    break;
                }
                assert!(Instant::now() < deadline, "draft was not cleared");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    let capture = octet.shutdown();
    assert!(capture.status.success());
    assert!(capture.termios_restored);
    let (fullscreen, primary, alternate) = keyboard_state(&capture.output);
    assert!(!fullscreen);
    assert!(
        primary.is_empty(),
        "primary stack was not restored: {primary:?}"
    );
    assert!(alternate.is_empty());
}

#[test]
fn signal_shutdown_restores_both_keyboard_stacks() {
    let _lock = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut octet = PtyOctet::spawn_default_fullscreen(
        Path::new(env!("CARGO_BIN_EXE_octet")),
        MouseMode::Default,
        StartupFixture::Model("probe"),
    );
    let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 0);
    let mut consumed = 0;
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "custom/probe",
        STARTUP_TIMEOUT,
    );
    let (fullscreen, primary, alternate) = keyboard_state(&octet.pty.output);
    assert!(fullscreen && !primary.is_empty() && !alternate.is_empty());
    assert_eq!(
        unsafe { libc::kill(octet.child.id() as i32, libc::SIGTERM) },
        0
    );
    let capture = octet.shutdown_with_input(&[]);
    assert_eq!(capture.status.code(), Some(128 + libc::SIGTERM));
    assert!(capture.termios_restored);
    let (fullscreen, primary, alternate) = keyboard_state(&capture.output);
    assert!(!fullscreen);
    assert!(
        primary.is_empty(),
        "primary stack was not restored: {primary:?}"
    );
    assert!(alternate.is_empty());
}
