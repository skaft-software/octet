#![cfg(unix)]

//! Real-binary PTY qualification for activity rows while an API response is held.
//!
//! The loopback server sends HTTP headers and then waits before sending its
//! finite SSE body. It emits no provider-token events while the row is sampled;
//! all HOME, workspace, session and provider state is disposable.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::ops::Range;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const INITIAL_COLUMNS: u16 = 96;
const INITIAL_ROWS: u16 = 18;
const RESIZED_COLUMNS: u16 = 64;
const RESIZED_ROWS: u16 = 12;
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const FRAME_END: &[u8] = b"\x1b[?2026l";
const SSE_BODY: &[u8] = concat!(
    "data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"fixture response done\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
)
.as_bytes();

fn trace_fixture(stage: &str) {
    // Bypass libtest's per-test capture so a stuck syscall leaves a CI breadcrumb.
    if std::env::var_os("OCTET_PTY_TRACE").is_some() {
        let _ = writeln!(io::stderr(), "activity-wait-pty: {stage}");
    }
}

fn pty_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn set_close_on_exec(fd: i32) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0, "F_GETFD: {}", io::Error::last_os_error());
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) },
        0,
        "F_SETFD: {}",
        io::Error::last_os_error()
    );
}

fn set_nonblocking(fd: i32) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0, "F_GETFL: {}", io::Error::last_os_error());
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0,
        "F_SETFL: {}",
        io::Error::last_os_error()
    );
}

fn terminal_attributes(fd: i32) -> libc::termios {
    let mut value = MaybeUninit::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(fd, value.as_mut_ptr()) },
        0,
        "tcgetattr: {}",
        io::Error::last_os_error()
    );
    unsafe { value.assume_init() }
}

fn duplicate_stdio(fd: i32) -> File {
    let duplicate = unsafe { libc::dup(fd) };
    assert!(
        duplicate >= 0,
        "dup PTY slave: {}",
        io::Error::last_os_error()
    );
    unsafe { File::from_raw_fd(duplicate) }
}

struct Pty {
    master: Option<File>,
    slave: Option<File>,
    original_termios: libc::termios,
    output: Vec<u8>,
}

impl Pty {
    fn open(columns: u16, rows: u16) -> Self {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
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
        set_close_on_exec(master_fd);
        set_close_on_exec(slave_fd);
        set_nonblocking(master_fd);
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        let original_termios = terminal_attributes(slave.as_raw_fd());
        Self {
            master: Some(master),
            slave: Some(slave),
            original_termios,
            output: Vec::new(),
        }
    }

    fn write_input(&mut self, input: &[u8]) {
        let master = self.master.as_mut().expect("open PTY master");
        master.write_all(input).expect("write PTY input");
        master.flush().expect("flush PTY input");
    }

