#![cfg(unix)]

//! Real-binary VT100 coverage for the first-run local-provider setup journey.
//!
//! Every process owns a disposable HOME, workspace, and session directory. The
//! only endpoint used by the online cases is a fresh loopback listener. The
//! tests never submit an inference request, use live credentials, or retain a
//! secret in a fixture.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use unicode_width::UnicodeWidthStr;

const COLUMNS: u16 = 80;
const REVIEW_COLUMNS: u16 = 1024;
const ROWS: u16 = 24;
const NARROW_COLUMNS: u16 = 40;
const NARROW_ROWS: u16 = 8;
const WAIT: Duration = Duration::from_secs(8);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);
const DRAIN: Duration = Duration::from_millis(30);
const MAX_READ_BYTES_PER_POLL: usize = 64 * 1024;
const MAX_PTY_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const FRAME_BEGIN: &[u8] = b"\x1b[?2026h";
const FRAME_END: &[u8] = b"\x1b[?2026l";
const SECRET: &str = "setup-secret-never-render";

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Clone, Copy)]
enum ServerReply {
    Models,
    Unauthorized,
}

/// A bounded HTTP fixture for exactly one explicit `/models` probe.
struct SetupServer {
    address: SocketAddr,
    requests: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SetupServer {
    fn start(reply: ServerReply) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback setup fixture");
        listener
            .set_nonblocking(true)
            .expect("make setup fixture nonblocking");
        let address = listener.local_addr().expect("setup fixture address");
        let requests = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_stopped = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            while !thread_stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if thread_stopped.load(Ordering::Acquire) {
                            return;
                        }
                        thread_requests.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let mut request = Vec::new();
                        let mut buffer = [0u8; 1024];
                        while !request.windows(4).any(|window| window == b"\r\n\r\n")
                            && request.len() < 16 * 1024
                        {
                            match stream.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(read) => request.extend_from_slice(&buffer[..read]),
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                                    ) =>
                                {
                                    break
                                }
                                Err(_) => break,
                            }
                        }
                        let (status, content_type, body) = match reply {
                            ServerReply::Models => (
                                "200 OK",
                                "application/json",
                                br#"{"data":[{"id":"fixture-model","name":"Fixture Model"}]}"#
                                    .as_slice(),
                            ),
                            ServerReply::Unauthorized => (
                                "401 Unauthorized",
                                "application/json",
                                br#"{"error":"unauthorized"}"#.as_slice(),
                            ),
                        };
                        let response = format!(
                            concat!(
                                "HTTP/1.1 {}\r\nContent-Type: {}\r\n",
                                "Content-Length: {}\r\nConnection: close\r\n\r\n"
                            ),
                            status,
                            content_type,
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(body);
                        let _ = stream.flush();
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            requests,
            stopped,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/v1/", self.address)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for SetupServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Wake the nonblocking accept loop so dropping a test cannot leave a
        // fixture thread behind.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct PtyTerminal {
    master: File,
    slave: File,
    original_termios: libc::termios,
    output: Vec<u8>,
}

