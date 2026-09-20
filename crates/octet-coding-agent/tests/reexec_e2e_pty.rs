#![cfg(unix)]

//! Real-binary PTY qualification for `/reload`'s hot re-exec path.
//!
//! Every pane runs a scratch *copy* of the built binary under a scratch
//! `HOME`, workspace, and session directory. To prove that a reload replaces
//! the process image in place, the copy is swapped for a marker wrapper that
//! records its pid, its parent pid, and its argv before `exec`ing a second
//! scratch copy. The assertions therefore show *same process, same session,
//! new image* without touching `target/` or the real `~/.octet`.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const COLUMNS: u16 = 100;
const ROWS: u16 = 20;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const STEP_TIMEOUT: Duration = Duration::from_secs(25);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// A loopback port nothing listens on; panes that never submit a prompt use it.
const UNREACHABLE_PROVIDER: &str = "http://127.0.0.1:9/v1/";
/// One unbroken token so PTY line wrapping can never split the marker.
const FIXTURE_REPLY: &[u8] = b"panebfixturereplymarker";
/// Mirrors `PROBE_FLAG` in `src/reexec.rs`.
const PROBE_FLAG: &str = "--internal-reexec-probe";

/// PTY fixtures contend for terminal resources; run them one at a time.
fn pty_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn built_binary() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_octet"));
    assert!(
        path.is_file(),
        "the built octet binary is missing at {}; build the package before running this test",
        path.display()
    );
    path
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
    assert!(flags >= 0, "F_GETFL: {}", io::Error::last_os_error());
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_eq!(result, 0, "F_SETFL: {}", io::Error::last_os_error());
}

fn duplicate_stdio(fd: RawFd) -> Stdio {
    let duplicated = unsafe { libc::dup(fd) };
    assert!(
        duplicated >= 0,
        "dup PTY slave failed: {}",
        io::Error::last_os_error()
    );
    // SAFETY: dup returned a new owned descriptor.
    Stdio::from(unsafe { File::from_raw_fd(duplicated) })
}

struct Pty {
    master: File,
    slave: File,
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
            master: unsafe { File::from_raw_fd(master_fd) },
            slave: unsafe { File::from_raw_fd(slave_fd) },
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
                Ok(read) => {
                    self.output.extend_from_slice(&buffer[..read]);
                    assert!(self.output.len() < 8 * 1024 * 1024, "unbounded PTY output");
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                // PTY masters commonly report EIO after the last slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
    }
}

/// One disposable pane root: scratch `HOME`, workspace, session store, and
/// binary directory.
struct Fixture {
    _root: TempDir,
    home: PathBuf,
    workspace: PathBuf,
    sessions: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new(provider_url: &str) -> Self {
        let root = tempfile::tempdir().expect("pane fixture root");
        let canonical = root.path().canonicalize().expect("canonical pane root");
        let home = canonical.join("home");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        let bin = canonical.join("bin");
        fs::create_dir_all(home.join(".octet/credentials")).expect("credential directory");
        fs::create_dir_all(&workspace).expect("workspace directory");
        fs::create_dir_all(&sessions).expect("session directory");
        fs::create_dir_all(&bin).expect("binary directory");
        let credential = home.join(".octet/credentials/custom.json");
        let record = serde_json::json!({
            "base_url": provider_url,
            "api_key": "",
            "api_name": "probe",
            "headers": [],
            "models": [],
            "auto_discover": false,
        });
        fs::write(&credential, record.to_string()).expect("custom-provider fixture");
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600))
            .expect("credential permissions");
        Self {
            _root: root,
            home,
            workspace,
            sessions,
            bin,
        }
    }

    /// One executable scratch copy of the built binary.
    fn copy_binary(&self, name: &str) -> PathBuf {
        let destination = self.bin.join(name);
        fs::copy(built_binary(), &destination).expect("copy the built binary");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
            .expect("copy permissions");
        destination
    }

    /// The canonical scratch root every fixture path is derived from.
    fn root(&self) -> &Path {
        self.home.parent().expect("scratch home has a parent")
    }
}