    fn resize(&self, columns: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::ioctl(
                    self.slave.as_ref().expect("open PTY slave").as_raw_fd(),
                    libc::TIOCSWINSZ,
                    &size as *const libc::winsize,
                )
            },
            0,
            "TIOCSWINSZ: {}",
            io::Error::last_os_error()
        );
    }

    fn read_available(&mut self) {
        let mut buffer = [0u8; 8192];
        loop {
            match self
                .master
                .as_mut()
                .expect("open PTY master")
                .read(&mut buffer)
            {
                Ok(0) => return,
                Ok(read) => {
                    self.output.extend_from_slice(&buffer[..read]);
                    assert!(self.output.len() < 4 * 1024 * 1024, "unbounded PTY output");
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
    }

    fn close(&mut self) {
        drop(self.master.take());
        drop(self.slave.take());
    }

    fn drain_for(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.read_available();
            thread::sleep(Duration::from_millis(2));
        }
        self.read_available();
    }
}

struct Candidate {
    child: Child,
    pty: Pty,
    _root: TempDir,
}

impl Candidate {
    fn spawn(api_url: &str, theme: &str, color: &str) -> Self {
        let root = tempfile::tempdir().expect("PTY fixture tempdir");
        let canonical = root.path().canonicalize().expect("canonical fixture root");
        let home = canonical.join("home");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        fs::create_dir_all(home.join(".octet/credentials")).expect("credential directory");
        fs::create_dir_all(&workspace).expect("workspace directory");
        fs::create_dir_all(&sessions).expect("session directory");
        let credential = home.join(".octet/credentials/custom.json");
        let record = serde_json::json!({
            "base_url": api_url,
            "api_key": "",
            "api_name": "probe",
            "headers": [],
            "models": [],
            "auto_discover": false,
        });
        fs::write(&credential, record.to_string()).expect("loopback credential fixture");
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600))
            .expect("credential permissions");

        let pty = Pty::open(INITIAL_COLUMNS, INITIAL_ROWS);
        let tty_fd = pty.slave.as_ref().expect("open PTY slave").as_raw_fd();
        let stdin = duplicate_stdio(tty_fd);
        let stdout = duplicate_stdio(tty_fd);
        let stderr = duplicate_stdio(tty_fd);
        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command
            .args([
                "--offline",
                "--no-context-files",
                "--no-tools",
                "--color",
                color,
                "--theme",
                theme,
                "--workspace",
            ])
            .arg(&workspace)
            .arg("--session-dir")
            .arg(&sessions)
            .args(["--model", "custom/probe"])
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &workspace)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LANG", "C.UTF-8")
            .env("OCTET_COLOR_SCHEME", theme)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1
                    || libc::ioctl(tty_fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        trace_fixture("spawning PTY child");
        let child = command.spawn().expect("spawn octet under PTY");
        trace_fixture("PTY child spawned");
        Self {
            child,
            pty,
            _root: root,
        }
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.pty.resize(columns, rows);
        assert_eq!(
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGWINCH) },
            0,
            "SIGWINCH: {}",
            io::Error::last_os_error()
        );
    }

    fn wait_for_output(&mut self, timeout: Duration, predicate: impl Fn(&[u8]) -> bool) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.pty.read_available();
            if predicate(&self.pty.output) {
                return;
            }
            assert!(
                self.child.try_wait().expect("poll octet").is_none(),
                "candidate exited: {}",
                visible_bytes(&self.pty.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "PTY output condition timed out: {}",
            visible_bytes(&self.pty.output)
        );
    }

    fn shutdown(mut self) {
        trace_fixture("shutting down PTY child");
        self.pty.write_input(&[4]);
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        let status = loop {
            self.pty.read_available();
            if let Some(status) = self.child.try_wait().expect("poll shutdown") {
                break status;
            }
            assert!(Instant::now() < deadline, "candidate did not shut down");
            thread::sleep(Duration::from_millis(5));
        };
        assert!(status.success(), "candidate shutdown status: {status}");
        // A controlling-terminal session exit can revoke the parent-held slave
        // on macOS. The retained master exposes the same terminal mode state
        // after the child exits on both macOS and Linux.
        trace_fixture("PTY child exited; checking restored terminal");
        let restored = terminal_attributes(
            self.pty
                .master
                .as_ref()
                .expect("open PTY master")
                .as_raw_fd(),
        );
        assert_eq!(
            restored.c_lflag & (libc::ICANON | libc::ECHO),
            self.pty.original_termios.c_lflag & (libc::ICANON | libc::ECHO),
            "PTY line discipline was not restored"
        );
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        if thread::panicking() {
            let start = self.pty.output.len().saturating_sub(16 * 1024);
            let _ = writeln!(
                io::stderr(),
                "activity-wait-pty failure tail: {}",
                visible_bytes(&self.pty.output[start..])
            );
        }
        // Do not keep parent-held PTY ends live while reaping a killed child.
        // This cleanup wait hid the original assertion indefinitely on macOS;
        // keep it bounded even when the child cannot promptly be reaped.
        self.pty.close();
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        trace_fixture("killing and reaping PTY child");
        let _ = self.child.kill();
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                trace_fixture("PTY child reaped");
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let _ = writeln!(
            io::stderr(),
            "activity-wait-pty: killed child did not reap within {SHUTDOWN_TIMEOUT:?}"
        );
        assert!(thread::panicking(), "PTY child cleanup timed out");
    }
}