impl PtyTerminal {
    fn open(columns: u16, rows: u16) -> Self {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = unsafe {
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
        assert_eq!(opened, 0, "openpty failed: {}", io::Error::last_os_error());
        set_close_on_exec(master_fd);
        set_close_on_exec(slave_fd);
        set_nonblocking(master_fd);
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        let original_termios = terminal_attributes(slave.as_raw_fd());
        Self {
            master,
            slave,
            original_termios,
            output: Vec::new(),
        }
    }

    fn write_input(&mut self, input: &[u8]) {
        self.master.write_all(input).expect("write PTY input");
        self.master.flush().expect("flush PTY input");
    }

    fn read_available(&mut self) {
        let mut buffer = [0u8; 8192];
        let mut read_bytes = 0;
        while read_bytes < MAX_READ_BYTES_PER_POLL {
            match self.master.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => {
                    read_bytes += read;
                    self.output.extend_from_slice(&buffer[..read]);
                    assert!(
                        self.output.len() <= MAX_PTY_OUTPUT_BYTES,
                        "PTY output exceeded {MAX_PTY_OUTPUT_BYTES} bytes"
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                // PTY masters commonly report EIO after the last slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
    }

    fn drain(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.read_available();
            thread::sleep(Duration::from_millis(2));
        }
        self.read_available();
    }
}

struct PtyOctet {
    child: Child,
    terminal: PtyTerminal,
    _root: TempDir,
    home: PathBuf,
}

impl PtyOctet {
    fn spawn(columns: u16, rows: u16, locale: &str, offline: bool, configured: bool) -> Self {
        let root = tempfile::tempdir().expect("setup TUI tempdir");
        let canonical_root = root
            .path()
            .canonicalize()
            .expect("canonical setup TUI root");
        let home = canonical_root.join("home");
        let workspace = canonical_root.join("workspace");
        let sessions = canonical_root.join("sessions");
        fs::create_dir_all(home.join(".octet/credentials")).expect("setup HOME");
        fs::create_dir_all(&workspace).expect("setup workspace");
        fs::create_dir_all(&sessions).expect("setup sessions");
        if configured {
            let registry = serde_json::json!({
                "version": 1,
                "providers": {
                    "configured": {
                        "label": "Configured local",
                        "base_url": "http://127.0.0.1:9/v1/",
                        "auth": {"kind": "none"},
                        "auto_discover": false,
                        "models": [{
                            "api_name": "existing-model",
                            "display_name": "Existing Model"
                        }]
                    }
                }
            });
            let path = home.join(".octet/credentials/custom.json");
            fs::write(&path, serde_json::to_vec(&registry).unwrap()).expect("configured registry");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("configured registry permissions");
            fs::write(
                home.join(".octet/config.toml"),
                "model = \"custom/configured/existing-model\"\n",
            )
            .expect("configured model preference");
        }

        let terminal = PtyTerminal::open(columns, rows);
        let stdin = duplicate_stdio(terminal.slave.as_raw_fd());
        let stdout = duplicate_stdio(terminal.slave.as_raw_fd());
        let stderr = duplicate_stdio(terminal.slave.as_raw_fd());
        let controlling_tty = terminal.slave.as_raw_fd();
        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command
            .args([
                "--theme",
                "dark",
                "--color",
                "never",
                "--mouse",
                "auto",
                "--no-context-files",
                "--no-tools",
                "--workspace",
            ])
            .arg(&workspace)
            .arg("--session-dir")
            .arg(&sessions);
        if offline {
            command.arg("--offline");
        }
        command
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &workspace)
            .env("TERM", "xterm-256color")
            .env("LANG", locale)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr);
        // The child needs a controlling terminal for raw input, SIGWINCH, and
        // the same primary-screen path as an interactive user.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(controlling_tty, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn octet setup TUI");
        Self {
            child,
            terminal,
            _root: root,
            home,
        }
    }

    fn screen_text(parser: &vt100::Parser, columns: u16) -> String {
        parser
            .screen()
            .rows(0, columns)
            .map(|row| row.trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wait_for_screen(
        &mut self,
        parser: &mut vt100::Parser,
        consumed: &mut usize,
        columns: u16,
        timeout: Duration,
        predicate: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            self.terminal.read_available();
            if *consumed < self.terminal.output.len() {
                parser.process(&self.terminal.output[*consumed..]);
                *consumed = self.terminal.output.len();
            }
            let screen = Self::screen_text(parser, columns);
            if predicate(&screen) {
                return screen;
            }
            if let Some(status) = self.child.try_wait().expect("poll setup TUI") {
                panic!(
                    "octet exited before setup screen condition ({status}); output: {}",
                    visible_bytes(&self.terminal.output)
                );
            }
            if Instant::now() >= deadline {
                panic!(
                    "setup screen condition timed out; output: {}\nscreen:\n{screen}",
                    visible_bytes(&self.terminal.output)
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_screen_after_output(
        &mut self,
        parser: &mut vt100::Parser,
        consumed: &mut usize,
        columns: u16,
        timeout: Duration,
        output_start: usize,
        predicate: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            self.terminal.read_available();
            if *consumed < self.terminal.output.len() {
                parser.process(&self.terminal.output[*consumed..]);
                *consumed = self.terminal.output.len();
            }
            let screen = Self::screen_text(parser, columns);
            if self.terminal.output.len() > output_start && predicate(&screen) {
                return screen;
            }
            if let Some(status) = self.child.try_wait().expect("poll setup TUI") {
                panic!(
                    "octet exited before updated setup screen condition ({status}); output: {}",
                    visible_bytes(&self.terminal.output)
                );
            }
            if Instant::now() >= deadline {
                panic!(
                    "updated setup screen condition timed out; output: {}\nscreen:\n{screen}",
                    visible_bytes(&self.terminal.output)
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_text(
        &mut self,
        parser: &mut vt100::Parser,
        consumed: &mut usize,
        columns: u16,
        text: &str,
    ) -> String {
        self.wait_for_screen(parser, consumed, columns, WAIT, |screen| {
            screen.contains(text)
        })
    }

    fn assert_screen_shape(&self, parser: &vt100::Parser, columns: u16, ascii: bool) {
        let screen = Self::screen_text(parser, columns);
        for row in screen.lines() {
            assert!(
                row.width() <= usize::from(columns),
                "row exceeds {columns} columns: {row:?}"
            );
        }
        if ascii {
            assert!(
                screen.is_ascii(),
                "ASCII fixture leaked non-ASCII text: {screen:?}"
            );
        }
    }

    fn shutdown(&mut self) -> ShutdownCapture {
        self.terminal.write_input(&[4]); // Ctrl-D
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        let status = loop {
            self.terminal.read_available();
            if let Some(status) = self.child.try_wait().expect("poll setup shutdown") {
                break status;
            }
            if Instant::now() >= deadline {
                terminate_child(&mut self.child);
                panic!(
                    "octet did not stop after Ctrl-D; output: {}",
                    visible_bytes(&self.terminal.output)
                );
            }
            thread::sleep(Duration::from_millis(5));
        };
        self.terminal.drain(DRAIN);
        let termios_restored = terminal_modes_equal(
            &self.terminal.original_termios,
            &terminal_attributes(self.terminal.master.as_raw_fd()),
        );
        ShutdownCapture {
            output: self.terminal.output.clone(),
            status,
            termios_restored,
        }
    }

    fn credentials_path(&self) -> PathBuf {
        self.home.join(".octet/credentials/custom.json")
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".octet/config.toml")
    }
}

fn terminate_child(child: &mut Child) {
    let pid = child.id() as libc::pid_t;
    let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
    if result == -1 {
        let _ = child.kill();
    }
    let _ = child.wait();
}

impl Drop for PtyOctet {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            terminate_child(&mut self.child);
        }
    }
}

struct ShutdownCapture {
    output: Vec<u8>,
    status: ExitStatus,
    termios_restored: bool,
}

fn duplicate_stdio(fd: RawFd) -> Stdio {
    let duplicated = unsafe { libc::dup(fd) };
    assert!(
        duplicated >= 0,
        "dup PTY slave failed: {}",
        io::Error::last_os_error()
    );
    Stdio::from(unsafe { File::from_raw_fd(duplicated) })
}

fn set_close_on_exec(fd: RawFd) {
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    assert_eq!(
        result,
        0,
        "fcntl(FD_CLOEXEC): {}",
        io::Error::last_os_error()
    );
}

fn set_nonblocking(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0, "fcntl(F_GETFL): {}", io::Error::last_os_error());
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_eq!(
        result,
        0,
        "fcntl(O_NONBLOCK): {}",
        io::Error::last_os_error()
    );
}

fn terminal_attributes(fd: RawFd) -> libc::termios {
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    let result = unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) };
    assert_eq!(result, 0, "tcgetattr: {}", io::Error::last_os_error());
    unsafe { attributes.assume_init() }
}

fn terminal_modes_equal(before: &libc::termios, after: &libc::termios) -> bool {
    before.c_iflag == after.c_iflag
        && before.c_oflag == after.c_oflag
        && before.c_cflag == after.c_cflag
        && before.c_lflag == after.c_lflag
        && before.c_cc == after.c_cc
}

fn visible_bytes(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 8 * 1024;
    let shown = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BYTES)]);
    let escaped = shown.escape_default().to_string();
    if bytes.len() > MAX_BYTES {
        format!("{escaped}… ({} bytes total)", bytes.len())
    } else {
        escaped
    }
}