struct Pane {
    child: Child,
    pty: Pty,
    fixture: Fixture,
    launch: PathBuf,
}

impl Pane {
    /// Spawn `launch` as an interactive pane under its own controlling PTY.
    fn spawn(fixture: Fixture, launch: &Path) -> Self {
        let pty = Pty::open();
        let stdin = duplicate_stdio(pty.slave.as_raw_fd());
        let stdout = duplicate_stdio(pty.slave.as_raw_fd());
        let stderr = duplicate_stdio(pty.slave.as_raw_fd());
        let tty_fd = pty.slave.as_raw_fd();
        let mut command = Command::new(launch);
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
            .arg(&fixture.workspace)
            .arg("--session-dir")
            .arg(&fixture.sessions)
            .args(["--model", "custom/probe", "--theme", "dark"])
            .current_dir(&fixture.workspace)
            .env_clear()
            .env("HOME", &fixture.home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &fixture.workspace)
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
            fixture,
            launch: launch.to_path_buf(),
        }
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// The process's current argv as the OS reports it. Linux reads
    /// `/proc/<pid>/cmdline`; every other Unix asks `ps`. `exec` replaces the
    /// argv the kernel exposes, so this observes a re-exec that the pid alone
    /// cannot.
    fn ps_argv(&self) -> String {
        process_argv_line(self.pid())
    }

