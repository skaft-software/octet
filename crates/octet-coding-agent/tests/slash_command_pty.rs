#![cfg(unix)]

//! One-enter coverage against the real interactive binary and a controlling PTY.

use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
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
        Self::spawn_with_provider(binary, "http://127.0.0.1:9/v1/")
    }

    fn spawn_with_provider(binary: &Path, base_url: &str) -> Self {
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
            format!(
                r#"{{"base_url":"{base_url}","api_key":"","api_name":"probe","headers":[],"models":[],"auto_discover":false}}"#
            ),
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

/// Provider response bytes written as soon as the request arrives. The visible
/// content is rendered before the tail exists, so a transcript that shows it is
/// mid-response, not after completion.
const STREAM_HEAD: &[u8] = b"data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"streaming first chunk\"},\"finish_reason\":null}]}\n\n";
/// Withheld until the test releases it; its visible marker must be absent while
/// the slash-command output is asserted.
const STREAM_TAIL: &[u8] = concat!(
    "data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"stream tail complete\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
)
.as_bytes();
const HEAD_MARKER: &[u8] = b"streaming first chunk";
const TAIL_MARKER: &[u8] = b"stream tail complete";

/// Loopback provider that starts an SSE response, then holds the remaining
/// bytes until `release`. Nothing else can finish the run meanwhile.
struct StreamApi {
    url: String,
    seen: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    completed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl StreamApi {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        listener
            .set_nonblocking(true)
            .expect("nonblocking loopback listener");
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let (seen_tx, seen) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let completed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_completed = completed.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                socket
                    .set_read_timeout(Some(Duration::from_millis(500)))
                    .expect("fixture read timeout");
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .expect("fixture write timeout");
                let Some(headers) = read_request(&mut socket) else {
                    continue;
                };
                assert!(
                    headers.starts_with("POST /v1/chat/completions HTTP/1.1"),
                    "unexpected fixture request: {headers}"
                );
                let total = STREAM_HEAD.len() + STREAM_TAIL.len();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
                );
                if socket.write_all(response.as_bytes()).is_err()
                    || socket.write_all(STREAM_HEAD).is_err()
                    || socket.flush().is_err()
                {
                    continue;
                }
                if seen_tx.send(()).is_err() {
                    return;
                }
                loop {
                    if worker_stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match released.recv_timeout(Duration::from_millis(50)) {
                        Ok(()) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                // The client observes the tail, then EOF when this drops.
                let _ = socket.write_all(STREAM_TAIL);
                let _ = socket.flush();
                worker_completed.store(true, Ordering::SeqCst);
            }
        });
        Self {
            url,
            seen,
            release,
            completed,
            stop,
            worker: Some(worker),
        }
    }

    fn wait_for_request(&self, octet: &mut PtyOctet) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            octet.pty.read_available();
            match self.seen.try_recv() {
                Ok(()) => return,
                Err(mpsc::TryRecvError::Empty) => {}
                Err(error) => panic!("stream fixture closed: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "no provider request arrived; transcript: {}",
                visible_bytes(&octet.pty.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn release(&self) {
        let _ = self.release.send(());
    }
}

impl Drop for StreamApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.release.send(());
        if let Some(worker) = self.worker.take() {
            if let Err(panic) = worker.join() {
                if !thread::panicking() {
                    std::panic::resume_unwind(panic);
                }
            }
        }
    }
}

/// Read one request's headers, draining its body so the client is not blocked.
fn read_request(socket: &mut std::net::TcpStream) -> Option<String> {
    let mut request = Vec::new();
    let header_end = loop {
        let mut bytes = [0u8; 1024];
        let read = match socket.read(&mut bytes) {
            Ok(0) => return None,
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(error) => panic!("read loopback request: {error}"),
        };
        request.extend_from_slice(&bytes[..read]);
        assert!(request.len() <= 128 * 1024, "unbounded fixture request");
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        })
        .unwrap_or(0);
    while request.len() < header_end + length {
        let mut bytes = [0u8; 1024];
        let read = socket.read(&mut bytes).expect("read loopback body");
        assert!(read > 0, "request ended before body");
        request.extend_from_slice(&bytes[..read]);
    }
    Some(headers)
}

/// Start one held response and return a mid-response PTY.
fn streaming_octet() -> (StreamApi, PtyOctet) {
    let api = StreamApi::start();
    let mut octet = PtyOctet::spawn_with_provider(Path::new(env!("CARGO_BIN_EXE_octet")), &api.url);
    octet.pty.wait_for(b"custom/probe");
    octet.pty.write_input(b"stream a long answer\r");
    api.wait_for_request(&mut octet);
    octet.pty.wait_for(HEAD_MARKER);
    (api, octet)
}