struct HeldApi {
    url: String,
    arrived: mpsc::Receiver<usize>,
    release: mpsc::Sender<()>,
    count: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HeldApi {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        listener
            .set_nonblocking(true)
            .expect("nonblocking loopback listener");
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let (arrived_tx, arrived) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_count = count.clone();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let worker_bodies = bodies.clone();
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
                let Some((headers, body)) = read_request(&mut socket, &worker_stop) else {
                    continue;
                };
                assert!(
                    headers.starts_with("POST /v1/chat/completions HTTP/1.1"),
                    "unexpected fixture request: {headers}"
                );
                assert!(
                    !headers.to_ascii_lowercase().contains("authorization:"),
                    "fixture must not receive credentials"
                );
                let response_headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    SSE_BODY.len()
                );
                if socket.write_all(response_headers.as_bytes()).is_err() {
                    continue;
                }
                worker_bodies.lock().unwrap().push(body);
                let index = worker_count.fetch_add(1, Ordering::SeqCst) + 1;
                if arrived_tx.send(index).is_err() {
                    return;
                }
                loop {
                    if worker_stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match released.recv_timeout(Duration::from_millis(50)) {
                        Ok(()) => {
                            let _ = socket.write_all(SSE_BODY);
                            let _ = socket.flush();
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        });
        Self {
            url,
            arrived,
            release,
            count,
            bodies,
            stop,
            worker: Some(worker),
        }
    }

    fn wait_for_request(&self, candidate: &mut Candidate, expected: usize) {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            candidate.pty.read_available();
            match self.arrived.try_recv() {
                Ok(actual) => {
                    assert_eq!(actual, expected);
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(error) => panic!("loopback fixture closed: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "request {expected} did not arrive: {}",
                visible_bytes(&candidate.pty.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn release_response(&self) {
        self.release.send(()).expect("release held response");
    }
}

impl Drop for HeldApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.release.send(());
        if let Some(worker) = self.worker.take() {
            trace_fixture("joining loopback fixture");
            if let Err(panic) = worker.join() {
                if !thread::panicking() {
                    std::panic::resume_unwind(panic);
                }
            }
            trace_fixture("loopback fixture joined");
        }
    }
}

fn read_request(socket: &mut std::net::TcpStream, stop: &AtomicBool) -> Option<(String, Vec<u8>)> {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let mut request = Vec::new();
    let header_end = loop {
        if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return None;
        }
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
        if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return None;
        }
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
            Err(error) => panic!("read loopback body: {error}"),
        };
        request.extend_from_slice(&bytes[..read]);
        assert!(request.len() <= 128 * 1024, "unbounded fixture body");
    }
    Some((headers, request[header_end..header_end + length].to_vec()))
}

#[test]
fn fixture_shutdown_cancels_partial_requests() {
    use std::net::{TcpListener, TcpStream};

    for partial in [
        b"POST /v1/chat/completions HTTP/1.1\r\n".as_slice(),
        b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 100\r\n\r\n{",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        client.write_all(partial).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (started_tx, started) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let request = read_request(&mut socket, &worker_stop);
            let _ = done_tx.send(request.is_none());
        });
        started.recv_timeout(WAIT_TIMEOUT).unwrap();
        stop.store(true, Ordering::SeqCst);
        let result = done.recv_timeout(Duration::from_secs(1));
        // Always release the connection and join, including on the red path.
        drop(client);
        let joined = worker.join();
        assert_eq!(
            result.ok(),
            Some(true),
            "partial request ignored fixture shutdown"
        );
        joined.unwrap();
    }
}