fn frame_count(bytes: &[u8], marker: &[u8]) -> usize {
    bytes
        .windows(marker.len())
        .filter(|window| *window == marker)
        .count()
}

fn has_styling_sgr(bytes: &[u8]) -> bool {
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == 0x1b && bytes[index + 1] == b'[' {
            let parameter_start = index + 2;
            let mut cursor = parameter_start;
            while cursor < bytes.len() && cursor - index <= 32 {
                if bytes[cursor] == b'm' {
                    let parameters = &bytes[parameter_start..cursor];
                    if parameters != b"" && parameters != b"0" {
                        return true;
                    }
                    break;
                }
                if bytes[cursor].is_ascii_alphabetic() {
                    break;
                }
                cursor += 1;
            }
        }
        index += 1;
    }
    false
}

fn assert_no_provider_state(octet: &PtyOctet) {
    assert!(
        !octet.credentials_path().exists(),
        "setup cancellation created a custom registry"
    );
    if let Ok(config) = fs::read_to_string(octet.config_path()) {
        assert!(
            !config.contains("custom/"),
            "setup selection leaked into config: {config}"
        );
    }
}

fn choose_openai_endpoint(
    octet: &mut PtyOctet,
    parser: &mut vt100::Parser,
    consumed: &mut usize,
    columns: u16,
    endpoint: &str,
) {
    octet.terminal.write_input(b"\x1b[B\r");
    octet.wait_for_text(parser, consumed, columns, "Endpoint URL:");
    let mut input = endpoint.as_bytes().to_vec();
    input.push(b'\r');
    octet.terminal.write_input(&input);
    octet.wait_for_text(parser, consumed, columns, "Credential source");
}

