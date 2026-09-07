#![cfg(unix)]

//! OSC 11 input regressions against the actual crossterm parser and octet binary.
//! Isolated HOME/workspace, --offline, no tools/context files, no submitted
//! prompt, and only an inert loopback provider record; no live credentials.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const QUERY: &[u8] = b"\x1b]11;?\x1b\\";
const FRAME_END: &[u8] = b"\x1b[?2026l";
const TIMEOUT: Duration = Duration::from_secs(5);

struct PtyOctet {
    child: Child,
    master: File,
    _slave: File,
    _root: TempDir,
    output: Vec<u8>,
    parser: vt100::Parser,
    frames: Vec<String>,
}

impl PtyOctet {
    fn spawn(theme: Option<&str>, model: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let sessions = root.path().join("sessions");
        fs::create_dir_all(home.join(".octet/credentials")).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        if model {
            let credentials = home.join(".octet/credentials/custom.json");
            fs::write(
                &credentials,
                r#"{"base_url":"http://127.0.0.1:9/v1/","api_key":"","api_name":"probe","headers":[],"models":[],"auto_discover":false}"#,
            )
            .unwrap();
            fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let (mut master_fd, mut slave_fd) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 24,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: all output pointers refer to live locals; the two successful
        // descriptors are immediately owned by Files below.
        let result = unsafe {
            #[allow(clippy::unnecessary_mut_passed)]
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        assert_eq!(result, 0, "openpty: {}", io::Error::last_os_error());
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        for fd in [master_fd, slave_fd] {
            assert_ne!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                -1
            );
        }
        let flags = unsafe { libc::fcntl(master_fd, libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_ne!(
            unsafe { libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            -1
        );

        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command
            .args([
                "--offline",
                "--no-context-files",
                "--no-tools",
                "--color",
                "always",
            ])
            .arg("--workspace")
            .arg(&workspace)
            .arg("--session-dir")
            .arg(&sessions)
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &workspace)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LANG", "C.UTF-8")
            // SSH is not evidence of missing terminal color capabilities.
            .env("SSH_CONNECTION", "127.0.0.1 12345 127.0.0.1 22")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        if let Some(theme) = theme {
            command.args(["--theme", theme]);
        }
        if model {
            command.args(["--model", "custom/probe"]);
        }
        // SAFETY: the pre-exec callback uses only async-signal-safe Unix calls;
        // the parent retains the slave until after its child exits.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1
                    || libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            child: command.spawn().expect("spawn offline octet under PTY"),
            master,
            _slave: slave,
            _root: root,
            output: Vec::new(),
            parser: vt100::Parser::new(24, 100, 0),
            frames: Vec::new(),
        }
    }

    fn read_available(&mut self) {
        let mut buffer = [0; 8192];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    for byte in &buffer[..read] {
                        self.output.push(*byte);
                        self.parser.process(std::slice::from_ref(byte));
                        if self.output.ends_with(FRAME_END) {
                            self.frames.push(self.parser.screen().contents());
                        }
                    }
                    assert!(self.output.len() < 4 * 1024 * 1024, "unbounded PTY output");
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("PTY read: {error}"),
            }
        }
    }

    fn send(&mut self, input: &[u8]) {
        self.master.write_all(input).unwrap();
        self.master.flush().unwrap();
    }

    fn wait_until(&mut self, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.read_available();
            if predicate(self) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "PTY wait failed; screen: {:?}; bytes: {:?}",
                self.parser.screen().contents(),
                String::from_utf8_lossy(&self.output),
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_for_query(&mut self) {
        self.wait_until(|pty| pty.output.windows(QUERY.len()).any(|bytes| bytes == QUERY));
    }

    fn wait_for_screen(&mut self, text: &str) {
        self.wait_until(|pty| pty.parser.screen().contents().contains(text));
    }

    fn drain_for(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.read_available();
            thread::sleep(Duration::from_millis(2));
        }
        self.read_available();
    }

    fn assert_no_reply_in_frames(&self) {
        for screen in &self.frames {
            assert!(
                !screen.contains("1e1e") && !screen.contains("11;rgb:"),
                "terminal reply reached an input surface: {screen:?}",
            );
        }
    }

    fn close(mut self) {
        self.send(&[4]); // genuine Ctrl-D must still close from every surface
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.read_available();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "Ctrl-D exit: {status}");
                return;
            }
            assert!(Instant::now() < deadline, "Ctrl-D did not close the TUI");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for PtyOctet {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn send_reply(pty: &mut PtyOctet, ending: &[u8], fragmented: bool) {
    let mut reply = b"\x1b]11;rgb:1e1e/1e1e/1e1e".to_vec();
    reply.extend_from_slice(ending);
    if fragmented {
        // Split both the OSC opener and ST, not just the printable body. This
        // exercises Esc + ']' as well as crossterm's usual Alt+] representation.
        for byte in reply {
            pty.send(&[byte]);
            pty.drain_for(Duration::from_millis(20));
        }
    } else {
        pty.send(&reply);
    }
}

#[test]
fn late_terminal_reply_complete_and_fragmented_bel_st_keep_typing_and_paste() {
    for ending in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
        for fragmented in [false, true] {
            let mut pty = PtyOctet::spawn(Some("auto"), true);
            pty.wait_for_query();
            // Genuine typing during the old synchronous probe was consumed
            // and lost. It must survive the probe and startup lifecycle waits.
            pty.send(b"typed ");
            pty.drain_for(Duration::from_millis(400));
            pty.wait_for_screen("custom/probe");
            send_reply(&mut pty, ending, fragmented);
            pty.send("\x1b[200~pasted 雪\x1b[201~".as_bytes());
            pty.wait_for_screen("typed pasted 雪");
            pty.drain_for(Duration::from_millis(300));
            pty.assert_no_reply_in_frames();
            // Ctrl-C clears the retained draft rather than exiting or being
            // swallowed as a reply; subsequent ordinary input still works.
            pty.send(&[3]);
            pty.wait_until(|pty| !pty.parser.screen().contents().contains("typed pasted 雪"));
            pty.send(b"after-clear");
            pty.wait_for_screen("after-clear");
            pty.close();
        }
    }
}