fn await_screen(
    candidate: &mut Candidate,
    parser: &mut vt100::Parser,
    consumed: &mut usize,
    text: &str,
) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        candidate.pty.read_available();
        parser.process(&candidate.pty.output[*consumed..]);
        *consumed = candidate.pty.output.len();
        if parser.screen().contents().contains(text) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "screen did not show {text:?}: {}",
            parser.screen().contents()
        );
        assert!(
            candidate.child.try_wait().unwrap().is_none(),
            "candidate exited"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn frame_ranges(bytes: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = bytes[cursor..]
        .windows(FRAME_END.len())
        .position(|window| window == FRAME_END)
    {
        let end = cursor + offset + FRAME_END.len();
        ranges.push(cursor..end);
        cursor = end;
    }
    ranges
}

fn resize_frame_end(bytes: &[u8]) -> Option<usize> {
    // A previous frame may finish before the resize repaint has fully arrived.
    // Require the clear and its following frame end, not independent markers.
    frame_ranges(bytes)
        .into_iter()
        .find(|range| {
            bytes[range.clone()]
                .windows(4)
                .any(|window| window == b"\x1b[2J")
        })
        .map(|range| range.end)
}

#[test]
fn resize_wait_rejects_a_previous_frame_end() {
    let bytes =
        b"\x1b[?2026hprevious frame\x1b[?2026l\x1b[?2026h\x1b[2Jdraft remains local\x1b[?2026l";
    for end in 0..bytes.len() {
        assert!(
            resize_frame_end(&bytes[..end]).is_none(),
            "accepted incomplete resize frame at byte {end}"
        );
    }
    assert_eq!(resize_frame_end(bytes), Some(bytes.len()));
    let mut with_partial_frame = bytes.to_vec();
    with_partial_frame.extend_from_slice(b"\x1b[?2026h\x1b[2J");
    assert_eq!(resize_frame_end(&with_partial_frame), Some(bytes.len()));
}

fn status_colors(parser: &vt100::Parser, label: &str, columns: u16) -> Option<Vec<vt100::Color>> {
    parser
        .screen()
        .rows(0, columns)
        .enumerate()
        .find_map(|(row, text)| {
            let index = text.find(label)?;
            let column = text[..index].chars().count();
            Some(
                (column..column + label.chars().count())
                    .map(|column| {
                        parser
                            .screen()
                            .cell(row as u16, column as u16)
                            .unwrap()
                            .fgcolor()
                    })
                    .collect(),
            )
        })
}

fn visible_bytes(bytes: &[u8]) -> String {
    sexy_tui_rs::strip_terminal_sequences(&String::from_utf8_lossy(bytes))
}

fn run_activity_case(theme: &str, compact: bool, color: &str) {
    const SAMPLE: Duration = Duration::from_millis(640);
    let api = HeldApi::start();
    let mut candidate = Candidate::spawn(&api.url, theme, color);
    let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 512);
    let mut consumed = 0;
    await_screen(&mut candidate, &mut parser, &mut consumed, "custom/probe");

    candidate.pty.write_input(b"fixture initial prompt\r");
    api.wait_for_request(&mut candidate, 1);
    let (label, request_count) = if compact {
        api.release_response();
        await_screen(&mut candidate, &mut parser, &mut consumed, "completed");
        // Public text precedes authoritative settlement. Wait for the terminal
        // outcome before requesting idle-only compaction, rather than sending
        // into the active run's after-response lifecycle owner.
        // A trailing space makes this an unambiguous no-argument command.
        candidate.pty.write_input(b"/compact \r");
        api.wait_for_request(&mut candidate, 2);
        ("Compacting context", 2)
    } else {
        ("Working", 1)
    };
    await_screen(&mut candidate, &mut parser, &mut consumed, label);

    let sample_start = candidate.pty.output.len();
    candidate.pty.drain_for(SAMPLE);
    let frames = frame_ranges(&candidate.pty.output[sample_start..]);
    assert!(
        frames.len() <= 12,
        "{theme}/{label} redraw is not bounded to the 80ms schedule: {} frames",
        frames.len()
    );
    if color == "never" || compact {
        assert!(
            frames.len() <= 2,
            "non-shimmering activity only refreshes its timer"
        );
    } else {
        assert!(
            frames.len() >= 3,
            "{theme}/{label} did not redraw while held"
        );
        let mut palettes = Vec::new();
        for frame in &frames {
            let end = sample_start + frame.end;
            parser.process(&candidate.pty.output[consumed..end]);
            consumed = end;
            if let Some(colors) = status_colors(&parser, label, INITIAL_COLUMNS) {
                if !palettes.contains(&colors) {
                    palettes.push(colors);
                }
            }
        }
        assert!(
            palettes.len() >= 2,
            "{theme}/{label} style did not advance without provider tokens"
        );
    }

    candidate.pty.write_input(b"draft remains local");
    await_screen(
        &mut candidate,
        &mut parser,
        &mut consumed,
        "draft remains local",
    );

    let resize_start = candidate.pty.output.len();
    candidate.resize(RESIZED_COLUMNS, RESIZED_ROWS);
    candidate.wait_for_output(WAIT_TIMEOUT, |bytes| {
        resize_frame_end(&bytes[resize_start..]).is_some()
    });
    let resize_end =
        resize_start + resize_frame_end(&candidate.pty.output[resize_start..]).unwrap();
    parser.set_size(RESIZED_ROWS, RESIZED_COLUMNS);
    parser.process(&candidate.pty.output[consumed..resize_end]);
    consumed = resize_end;
    assert!(
        parser.screen().contents().contains("draft remains local"),
        "resize lost local input: {}",
        parser.screen().contents()
    );

    candidate.pty.write_input(b"\x1b");
    let cancellation = if compact {
        "compaction cancelled"
    } else {
        "interrupted"
    };
    await_screen(&mut candidate, &mut parser, &mut consumed, cancellation);
    api.release_response();
    candidate.pty.drain_for(Duration::from_millis(250));
    parser.process(&candidate.pty.output[consumed..]);
    assert!(
        !parser.screen().contents().contains(label),
        "stale activity row"
    );
    assert_eq!(
        api.count.load(Ordering::SeqCst),
        request_count,
        "held {label} issued a duplicate request"
    );

    candidate.shutdown();
    eprintln!(
        "activity-wait-pty theme={theme} label={label} color={color}: frames={} request_count={request_count} input_budget_ms=500 resize=true cancellation=true",
        frames.len()
    );
}