fn choose_manual_model(
    octet: &mut PtyOctet,
    parser: &mut vt100::Parser,
    consumed: &mut usize,
    columns: u16,
    model: &str,
) {
    // No-auth is the first row. In online mode the inventory picker then puts
    // manual entry on its second row; offline mode skips that picker.
    octet.terminal.write_input(b"\r");
    let screen = octet.wait_for_screen(parser, consumed, columns, WAIT, |screen| {
        screen.contains("Model inventory") || screen.contains("Model ID:")
    });
    if screen.contains("Model inventory") {
        octet.terminal.write_input(b"\x1b[B\r");
        octet.wait_for_text(parser, consumed, columns, "Model ID:");
    }
    let mut input = model.as_bytes().to_vec();
    input.push(b'\r');
    octet.terminal.write_input(&input);
}

#[test]
fn setup_discovery_reaches_normal_prompt_and_records_secret_free_receipt() {
    let _guard = test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let columns = REVIEW_COLUMNS;
    let server = SetupServer::start(ServerReply::Models);
    let mut octet = PtyOctet::spawn(columns, ROWS, "C.UTF-8", false, false);
    let mut parser = vt100::Parser::new(ROWS, columns, 1024);
    let mut consumed = 0;

    let first = octet.wait_for_text(&mut parser, &mut consumed, columns, "Set up a provider");
    assert!(first.contains("LM Studio"));
    assert!(first.contains("OpenAI-compatible endpoint"));
    octet.assert_screen_shape(&parser, columns, false);

    choose_openai_endpoint(
        &mut octet,
        &mut parser,
        &mut consumed,
        columns,
        &server.url(),
    );
    octet.terminal.write_input(b"\r");
    octet.wait_for_text(&mut parser, &mut consumed, columns, "Model inventory");
    octet.terminal.write_input(b"\r");
    octet.wait_for_text(&mut parser, &mut consumed, columns, "Fixture Model");
    assert_eq!(
        server.requests(),
        1,
        "setup should probe only the selected endpoint"
    );

    octet.terminal.write_input(b"\r");
    let review = octet.wait_for_screen(&mut parser, &mut consumed, columns, WAIT, |screen| {
        screen.contains("Review provider setup") && screen.contains("Confirm and save")
    });
    for expected in [
        "Provider setup ready",
        "custom/custom/fixture-model",
        "workspace trust:",
        "tool authority:",
        "OS isolation: none",
    ] {
        assert!(
            review.contains(expected),
            "receipt omitted {expected:?}: {review}"
        );
    }
    assert!(
        !octet
            .terminal
            .output
            .windows(SECRET.len())
            .any(|window| window == SECRET.as_bytes()),
        "a setup secret appeared in the TUI transcript"
    );

    // Confirm the reviewed transaction and wait for both the display label and
    // the ordinary composer. Canonical identity is checked in the saved config
    // below; the startup surface intentionally renders the model's display name.
    octet.terminal.write_input(b"\r");
    let ready = octet.wait_for_screen(&mut parser, &mut consumed, columns, WAIT, |screen| {
        screen.contains("Fixture Model")
            && screen.lines().any(|line| line.trim() == "›")
            && !screen.contains("Review provider setup")
    });
    assert!(ready.contains("Fixture Model"));
    let capture = octet.shutdown();
    assert!(capture.status.success(), "Ctrl-D exit: {}", capture.status);
    assert!(capture.termios_restored);
    assert!(!capture
        .output
        .windows(SECRET.len())
        .any(|window| window == SECRET.as_bytes()));
    assert_eq!(
        frame_count(&capture.output, FRAME_BEGIN),
        frame_count(&capture.output, FRAME_END)
    );
    let alternate_screen = b"\x1b[?1049h";
    assert!(!capture
        .output
        .windows(alternate_screen.len())
        .any(|window| window == alternate_screen));

    let registry = fs::read_to_string(octet.credentials_path()).expect("saved custom registry");
    assert!(registry.contains("fixture-model"));
    assert!(!registry.contains(SECRET));
    let config = fs::read_to_string(octet.config_path()).expect("saved model preference");
    assert!(config.contains("custom/custom/fixture-model"));
}

