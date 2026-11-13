//! Human command output at redirected and terminal boundaries.

use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for path in ["home", "tmp", "work", "pi"] {
            std::fs::create_dir(root.path().join(path)).unwrap();
        }
        Self { root }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .current_dir(self.root.path().join("work"))
            .args(["--offline", "--no-extensions", "--no-context-files"])
            .args(args);
        command
    }
}

#[cfg(unix)]
fn terminal_capture(mut command: Command, columns: u16) -> (String, Output) {
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::fd::FromRawFd;
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 40,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes both descriptors; Files take sole ownership.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(size),
            )
        },
        0
    );
    let mut reader = unsafe { File::from_raw_fd(master) };
    let writer = unsafe { File::from_raw_fd(slave) };
    command.stdout(writer).stderr(std::process::Stdio::piped());
    let child = command.spawn().unwrap();
    drop(command);
    let drain = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
        String::from_utf8(bytes).unwrap().replace("\r\n", "\n")
    });
    let output = child.wait_with_output().unwrap();
    (drain.join().unwrap(), output)
}

#[cfg(unix)]
fn terminal_output(command: Command, columns: u16) -> String {
    let (text, output) = terminal_capture(command, columns);
    assert!(output.status.success(), "{output:?}");
    text
}

fn stdout(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn migration_pipe_is_plain() {
    let fixture = Fixture::new();
    let pi = fixture.root.path().join("pi");
    let output = stdout(
        fixture
            .command(&[
                "migrate",
                "pi",
                "--dry-run",
                "--pi-home",
                pi.to_str().unwrap(),
            ])
            .output()
            .unwrap(),
    );
    assert!(output.starts_with("Your Pi setup  "));
    assert!(!output.contains('\x1b'));
    assert!(!output.contains('␛'));
}

#[cfg(unix)]
#[test]
fn migration_terminal_styles_follow_color_policy() {
    let fixture = Fixture::new();
    let pi = fixture.root.path().join("pi\x1b[31m");
    for width in [80, 120] {
        let args = [
            "migrate",
            "pi",
            "--dry-run",
            "--pi-home",
            pi.to_str().unwrap(),
        ];
        let styled = terminal_output(fixture.command(&args), width);
        assert!(
            styled.starts_with("\x1b[1mYour Pi setup\x1b[0m  \x1b[2m"),
            "{styled:?}"
        );
        assert!(
            styled.contains("pi␛[31m"),
            "untrusted ANSI stays neutralized"
        );
        assert!(!styled.contains("pi\x1b[31m"));
        for flag in ["never", "off"] {
            let mut command = fixture.command(&["--color", flag]);
            command.args(args);
            let plain = terminal_output(command, width);
            assert!(plain.starts_with("Your Pi setup  "), "{plain:?}");
            assert!(!plain.contains('\x1b'));
        }
        let mut command = fixture.command(&args);
        command.env("NO_COLOR", "1");
        assert!(!terminal_output(command, width).contains('\x1b'));
    }
}

#[test]
fn eval_report_keeps_lines_in_pipes_and_terminals() {
    let fixture = Fixture::new();
    let suite = fixture.root.path().join("suite.json");
    std::fs::write(&suite, r#"{"schema":"octet-eval-suite-1","name":"layout","cases":[{"id":"greeting","prompt":"hello","expect":{"contains":"hello"},"fixture":{"response":"hello"}}]}"#).unwrap();
    let args = ["eval", "run", suite.to_str().unwrap()];
    let check = |text: &str| {
        assert!(
            text.starts_with("Eval layout — 1 case(s), 1 passed, 0 failed (100.0%)\n  backend "),
            "{text:?}"
        );
        assert!(text.contains("\n  PASS greeting — "), "{text:?}");
        assert!(text.contains("\n  latency mean "), "{text:?}");
        assert!(text.contains("\nArtifacts: "), "{text:?}");
        assert!(!text.contains("<U+000A>"));
        assert!(!text.contains('\x1b'));
    };
    check(&stdout(fixture.command(&args).output().unwrap()));
    #[cfg(unix)]
    for width in [80, 120] {
        check(&terminal_output(fixture.command(&args), width));
    }
}

#[test]
fn share_review_keeps_fields_on_separate_control_safe_lines() {
    use octet_agent::{EntryValue, Session};
    use octet_ai::{Message, UserMessage, UserPart};
    let fixture = Fixture::new();
    let source = fixture.root.path().join("source.jsonl");
    let mut session = Session::create(&source).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("share fixture".into())],
        })))
        .unwrap();
    drop(session);
    let imported = stdout(
        fixture
            .command(&["sessions", "import", source.to_str().unwrap()])
            .output()
            .unwrap(),
    );
    let id = imported.split_whitespace().nth(3).unwrap();
    let args = ["sessions", "share", id];
    // No --yes and no terminal stdin: these reviews cannot publish anything.
    let check = |text: &str, output: &Output| {
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("share requires attended confirmation"));
        let lines = text.lines().collect::<Vec<_>>();
        assert!(lines[1].starts_with("Review: "), "{text:?}");
        assert!(lines[2].starts_with("SHA-256: "), "{text:?}");
        assert_eq!(lines[3], "Redacted values: 0");
        assert_eq!(lines.len(), 4);
        assert!(!text.contains('\x1b'));
    };
    let mut command = fixture.command(&args);
    command.stdin(std::process::Stdio::null());
    let output = command.output().unwrap();
    check(std::str::from_utf8(&output.stdout).unwrap(), &output);
    #[cfg(unix)]
    for width in [80, 120] {
        let mut command = fixture.command(&args);
        // A hostile TMPDIR must not inject additional review lines or commands.
        let tmp = fixture.root.path().join("tmp\nforged\x1b");
        std::fs::create_dir_all(&tmp).unwrap();
        command
            .env("TMPDIR", tmp)
            .stdin(std::process::Stdio::null());
        let (text, output) = terminal_capture(command, width);
        check(&text, &output);
    }
}