#[test]
fn failed_pty_fixture_releases_terminal_and_reaps_child() {
    let _guard = pty_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let candidate = Candidate::spawn("http://127.0.0.1:9/v1/", "dark", "never");
    let pid = candidate.child.id() as libc::pid_t;
    let failed = std::panic::catch_unwind(move || {
        let _candidate = candidate;
        panic!("intentional fixture failure");
    });
    assert!(failed.is_err());
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn real_activity_wait_pty_contract() {
    let _guard = pty_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Ordinary model waits are qualified on both appearance profiles; the
    // compaction path is independent, and no-color confirms static fallback.
    run_activity_case("dark", false, "always");
    run_activity_case("light", false, "always");
    run_activity_case("dark", true, "always");
    run_activity_case("dark", false, "never");
}

#[test]
fn real_queued_input_escape_dispatch_and_option_up_edit_pty_contract() {
    let _guard = pty_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let api = HeldApi::start();
    let mut candidate = Candidate::spawn(&api.url, "dark", "never");
    let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 512);
    let mut consumed = 0;
    await_screen(&mut candidate, &mut parser, &mut consumed, "custom/probe");
    candidate.pty.write_input(b"queue fixture initial\r");
    api.wait_for_request(&mut candidate, 1);
    candidate.pty.write_input(b"QUEUE-FIRST\rQUEUE-ORIGINAL\r");
    await_screen(&mut candidate, &mut parser, &mut consumed, "2 queued");
    candidate.pty.write_input(b"\x1b[1;3A"); // Option/Alt+Up, not an editor arrow.
    await_screen(&mut candidate, &mut parser, &mut consumed, "QUEUE-ORIGINAL");
    candidate
        .pty
        .write_input(b"\x03QUEUE-EDITED\rDRAFT-NEVER-SUBMIT");
    await_screen(
        &mut candidate,
        &mut parser,
        &mut consumed,
        "DRAFT-NEVER-SUBMIT",
    );
    assert_eq!(
        api.count.load(Ordering::SeqCst),
        1,
        "queue editing must not send"
    );
    candidate.pty.write_input(b"\x1b");
    await_screen(&mut candidate, &mut parser, &mut consumed, "interrupted");
    // The serial fixture must release its cancelled socket before accepting
    // the next one. The real frontend has already settled cancellation.
    api.release_response();
    api.wait_for_request(&mut candidate, 2);
    {
        let bodies = api.bodies.lock().unwrap();
        let request = String::from_utf8_lossy(&bodies[1]);
        assert!(request.contains("QUEUE-FIRST"), "{request}");
        assert!(!request.contains("QUEUE-ORIGINAL"), "{request}");
        assert!(
            !request.contains("QUEUE-EDITED"),
            "FIFO dispatch: {request}"
        );
        assert!(!request.contains("DRAFT-NEVER-SUBMIT"), "{request}");
    }
    api.release_response();
    api.wait_for_request(&mut candidate, 3);
    {
        let bodies = api.bodies.lock().unwrap();
        let request = String::from_utf8_lossy(&bodies[2]);
        assert!(request.contains("QUEUE-EDITED"), "{request}");
        assert!(!request.contains("QUEUE-ORIGINAL"), "{request}");
        assert!(!request.contains("DRAFT-NEVER-SUBMIT"), "{request}");
    }
    await_screen(
        &mut candidate,
        &mut parser,
        &mut consumed,
        "DRAFT-NEVER-SUBMIT",
    );
    candidate.pty.write_input(b"\x03\x03"); // clear draft, then plain cancellation
    candidate.pty.drain_for(Duration::from_millis(250));
    api.release_response();
    candidate.shutdown();
    assert_eq!(api.count.load(Ordering::SeqCst), 3);
}
