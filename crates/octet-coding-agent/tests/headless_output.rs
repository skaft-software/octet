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
            .args(args);
        command
    }
}

#[cfg(unix)]
fn terminal_output(mut command: Command, columns: u16) -> String {
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::fd::FromRawFd;
    let mut master = -1;
    let mut slave = -1;
    let size = libc::winsize {
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
                std::ptr::null(),
                &size,
            )
        },
        0
    );
    let mut reader = unsafe { File::from_raw_fd(master) };
    let writer = unsafe { File::from_raw_fd(slave) };
    command.stdout(writer);
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
    assert!(child.wait_with_output().unwrap().status.success());
    drain.join().unwrap()
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