#[test]
fn real_octet_slash_help_renders_while_a_response_is_streaming() {
    let (api, mut octet) = streaming_octet();
    octet.pty.write_input(b"/help\r");
    octet.pty.wait_for(b"Slash commands:");

    // The response tail cannot exist yet: the run is provably still open.
    assert!(
        !contains_bytes(&octet.pty.output, TAIL_MARKER),
        "help rendered only after the response finished; transcript: {}",
        visible_bytes(&octet.pty.output)
    );
    assert!(!api.completed.load(Ordering::SeqCst));

    api.release();
    // Enter dismisses the report overlay; an empty draft is not a follow-up.
    octet.pty.write_input(b"\r");
    octet.pty.wait_for(TAIL_MARKER);
    octet.shutdown();
}

#[test]
fn real_octet_slash_cost_renders_while_a_response_is_streaming() {
    let (api, mut octet) = streaming_octet();
    octet.pty.write_input(b"/cost\r");
    octet.pty.wait_for(b"Session cost");

    assert!(
        !contains_bytes(&octet.pty.output, TAIL_MARKER),
        "cost rendered only after the response finished; transcript: {}",
        visible_bytes(&octet.pty.output)
    );
    assert!(!api.completed.load(Ordering::SeqCst));

    api.release();
    octet.pty.write_input(b"\r");
    octet.pty.wait_for(TAIL_MARKER);
    octet.shutdown();
}

/// `/context` takes the other live-inspection route: the report is built from
/// the run's own `ContextSnapshot` rather than from the session store, so a
/// queued-to-idle implementation would not render it either.
#[test]
fn real_octet_slash_context_renders_while_a_response_is_streaming() {
    let (api, mut octet) = streaming_octet();
    octet.pty.write_input(b"/context\r");
    octet.pty.wait_for(b"Estimated next request");

    assert!(
        !contains_bytes(&octet.pty.output, TAIL_MARKER),
        "context rendered only after the response finished; transcript: {}",
        visible_bytes(&octet.pty.output)
    );
    assert!(!api.completed.load(Ordering::SeqCst));

    api.release();
    octet.pty.write_input(b"\r");
    octet.pty.wait_for(TAIL_MARKER);
    octet.shutdown();
}

/// The effort picker is opened inline by the active-run dispatcher, not queued
/// to the idle boundary with the setting change it produces.
#[test]
fn real_octet_slash_thinking_opens_the_effort_menu_while_a_response_is_streaming() {
    let (api, mut octet) = streaming_octet();
    octet.pty.write_input(b"/thinking\r");
    octet.pty.wait_for(b"Select thinking level");

    assert!(
        !contains_bytes(&octet.pty.output, TAIL_MARKER),
        "the effort menu opened only after the response finished; transcript: {}",
        visible_bytes(&octet.pty.output)
    );
    assert!(!api.completed.load(Ordering::SeqCst));

    // Escape cancels the picker without changing the preference, then the
    // withheld response tail settles the still-active run.
    octet.pty.write_input(b"\x1b");
    api.release();
    octet.pty.wait_for(TAIL_MARKER);
    octet.shutdown();
}

#[test]
fn real_octet_slash_model_opens_the_picker_while_a_response_is_streaming() {
    let (api, mut octet) = streaming_octet();
    octet.pty.write_input(b"/model\r");
    octet.pty.wait_for(b"Select model");

    assert!(
        !contains_bytes(&octet.pty.output, TAIL_MARKER),
        "the model picker opened only after the response finished; transcript: {}",
        visible_bytes(&octet.pty.output)
    );
    assert!(!api.completed.load(Ordering::SeqCst));

    // Escape cancels the picker without changing the model, then the withheld
    // response tail settles the still-active run.
    octet.pty.write_input(b"\x1b");
    api.release();
    octet.pty.wait_for(TAIL_MARKER);
    octet.shutdown();
}

#[test]
fn real_octet_model_picker_selects_and_persists_without_inference() {
    let mut octet = PtyOctet::spawn(Path::new(env!("CARGO_BIN_EXE_octet")));
    octet.pty.wait_for(b"custom/probe");
    octet.pty.write_input(b"/model\r");
    octet.pty.wait_for(b"Select model");
    // Filter the actual picker, then submit its selected local fixture model.
    // The fixture is offline with no tools or usable inference endpoint.
    octet.pty.write_input(b"probe\r");
    let config = octet._root.path().join("home/.octet/config.toml");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        octet.pty.read_available();
        if fs::read_to_string(&config).is_ok_and(|text| text.contains("model = \"custom/probe\"")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "model selection was not persisted; transcript: {}",
            visible_bytes(&octet.pty.output)
        );
        thread::sleep(Duration::from_millis(5));
    }
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