#[test]
fn late_terminal_reply_startup_timeout_mid_fragment_keeps_the_reply_out_of_input() {
    let mut pty = PtyOctet::spawn(Some("auto"), true);
    pty.wait_for_query();
    pty.send(b"\x1b]11;rgb:");
    // Cross the 120 ms color-probe deadline. The identified reply has no
    // fragment idle deadline; startup and the editor keep the same parser.
    pty.drain_for(Duration::from_millis(170));
    pty.send(b"1e1e/1e1e/1e1e\x1b");
    pty.drain_for(Duration::from_millis(20));
    pty.send(b"\\");
    pty.wait_for_screen("custom/probe");
    pty.send(b"after-handoff");
    pty.wait_for_screen("after-handoff");
    pty.assert_no_reply_in_frames();
    pty.close();
}

#[test]
fn late_terminal_reply_after_six_seconds_keeps_the_editor_clean() {
    let mut pty = PtyOctet::spawn(Some("auto"), true);
    pty.wait_for_query();
    pty.drain_for(Duration::from_secs(6));
    pty.wait_for_screen("custom/probe");
    pty.send(b"very-late ");
    pty.wait_for_screen("very-late");
    send_reply(&mut pty, b"\x1b\\", false);
    pty.send(b"\x1b[200~paste-kept\x1b[201~");
    pty.wait_for_screen("very-late paste-kept");
    pty.assert_no_reply_in_frames();
    pty.close();
}

#[test]
fn late_terminal_reply_slow_fragments_keep_paste_without_payload_expiry() {
    let mut pty = PtyOctet::spawn(Some("auto"), true);
    pty.wait_for_query();
    pty.drain_for(Duration::from_millis(400));
    pty.wait_for_screen("custom/probe");
    // Once the complete header identifies the response, no idle or total
    // deadline may turn its printable body (or a split ST) into editor input.
    pty.send(b"\x1b]11;");
    pty.drain_for(Duration::from_millis(700));
    pty.send(b"\x1b[200~paste-during-reply\x1b[201~");
    pty.wait_for_screen("paste-during-reply");
    for fragment in ["rgb:", "1e1e/", "1e1e/", "1e1e", "\x1b", "\\"] {
        pty.send(fragment.as_bytes());
        pty.drain_for(Duration::from_millis(700));
    }
    pty.send(b" done");
    pty.wait_for_screen("paste-during-reply done");
    pty.assert_no_reply_in_frames();
    pty.close();
}

#[test]
fn late_terminal_reply_immediate_response_and_explicit_appearance_preserve_input() {
    let mut immediate = PtyOctet::spawn(Some("auto"), true);
    immediate.wait_for_query();
    send_reply(&mut immediate, b"\x07", false);
    immediate.wait_for_screen("custom/probe");
    immediate.send(b"immediate-kept");
    immediate.wait_for_screen("immediate-kept");
    immediate.assert_no_reply_in_frames();
    immediate.close();

    for theme in ["dark", "light"] {
        let mut explicit = PtyOctet::spawn(Some(theme), true);
        explicit.wait_for_screen("custom/probe");
        explicit.drain_for(Duration::from_millis(400));
        assert!(!explicit
            .output
            .windows(QUERY.len())
            .any(|bytes| bytes == QUERY));
        explicit.send(b"explicit-kept");
        explicit.wait_for_screen("explicit-kept");
        explicit.close();
    }
}

#[test]
fn late_terminal_reply_cannot_type_into_or_close_appearance_and_provider_panels() {
    for model in [true, false] {
        // No explicit theme opens the appearance panel before readiness. With
        // no model, dismiss it and exercise the initial provider setup panel.
        let mut pty = PtyOctet::spawn(None, model);
        pty.wait_for_query();
        pty.wait_for_screen("Choose terminal appearance");
        if !model {
            pty.send(b"\r");
            pty.wait_for_screen("Set up a provider");
        }
        pty.drain_for(Duration::from_millis(400));
        send_reply(&mut pty, b"\x1b\\", true);
        pty.drain_for(Duration::from_millis(300));
        pty.assert_no_reply_in_frames();
        assert!(pty.parser.screen().contents().contains(if model {
            "Choose terminal appearance"
        } else {
            "Set up a provider"
        }));
        if !model {
            // The initial model-less provider panel remains operable before
            // finish_startup: End + Enter selects Continue without a provider.
            pty.send(b"\x1b[F\r");
            pty.wait_for_screen("No configured model");
        }
        pty.close();
    }
}