#[test]
fn setup_auth_failure_supports_retry_edit_back_manual_review_and_cancel() {
    let _guard = test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = SetupServer::start(ServerReply::Unauthorized);
    let mut octet = PtyOctet::spawn(COLUMNS, ROWS, "C.UTF-8", false, false);
    let mut parser = vt100::Parser::new(ROWS, COLUMNS, 1024);
    let mut consumed = 0;
    octet.wait_for_text(&mut parser, &mut consumed, COLUMNS, "Set up a provider");

    choose_openai_endpoint(
        &mut octet,
        &mut parser,
        &mut consumed,
        COLUMNS,
        &server.url(),
    );
    // Enter the bounded secret surface. It shows only its prompt, never the
    // bytes typed here, before returning to the credential picker.
    octet.terminal.write_input(b"\x1b[B\r");
    octet.wait_for_text(&mut parser, &mut consumed, COLUMNS, "API key:");
    let mut secret = SECRET.as_bytes().to_vec();
    secret.push(b'\r');
    octet.terminal.write_input(&secret);
    octet.wait_for_text(&mut parser, &mut consumed, COLUMNS, "Model inventory");
    octet.terminal.write_input(b"\r");
    let failure = octet.wait_for_text(
        &mut parser,
        &mut consumed,
        COLUMNS,
        "Model discovery needs attention",
    );
    assert!(failure.contains("authentication"));
    assert!(!failure.contains(SECRET));

    // The first recovery row retries the same endpoint. Wait for a fresh
    // recovery render as well as the second request so the following key
    // sequence cannot race the retry lifecycle.
    let retry_output_start = octet.terminal.output.len();
    octet.terminal.write_input(b"\r");
    octet.wait_for_screen_after_output(
        &mut parser,
        &mut consumed,
        COLUMNS,
        WAIT,
        retry_output_start,
        |screen| server.requests() >= 2 && screen.contains("Model discovery needs attention"),
    );
    assert!(
        server.requests() >= 2,
        "retry did not contact the selected endpoint"
    );
    // Select Edit endpoint after the failed retry, then exercise the endpoint
    // input's Escape/back path before entering it again.
    octet.terminal.write_input(b"\x1b[B\x1b[B\r");
    octet.wait_for_text(&mut parser, &mut consumed, COLUMNS, "Set up a provider");
    choose_openai_endpoint(
        &mut octet,
        &mut parser,
        &mut consumed,
        COLUMNS,
        &server.url(),
    );
    octet.terminal.write_input(b"\x1b");
    octet.wait_for_text(&mut parser, &mut consumed, COLUMNS, "Set up a provider");

    // Manual/offline recovery is available from the same setup transaction
    // without another endpoint request. This run selects manual inventory,
    // then edits the review back to the access-path screen.
    choose_openai_endpoint(
        &mut octet,
        &mut parser,
        &mut consumed,
        COLUMNS,
        &server.url(),
    );
    choose_manual_model(
        &mut octet,
        &mut parser,
        &mut consumed,
        COLUMNS,
        "manual-recovery-model",
    );
    let review = octet.wait_for_screen(&mut parser, &mut consumed, COLUMNS, WAIT, |screen| {
        screen.contains("Review provider setup") && screen.contains("Confirm and save")
    });
    assert!(review.contains("Provider setup ready"));
    assert!(!review.contains(SECRET));
    octet.terminal.write_input(b"\x1b[B\r");
    octet.wait_for_text(&mut parser, &mut consumed, COLUMNS, "Set up a provider");

    // Continue without a provider is an explicit cancellation screen/action.
    octet.terminal.write_input(b"\x1b[B\x1b[B\r");
    let modeless = octet.wait_for_text(&mut parser, &mut consumed, COLUMNS, "No configured model");
    assert!(!modeless.contains("Review provider setup"));
    assert_no_provider_state(&octet);
    assert!(!octet
        .terminal
        .output
        .windows(SECRET.len())
        .any(|window| window == SECRET.as_bytes()));

    let capture = octet.shutdown();
    assert!(capture.status.success(), "Ctrl-D exit: {}", capture.status);
    assert!(capture.termios_restored);
    assert!(!capture
        .output
        .windows(SECRET.len())
        .any(|window| window == SECRET.as_bytes()));
}