    /// Poll the pane's PTY and liveness until `ready` holds.
    fn wait_until(&mut self, timeout: Duration, what: &str, mut ready: impl FnMut(&Pane) -> bool) {
        let deadline = Instant::now() + timeout;
        loop {
            self.pty.read_available();
            if ready(self) {
                return;
            }
            if let Some(status) = self.child.try_wait().expect("poll pane") {
                panic!(
                    "pane exited with {status} while waiting for {what}: {}",
                    visible(&self.pty.output)
                );
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what} after {timeout:?}: {}",
                visible(&self.pty.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for(&mut self, needle: &[u8], timeout: Duration) {
        let what = format!(
            "{:?} in the PTY transcript",
            String::from_utf8_lossy(needle)
        );
        self.wait_until(timeout, &what, |pane| {
            contains_bytes(&pane.pty.output, needle)
        });
    }

    /// Wait for `needle` to appear after `start`, the byte offset of a marker
    /// the old image wrote; anything after it belongs to the replacement.
    fn wait_for_suffix(&mut self, start: usize, needle: &[u8], timeout: Duration) {
        let what = format!("{:?} after byte {start}", String::from_utf8_lossy(needle));
        self.wait_until(timeout, &what, |pane| {
            let end = pane.pty.output.len();
            let start = start.min(end);
            contains_bytes(&pane.pty.output[start..], needle)
        });
    }

    /// The single workspace-scoped session store this pane owns.
    fn store_directory(&self) -> Option<PathBuf> {
        let mut directories = fs::read_dir(&self.fixture.sessions)
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        directories.sort();
        (directories.len() == 1).then(|| directories.pop().expect("one directory"))
    }

    fn session_files(&self) -> Vec<PathBuf> {
        let Some(store) = self.store_directory() else {
            return Vec::new();
        };
        let mut files = fs::read_dir(&store)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()) != Some("ephemeral-sessions.jsonl")
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    /// Wait for the pane's boot frame and its single durable session file.
    fn await_ready(&mut self, timeout: Duration) -> PathBuf {
        self.wait_for(b"custom/probe", timeout);
        let sessions = self.fixture.sessions.clone();
        self.wait_until(timeout, "exactly one session transcript", move |_| {
            !transcript_files(&sessions).is_empty()
        });
        let files = self.session_files();
        assert_eq!(
            files.len(),
            1,
            "the ready pane must own exactly one transcript: {files:?}"
        );
        files.into_iter().next().expect("one transcript")
    }

    /// Replace the running image's path with a wrapper that records one
    /// invocation (pid, parent pid, argv) and then `exec`s `image` unchanged.
    fn install_marker_wrapper(&self, image: &Path, marker: &Path) {
        let staging = self.fixture.bin.join("octet.wrapper.tmp");
        fs::write(&staging, wrapper_script(image, marker)).expect("write marker wrapper");
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))
            .expect("wrapper permissions");
        fs::rename(&staging, &self.launch).expect("install marker wrapper");
    }

    fn wait_for_record(
        &mut self,
        marker: &Path,
        timeout: Duration,
        predicate: impl Fn(&MarkerRecord) -> bool,
    ) -> MarkerRecord {
        let deadline = Instant::now() + timeout;
        loop {
            self.pty.read_available();
            if let Some(record) = read_marker(marker)
                .into_iter()
                .find(|record| predicate(record))
            {
                return record;
            }
            if let Some(status) = self.child.try_wait().expect("poll pane") {
                panic!(
                    "pane exited with {status} while waiting for a marker record: {}",
                    visible(&self.pty.output)
                );
            }
            assert!(
                Instant::now() < deadline,
                "no marker record appeared within {timeout:?}: {}",
                visible(&self.pty.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait until the pane's transcript has been byte-stable for `quiet`.
    fn wait_for_session_stable(
        &mut self,
        path: &Path,
        quiet: Duration,
        timeout: Duration,
    ) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut last = fs::read(path).expect("read session transcript");
        let mut stable_since = Instant::now();
        loop {
            self.pty.read_available();
            thread::sleep(Duration::from_millis(20));
            let current = fs::read(path).expect("read session transcript");
            if current.len() != last.len() {
                last = current;
                stable_since = Instant::now();
            } else if stable_since.elapsed() >= quiet {
                return current;
            }
            assert!(
                self.child.try_wait().expect("poll pane").is_none(),
                "pane exited while its transcript settled: {}",
                visible(&self.pty.output)
            );
            assert!(
                Instant::now() < deadline,
                "the session transcript never settled: {}",
                visible(&self.pty.output)
            );
        }
    }

    fn shutdown(mut self) {
        self.pty.write_input(&[4]);
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            self.pty.read_available();
            if let Some(status) = self.child.try_wait().expect("poll shutdown") {
                assert!(
                    status.success(),
                    "pane exited unsuccessfully ({status}): {}",
                    visible(&self.pty.output)
                );
                return;
            }
            if Instant::now() >= deadline {
                unsafe {
                    let _ = libc::kill(self.pid(), libc::SIGKILL);
                }
                let _ = self.child.wait();
                panic!(
                    "pane did not stop after Ctrl-D: {}",
                    visible(&self.pty.output)
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                let _ = libc::kill(self.pid(), libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}

fn transcript_files(sessions: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(sessions) else {
        return Vec::new();
    };
    let mut stores = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    stores.sort();
    if stores.len() != 1 {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&stores[0]) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter(|path| {
            path.file_name().and_then(|name| name.to_str()) != Some("ephemeral-sessions.jsonl")
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn session_id_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("session transcript {} has no usable id", path.display()))
}

/// The session id the courtesy line named, if any. The line is written just
/// before `exec`, so it is independent evidence of the session the replacement
/// was asked to resume.
fn courtesy_resume_id(transcript: &[u8]) -> Option<String> {
    let anchor = b"To resume this session:";
    let anchor_at = find_bytes(transcript, anchor)? + anchor.len();
    let resume = b"octet --resume ";
    let resume_at = find_bytes(&transcript[anchor_at..], resume)? + anchor_at + resume.len();
    let rest = &transcript[resume_at..];
    let rest = match rest.first() {
        Some(b'\'') => &rest[1..],
        _ => rest,
    };
    let end = rest
        .iter()
        .position(|byte| *byte == b'\'' || byte.is_ascii_whitespace())?;
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

#[derive(Clone, Debug)]
struct MarkerRecord {
    pid: i32,
    ppid: i32,
    args: Vec<String>,
}

/// Every complete record the marker wrapper appended, in file order. Writes
/// that were interrupted before `end` are ignored rather than half-parsed.
fn read_marker(path: &Path) -> Vec<MarkerRecord> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    let mut current: Option<MarkerRecord> = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            current = Some(MarkerRecord {
                pid: value.parse().unwrap_or(0),
                ppid: 0,
                args: Vec::new(),
            });
        } else if let Some(value) = line.strip_prefix("ppid=") {
            if let Some(record) = current.as_mut() {
                record.ppid = value.parse().unwrap_or(0);
            }
        } else if let Some(value) = line.strip_prefix("arg=") {
            if let Some(record) = current.as_mut() {
                record.args.push(value.to_owned());
            }
        } else if line == "end" {
            if let Some(record) = current.take() {
                if record.pid > 0 {
                    records.push(record);
                }
            }
        }
    }
    records
}

/// Wait for the pane's probe of the swapped-in candidate and check its shape.
fn await_probe_record(pane: &mut Pane, marker: &Path, timeout: Duration) -> MarkerRecord {
    let pane_pid = pane.pid();
    let record = pane.wait_for_record(marker, timeout, |record| {
        record.args.iter().any(|arg| arg.as_str() == PROBE_FLAG)
    });
    assert_eq!(
        record.args,
        vec![PROBE_FLAG.to_owned()],
        "the candidate must only receive the internal probe flag"
    );
    assert_eq!(
        record.ppid, pane_pid,
        "the probe must run as a child of the pane process"
    );
    assert_ne!(
        record.pid, pane_pid,
        "the probe is a subprocess, not the pane image"
    );
    record
}

/// Wait for the replacement invocation the pane exec'd into and check the
/// request that the kernel will honour.
fn await_resume_record(pane: &mut Pane, marker: &Path, timeout: Duration) -> MarkerRecord {
    let pane_pid = pane.pid();
    let record = pane.wait_for_record(marker, timeout, |record| {
        record.args.iter().any(|arg| arg.as_str() == "--resume")
    });
    assert_eq!(
        record.pid, pane_pid,
        "the re-exec must replace the running image in place, not fork"
    );
    assert_eq!(
        record.ppid,
        std::process::id() as i32,
        "the replacement image must remain this test's direct child"
    );
    assert_eq!(
        record
            .args
            .iter()
            .filter(|arg| arg.as_str() == "--resume")
            .count(),
        1,
        "the rebuilt argv must name exactly one session: {:?}",
        record.args
    );
    assert!(
        !record.args.iter().any(|arg| arg.as_str() == PROBE_FLAG),
        "the internal probe flag must not survive into the replacement: {:?}",
        record.args
    );
    record
}

fn wrapper_script(image: &Path, marker: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         printf 'pid=%s\\n' \"$$\" >> {marker}\n\
         printf 'ppid=%s\\n' \"$PPID\" >> {marker}\n\
         for arg in \"$@\"; do printf 'arg=%s\\n' \"$arg\" >> {marker}; done\n\
         printf 'end\\n' >> {marker}\n\
         exec {image} \"$@\"\n",
        marker = shell_quote(marker),
        image = shell_quote(image),
    )
}

fn shell_quote(path: &Path) -> String {
    let text = path.to_str().expect("fixture path is UTF-8");
    assert!(
        !text.contains('\''),
        "fixture path contains a single quote: {text}"
    );
    format!("'{text}'")
}

fn process_argv_line(pid: i32) -> String {
    #[cfg(target_os = "linux")]
    {
        let bytes = fs::read(format!("/proc/{pid}/cmdline"))
            .unwrap_or_else(|error| panic!("cannot read /proc/{pid}/cmdline: {error}"));
        let args = bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>();
        assert!(
            !args.is_empty(),
            "the kernel reported no argv for pid {pid}"
        );
        args.join(" ")
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = Command::new("/bin/ps")
            .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .unwrap_or_else(|error| panic!("cannot run ps for pid {pid}: {error}"));
        assert!(output.status.success(), "ps failed for pid {pid}");
        let line = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        assert!(!line.is_empty(), "ps reported no argv for pid {pid}");
        line
    }
}

/// Whether the transcript file can still be exclusively locked right now.
/// Session appends take the lock only for the duration of a write, so a healthy
/// pane releases it; a stale lock from a torn-down image never would.
fn lock_is_available(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open session transcript");
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked == 0 {
            unsafe {
                let _ = libc::flock(file.as_raw_fd(), libc::LOCK_UN);
            }
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Loopback OpenAI-chat fixture that answers every request immediately with a
/// fixed SSE turn, so a pane's durable append can be observed.
struct FixtureApi {
    url: String,
    served: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl FixtureApi {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        listener
            .set_nonblocking(true)
            .expect("nonblocking loopback listener");
        let url = format!(
            "http://{}/v1/",
            listener.local_addr().expect("loopback addr")
        );
        let served = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_served = served.clone();
        let worker_stop = stop.clone();
        let body = fixture_sse();
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
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if socket.write_all(response.as_bytes()).is_ok()
                    && socket.write_all(&body).is_ok()
                    && socket.flush().is_ok()
                {
                    worker_served.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        Self {
            url,
            served,
            stop,
            worker: Some(worker),
        }
    }

    fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

impl Drop for FixtureApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            if let Err(panic) = worker.join() {
                if !thread::panicking() {
                    std::panic::resume_unwind(panic);
                }
            }
        }
    }
}

fn fixture_sse() -> Vec<u8> {
    format!(
        "data: {{\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"{}\"}},\"finish_reason\":null}}]}}\n\n\
         data: {{\"id\":\"fixture\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}\n\n\
         data: [DONE]\n\n",
        String::from_utf8_lossy(FIXTURE_REPLY),
    )
    .into_bytes()
}

/// Read one request's headers, draining its body so the client is not blocked.
fn read_request(socket: &mut TcpStream) -> Option<String> {
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
                continue;
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

fn contains_bytes(bytes: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && bytes.windows(needle.len()).any(|window| window == needle)
}

fn find_bytes(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || bytes.len() < needle.len() {
        return None;
    }
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

fn visible(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 16 * 1024;
    let start = bytes.len().saturating_sub(MAX_BYTES);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let escaped = text.escape_default().to_string();
    if start > 0 {
        format!("…{escaped} ({} bytes total)", bytes.len())
    } else {
        escaped
    }
}

/// (2) A changed executable is probed, then replaces the pane image in place
/// and resumes the exact session the pane was running.
#[test]
fn real_octet_reload_reexecs_a_changed_binary_in_place_and_resumes_the_same_session() {
    let _guard = pty_lock().lock().unwrap_or_else(PoisonError::into_inner);
    let fixture = Fixture::new(UNREACHABLE_PROVIDER);
    let launch = fixture.copy_binary("octet");
    let image = fixture.copy_binary("octet-image");
    let marker = fixture.root().join("reexec-marker.log");
    let mut pane = Pane::spawn(fixture, &launch);
    let session = pane.await_ready(STARTUP_TIMEOUT);
    let session_id = session_id_of(&session);
    let pane_pid = pane.pid();
    let workspace = pane.fixture.workspace.clone();
    let sessions = pane.fixture.sessions.clone();

    // The on-disk image changes under the running process: the pane must probe
    // the candidate (through the wrapper) before it may replace itself.
    pane.install_marker_wrapper(&image, &marker);
    pane.pty.write_input(b"/reload --force\r");

    await_probe_record(&mut pane, &marker, STEP_TIMEOUT);
    let reexec = await_resume_record(&mut pane, &marker, STEP_TIMEOUT);

    // (c) exactly one `--resume`, naming the session the pane was running.
    let resume_index = reexec
        .args
        .iter()
        .position(|arg| arg.as_str() == "--resume")
        .expect("checked above");
    assert_eq!(
        reexec.args[resume_index + 1],
        session_id,
        "the replacement must resume the pane's own session: {:?}",
        reexec.args
    );
    // The canonical argv preserves the pane's invocation and drops nothing it
    // was launched with, so the replacement re-enters the same store.
    let expected = vec![
        "--offline".to_owned(),
        "--no-context-files".to_owned(),
        "--no-tools".to_owned(),
        "--color".to_owned(),
        "never".to_owned(),
        "--mouse".to_owned(),
        "off".to_owned(),
        "--workspace".to_owned(),
        workspace.to_str().expect("workspace path").to_owned(),
        "--session-dir".to_owned(),
        sessions.to_str().expect("session dir path").to_owned(),
        "--model".to_owned(),
        "custom/probe".to_owned(),
        "--theme".to_owned(),
        "dark".to_owned(),
        "--resume".to_owned(),
        session_id.clone(),
    ];
    assert_eq!(
        reexec.args, expected,
        "the replacement argv must be the canonical resume command"
    );

    // The product's own notice and courtesy line agree that a ready plan was
    // presented and the TUI was left before `exec`.
    pane.wait_for(b"reloading into build", STEP_TIMEOUT);
    pane.wait_for(b"To resume this session", STEP_TIMEOUT);
    assert_eq!(
        courtesy_resume_id(&pane.pty.output).as_deref(),
        Some(session_id.as_str()),
        "the courtesy line must name the resumed session"
    );

    // (b)/(d) same pid, new image, responsive pane.
    assert_eq!(pane.pid(), pane_pid, "exec preserves the pane pid");
    pane.wait_until(STEP_TIMEOUT, "the replacement image argv", |pane| {
        pane.ps_argv().contains("--resume")
    });
    let argv = pane.ps_argv();
    assert!(
        argv.contains(&session_id),
        "the live process argv must carry the session: {argv}"
    );
    assert!(
        argv.contains(image.to_str().expect("image path")),
        "the live process must be the replacement image: {argv}"
    );
    let courtesy_at = find_bytes(&pane.pty.output, b"To resume this session")
        .expect("observed above")
        + b"To resume this session".len();
    pane.wait_for_suffix(courtesy_at, b"custom/probe", STEP_TIMEOUT);
    pane.pty.write_input(b"/verbose on\r");
    pane.wait_for(b"verbose transcript enabled", STEP_TIMEOUT);

    // The resumed pane kept the same durable session and created no second one.
    assert_eq!(pane.session_files(), vec![session.clone()]);
    pane.shutdown();
}

/// (3) An unchanged executable stays in this process, says so, and never runs
/// the candidate path that a re-exec would have used.
#[test]
fn real_octet_reload_with_an_unchanged_binary_stays_in_place_and_reports_it() {
    let _guard = pty_lock().lock().unwrap_or_else(PoisonError::into_inner);
    let fixture = Fixture::new(UNREACHABLE_PROVIDER);
    let launch = fixture.copy_binary("octet");
    let marker = fixture.root().join("reexec-marker.log");
    let mut pane = Pane::spawn(fixture, &launch);
    let session = pane.await_ready(STARTUP_TIMEOUT);
    let before_argv = pane.ps_argv();
    assert!(
        !before_argv.contains("--resume"),
        "the pane must start without a resume command: {before_argv}"
    );

    pane.pty.write_input(b"/reload --force\r");
    pane.wait_for(b"binary unchanged", STEP_TIMEOUT);
    assert!(
        contains_bytes(&pane.pty.output, b"resources reloaded"),
        "the unchanged notice must be the resources-only one: {}",
        visible(&pane.pty.output)
    );
    // Both of these are written only after the interactive loop decides to
    // leave the TUI for a replacement image.
    assert!(
        !contains_bytes(&pane.pty.output, b"reloading into build"),
        "an unchanged binary must not produce a ready plan: {}",
        visible(&pane.pty.output)
    );
    assert!(
        !contains_bytes(&pane.pty.output, b"To resume this session"),
        "an unchanged binary must not leave the TUI: {}",
        visible(&pane.pty.output)
    );
    assert!(
        !marker.exists(),
        "no candidate was ever installed, so no wrapper may have run"
    );
    // Any re-exec would have replaced the kernel-visible argv by now.
    thread::sleep(Duration::from_millis(300));
    pane.pty.read_available();
    let after_argv = pane.ps_argv();
    assert!(
        !after_argv.contains("--resume"),
        "the running image was replaced despite an unchanged binary: {after_argv}"
    );
    assert_eq!(
        after_argv, before_argv,
        "an unchanged binary must keep the exact image and argv it started with"
    );
    assert_eq!(pane.session_files(), vec![session.clone()]);

    // Still fully responsive and able to exit cleanly.
    pane.pty.write_input(b"/verbose on\r");
    pane.wait_for(b"verbose transcript enabled", STEP_TIMEOUT);
    pane.shutdown();
}

/// (4) Reloading one pane must not disturb a second pane or its session.
#[test]
fn real_octet_reload_of_one_pane_leaves_the_other_pane_and_session_healthy() {
    let _guard = pty_lock().lock().unwrap_or_else(PoisonError::into_inner);
    let api = FixtureApi::start();
    let fixture_a = Fixture::new(UNREACHABLE_PROVIDER);
    let launch_a = fixture_a.copy_binary("octet");
    let image_a = fixture_a.copy_binary("octet-image");
    let marker_a = fixture_a.root().join("reexec-marker.log");
    let mut pane_a = Pane::spawn(fixture_a, &launch_a);
    let session_a = pane_a.await_ready(STARTUP_TIMEOUT);
    let session_a_id = session_id_of(&session_a);
    let sessions_a = pane_a.fixture.sessions.clone();

    let fixture_b = Fixture::new(&api.url);
    let launch_b = fixture_b.copy_binary("octet");
    let mut pane_b = Pane::spawn(fixture_b, &launch_b);
    let session_b = pane_b.await_ready(STARTUP_TIMEOUT);
    let pid_b = pane_b.pid();
    let argv_b = pane_b.ps_argv();

    // Pane B completes one real turn against the loopback fixture, so its
    // transcript is durable and its writer line is warm.
    pane_b.pty.write_input(b"pane b first prompt\r");
    pane_b.wait_for(FIXTURE_REPLY, STEP_TIMEOUT);
    let durable_b = session_b.clone();
    pane_b.wait_until(STEP_TIMEOUT, "pane B's first turn to durably land", |_| {
        let bytes = fs::read(&durable_b).unwrap_or_default();
        contains_bytes(&bytes, b"pane b first prompt") && contains_bytes(&bytes, FIXTURE_REPLY)
    });
    let settled_b =
        pane_b.wait_for_session_stable(&session_b, Duration::from_millis(500), STEP_TIMEOUT);
    assert!(
        contains_bytes(&settled_b, b"pane b first prompt"),
        "the first prompt must be durable before pane A reloads"
    );
    assert!(contains_bytes(&settled_b, FIXTURE_REPLY));
    assert_eq!(api.served(), 1, "the fixture must have answered one turn");

    // Reload pane A exactly as in (2), with the changed-binary wrapper.
    pane_a.install_marker_wrapper(&image_a, &marker_a);
    pane_a.pty.write_input(b"/reload --force\r");
    await_probe_record(&mut pane_a, &marker_a, STEP_TIMEOUT);
    let reexec = await_resume_record(&mut pane_a, &marker_a, STEP_TIMEOUT);
    let resume_index = reexec
        .args
        .iter()
        .position(|arg| arg.as_str() == "--resume")
        .expect("checked above");
    assert_eq!(reexec.args[resume_index + 1], session_a_id);
    assert!(
        reexec
            .args
            .iter()
            .any(|arg| arg.as_str() == sessions_a.to_str().expect("session dir path")),
        "pane A must resume in its own store: {:?}",
        reexec.args
    );

    // Pane B is untouched: alive, same pid, same argv, same durable bytes,
    // its transcript still lockable, and still never left the TUI.
    assert!(
        pane_b.child.try_wait().expect("poll pane b").is_none(),
        "pane B exited during pane A's reload: {}",
        visible(&pane_b.pty.output)
    );
    assert_eq!(pane_b.pid(), pid_b);
    assert_eq!(
        pane_b.ps_argv(),
        argv_b,
        "pane B's image must not change when pane A reloads"
    );
    assert!(
        fs::read(&session_b)
            .expect("read pane B transcript")
            .starts_with(&settled_b),
        "pane A's reload rewrote pane B's transcript"
    );
    assert_eq!(pane_b.session_files(), vec![session_b.clone()]);
    assert!(
        !contains_bytes(&pane_b.pty.output, b"To resume this session"),
        "pane B must not leave the TUI when pane A reloads"
    );
    assert!(
        lock_is_available(&session_b, STEP_TIMEOUT),
        "pane B's transcript lock was left held after pane A's reload"
    );

    // It can still append: a second turn grows the same file again, and the
    // fixture really answers it before the second turn settles.
    pane_b.pty.write_input(b"pane b second prompt\r");
    pane_b.wait_until(STEP_TIMEOUT, "the second fixture turn", |_| {
        api.served() >= 2
    });
    let grown =
        pane_b.wait_for_session_stable(&session_b, Duration::from_millis(500), STEP_TIMEOUT);
    assert!(
        grown.len() > settled_b.len(),
        "pane B must append its second turn after pane A reloaded"
    );
    assert!(
        contains_bytes(&grown, b"pane b second prompt"),
        "pane B's second turn must be durable after pane A reloaded"
    );

    // And pane A is responsive while pane B keeps running.
    let courtesy_at = {
        pane_a.wait_for(b"To resume this session", STEP_TIMEOUT);
        find_bytes(&pane_a.pty.output, b"To resume this session").expect("observed above")
            + b"To resume this session".len()
    };
    pane_a.wait_for_suffix(courtesy_at, b"custom/probe", STEP_TIMEOUT);
    pane_a.pty.write_input(b"/verbose on\r");
    pane_a.wait_for(b"verbose transcript enabled", STEP_TIMEOUT);
    pane_b.pty.write_input(b"/verbose on\r");
    pane_b.wait_for(b"verbose transcript enabled", STEP_TIMEOUT);

    assert_eq!(pane_a.session_files(), vec![session_a.clone()]);
    assert_eq!(pane_b.session_files(), vec![session_b.clone()]);
    pane_a.shutdown();
    pane_b.shutdown();
}

/// (5) `octet serve` cannot be started hermetically from a default test build:
/// `serve` is an opt-in crate feature, and without it the binary requires an
/// installed `octet-serve` application package under `$HOME/.octet/extensions`.
#[test]
#[ignore = "octet serve needs the embedded 'serve' feature or an installed octet-serve package under a scratch HOME/.octet/extensions; a default build has neither"]
fn real_octet_reload_leaves_a_coexisting_serve_process_untouched() {
    panic!(
        "not implemented: see the #[ignore] reason. A hermetic version needs either \
         `--features serve` (embedded runtime) or a staged octet-serve app package; \
         neither exists for the default test build, so serve coexistence stays unproven."
    );
}
