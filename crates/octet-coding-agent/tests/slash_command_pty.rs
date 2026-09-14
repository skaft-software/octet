#![cfg(unix)]

//! One-enter coverage against the real interactive binary and a controlling PTY.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const COLUMNS: u16 = 96;
const ROWS: u16 = 18;
const TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

struct Pty {
    master: fs::File,
    slave: fs::File,
    output: Vec<u8>,
}

impl Pty {
    fn open() -> Self {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut size = libc::winsize {
            ws_row: ROWS,
            ws_col: COLUMNS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe {
            // macOS declares `winp` mutable while Linux declares it const.
            #[allow(clippy::unnecessary_mut_passed)]
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        assert_eq!(result, 0, "openpty failed: {}", io::Error::last_os_error());
        set_close_on_exec(master_fd);
        set_close_on_exec(slave_fd);
        set_nonblocking(master_fd);
        Self {
            // SAFETY: openpty returned owned descriptors on success.
            master: unsafe { fs::File::from_raw_fd(master_fd) },
            slave: unsafe { fs::File::from_raw_fd(slave_fd) },
            output: Vec::new(),
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("write PTY input");
        self.master.flush().expect("flush PTY input");
    }

    fn read_available(&mut self) {
        let mut buffer = [0u8; 8192];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => self.output.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                // PTY masters commonly report EIO after the last slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
    }

    fn wait_for(&mut self, needle: &[u8]) {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            self.read_available();
            if contains_bytes(&self.output, needle) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "PTY did not receive {needle:?}; transcript: {}",
            visible_bytes(&self.output)
        );
    }
}

struct PtyOctet {
    child: Child,
    pty: Pty,
    _root: TempDir,
}

impl PtyOctet {
    fn spawn(binary: &Path) -> Self {
        let root = tempfile::tempdir().expect("PTY fixture tempdir");
        let root_path = root
            .path()
            .canonicalize()
            .expect("canonical PTY fixture root");
        let home = root_path.join("home");
        let workspace = root_path.join("workspace");
        let sessions = root_path.join("sessions");
        fs::create_dir_all(home.join(".octet/credentials")).expect("credential directory");
        fs::create_dir_all(&workspace).expect("workspace directory");
        fs::create_dir_all(&sessions).expect("session directory");
        let credential = home.join(".octet/credentials/custom.json");
        fs::write(
            &credential,
            r#"{"base_url":"http://127.0.0.1:9/v1/","api_key":"","api_name":"probe","headers":[],"models":[],"auto_discover":false}"#,
        )
        .expect("custom-provider fixture");
        let mut permissions = fs::metadata(&credential)
            .expect("credential fixture metadata")
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&credential, permissions).expect("credential fixture permissions");

        let pty = Pty::open();
        let stdin = duplicate_stdio(pty.slave.as_raw_fd());
        let stdout = duplicate_stdio(pty.slave.as_raw_fd());
        let stderr = duplicate_stdio(pty.slave.as_raw_fd());
        let tty_fd = pty.slave.as_raw_fd();
        let mut command = Command::new(binary);
        command
            .args([
                "--offline",
                "--no-context-files",
                "--no-tools",
                "--color",
                "never",
                "--mouse",
                "off",
                "--workspace",
            ])
            .arg(&workspace)
            .arg("--session-dir")
            .arg(&sessions)
            .args(["--model", "custom/probe", "--theme", "dark"])
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &workspace)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LANG", "C.UTF-8")
            .env("OCTET_COLOR_SCHEME", "dark")
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr);
        // `openpty` does not make the slave a controlling terminal by itself.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(tty_fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn octet under PTY");
        Self {
            child,
            pty,
            _root: root,
        }
    }

    fn shutdown(mut self) {
        self.pty.write_input(b"\x04");
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        let status = loop {
            self.pty.read_available();
            if let Some(status) = self.child.try_wait().expect("poll octet") {
                break status;
            }
            if Instant::now() >= deadline {
                unsafe {
                    let _ = libc::kill(self.child.id() as i32, libc::SIGKILL);
                }
                let _ = self.child.wait();
                panic!(
                    "octet did not stop after Ctrl-D; transcript: {}",
                    visible_bytes(&self.pty.output)
                );
            }
            thread::sleep(Duration::from_millis(5));
        };
        assert!(
            status.success(),
            "octet exited unsuccessfully ({status}); transcript: {}",
            visible_bytes(&self.pty.output)
        );
    }
}

impl Drop for PtyOctet {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                let _ = libc::kill(self.child.id() as i32, libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}

#[test]
fn real_octet_slash_enter_invokes_highlighted_command_in_one_submission() {
    let mut octet = PtyOctet::spawn(Path::new(env!("CARGO_BIN_EXE_octet")));
    octet.pty.wait_for(b"custom/probe");

    // `/changelog` has one matching popup row. Down keeps that row highlighted;
    // the following single Enter must open its report rather than only filling
    // the composer for a later submission.
    octet.pty.write_input(b"/changelog\x1b[B\r");
    octet.pty.wait_for(b"Changelog");
    octet.shutdown();
}

fn duplicate_stdio(fd: RawFd) -> Stdio {
    let duplicated = unsafe { libc::dup(fd) };
    assert!(
        duplicated >= 0,
        "dup PTY slave failed: {}",
        io::Error::last_os_error()
    );
    // SAFETY: dup returned a new owned descriptor.
    Stdio::from(unsafe { fs::File::from_raw_fd(duplicated) })
}

fn set_close_on_exec(fd: RawFd) {
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    assert_eq!(
        result,
        0,
        "fcntl(FD_CLOEXEC) failed: {}",
        io::Error::last_os_error()
    );
}

fn set_nonblocking(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(
        flags >= 0,
        "fcntl(F_GETFL) failed: {}",
        io::Error::last_os_error()
    );
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_eq!(
        result,
        0,
        "fcntl(O_NONBLOCK) failed: {}",
        io::Error::last_os_error()
    );
}

fn contains_bytes(bytes: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && bytes.windows(needle.len()).any(|window| window == needle)
}

fn visible_bytes(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 4096;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BYTES)]);
    let escaped = text.escape_default().to_string();
    if bytes.len() > MAX_BYTES {
        format!("{escaped}… ({} bytes total)", bytes.len())
    } else {
        escaped
    }
}