#[test]
fn setup_manual_narrow_ascii_no_color_and_configured_startup_remain_bounded() {
    let _guard = test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // `LANG=C` selects the product's ASCII capability while --color never
    // removes ANSI styling. This uses the offline manual recovery path so no
    // endpoint is contacted.
    let mut narrow = PtyOctet::spawn(NARROW_COLUMNS, NARROW_ROWS, "C", true, false);
    let mut narrow_parser = vt100::Parser::new(NARROW_ROWS, NARROW_COLUMNS, 1024);
    let mut narrow_consumed = 0;
    let first = narrow.wait_for_text(
        &mut narrow_parser,
        &mut narrow_consumed,
        NARROW_COLUMNS,
        "Set up a provider",
    );
    assert!(first.is_ascii());
    narrow.assert_screen_shape(&narrow_parser, NARROW_COLUMNS, true);
    assert!(
        !has_styling_sgr(&narrow.terminal.output),
        "no-color setup frame emitted styling SGR"
    );
    choose_openai_endpoint(
        &mut narrow,
        &mut narrow_parser,
        &mut narrow_consumed,
        NARROW_COLUMNS,
        "http://127.0.0.1:9/v1/",
    );
    choose_manual_model(
        &mut narrow,
        &mut narrow_parser,
        &mut narrow_consumed,
        NARROW_COLUMNS,
        "narrow-manual-model",
    );
    let review = narrow.wait_for_text(
        &mut narrow_parser,
        &mut narrow_consumed,
        NARROW_COLUMNS,
        "Review provider setup",
    );
    assert!(review.is_ascii());
    narrow.assert_screen_shape(&narrow_parser, NARROW_COLUMNS, true);
    assert!(
        !has_styling_sgr(&narrow.terminal.output),
        "no-color review frame emitted styling SGR"
    );
    // Escape cancels the review without saving the manual inventory.
    narrow.terminal.write_input(b"\x1b");
    narrow.wait_for_text(
        &mut narrow_parser,
        &mut narrow_consumed,
        NARROW_COLUMNS,
        "No configured model",
    );
    assert_no_provider_state(&narrow);
    let capture = narrow.shutdown();
    assert!(capture.status.success());
    assert!(capture.termios_restored);

    // A configured provider/catalog follows the normal startup path and does
    // not offer onboarding, even in the same no-color ASCII terminal profile.
    let mut configured = PtyOctet::spawn(COLUMNS, ROWS, "C", true, true);
    let mut configured_parser = vt100::Parser::new(ROWS, COLUMNS, 1024);
    let mut configured_consumed = 0;
    let configured_screen = configured.wait_for_text(
        &mut configured_parser,
        &mut configured_consumed,
        COLUMNS,
        "Existing Model",
    );
    assert!(configured_screen.contains("Existing Model"));
    assert!(!configured_screen.contains("Set up a provider"));
    assert!(!configured_screen.contains("No configured model"));
    assert!(configured_screen.is_ascii());
    configured.assert_screen_shape(&configured_parser, COLUMNS, true);
    let capture = configured.shutdown();
    assert!(capture.status.success());
    assert!(capture.termios_restored);
    assert!(
        !has_styling_sgr(&capture.output),
        "configured no-color startup emitted styling SGR"
    );
}
