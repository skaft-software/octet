//! P4 acceptance: the Pi extension host must never hold up the first frame.
//!
//! The user-visible failure is startup latency with a real Pi pile: the first
//! interactive frame waits for every factory to load. This test runs the real
//! binary on a PTY against the real `octet-pi-compat` adapter, configured
//! around one reviewed fixture factory that spends 2 s inside its factory
//! function and registers a `before_agent_start` (native `before_prompt`) hook.
//!
//! It asserts both halves of the contract:
//!
//! * `frame.ready` with the slow fixture stays within 2x the same binary's
//!   `frame.ready` without extensions, and far below the fixture's own 2 s;
//! * a prompt submitted the instant the frame is ready waits for that hook —
//!   the model request carries the system prompt the hook installed — instead
//!   of racing past it.
//!
//! `OCTET_STARTUP_TRACE` is captured on a separate stderr pipe, never on the
//! terminal, because a terminal stderr routes diagnostics into the TUI where
//! they are not observable.

#![cfg(unix)]

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const COLUMNS: u16 = 100;
const ROWS: u16 = 30;
const FRAME_BUDGET: Duration = Duration::from_secs(20);
const HOOK_MARKER: &str = "P4_FIXTURE_HOOK_RAN";

/// The fixture factory blocks its event loop for two seconds, exactly like a
/// heavy real-world factory (module graph plus synchronous setup).
const SLOW_FACTORY: &str = r#"import { writeFileSync } from 'node:fs';
export default pi => {
  const until = Date.now() + 2000;
  while (Date.now() < until) { /* 2 s inside the factory */ }
  pi.on('before_agent_start', async event => {
    writeFileSync(MARKER_PATH, JSON.stringify({ prompt: event.prompt }));
    return { systemPrompt: `${event.systemPrompt}\nHOOK_MARKER_TEXT` };
  });
};
"#;

fn pty_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn set_nonblocking(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0, "fcntl F_GETFL: {}", io::Error::last_os_error());
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_eq!(result, 0, "fcntl F_SETFL: {}", io::Error::last_os_error());
}

fn duplicate_stdio(fd: RawFd) -> Stdio {
    let duplicated = unsafe { libc::dup(fd) };
    assert!(duplicated >= 0, "dup: {}", io::Error::last_os_error());
    set_nonblocking(duplicated);
    Stdio::from(unsafe { File::from_raw_fd(duplicated) })
}

/// Minimal PTY for this test: a nonblocking master transcript plus the child.
struct Pty {
    master: File,
    slave: File,
    output: Vec<u8>,
}

impl Pty {
    fn open() -> Self {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut dimensions = libc::winsize {
            ws_row: ROWS,
            ws_col: COLUMNS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = unsafe {
            #[allow(clippy::unnecessary_mut_passed)]
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dimensions,
            )
        };
        assert_eq!(opened, 0, "openpty failed: {}", io::Error::last_os_error());
        set_nonblocking(master_fd);
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        Self {
            master,
            slave,
            output: Vec::new(),
        }
    }

    fn read_available(&mut self) {
        let mut buffer = [0u8; 8192];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => self.output.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
    }

    fn write_input(&mut self, input: &[u8]) {
        self.master.write_all(input).expect("write PTY input");
        self.master.flush().expect("flush PTY input");
    }
}

/// Loopback OpenAI-compatible provider that records each request body.
struct MockProvider {
    url: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl MockProvider {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut bytes = [0u8; 1024];
                    let read = socket.read(&mut bytes).unwrap();
                    assert!(read > 0, "request ended before headers");
                    request.extend_from_slice(&bytes[..read]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let mut bytes = [0u8; 1024];
                    let read = socket.read(&mut bytes).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&bytes[..read]);
                }
                if let Ok(body) =
                    serde_json::from_slice::<serde_json::Value>(&request[header_end..])
                {
                    recorded.lock().unwrap().push(body);
                }
                let sse = concat!(
                    "data: {\"id\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"p4 ok\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                    "data: [DONE]\n\n",
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    sse.len(),
                    sse
                );
                let _ = socket.write_all(response.as_bytes());
                let _ = socket.flush();
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn user_prompts(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .flat_map(|request| request["messages"].as_array().cloned().unwrap_or_default())
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_str().map(str::to_owned))
            .collect()
    }

    fn system_prompts(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .flat_map(|request| request["messages"].as_array().cloned().unwrap_or_default())
            .filter(|message| message["role"] == "system")
            .filter_map(|message| message["content"].as_str().map(str::to_owned))
            .collect()
    }
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    workspace: PathBuf,
    sessions: PathBuf,
    extension_root: PathBuf,
    hook_marker: PathBuf,
    trace: PathBuf,
}

impl Fixture {
    /// A disposable HOME/workspace plus a real reviewed adapter around one
    /// fixture factory (only when `with_fixture`).
    fn new(with_fixture: bool, api: Option<&str>) -> Self {
        let root = tempfile::tempdir().expect("fixture tempdir");
        let canonical = root.path().canonicalize().expect("canonical fixture root");
        let home = canonical.join("home");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        let extension_root = canonical.join("extensions");
        for directory in [&home, &workspace, &sessions] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::create_dir_all(home.join(".octet/credentials")).unwrap();
        let credential = home.join(".octet/credentials/custom.json");
        fs::write(
            &credential,
            serde_json::json!({
                "base_url": api.unwrap_or("http://127.0.0.1:9/v1/"),
                "api_key": "",
                "api_name": "probe",
                "headers": [],
                "auto_discover": false,
                "models": [],
            })
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
        let hook_marker = canonical.join("hook-marker.json");
        if with_fixture {
            let entry = canonical.join("slow-factory.mjs");
            fs::write(
                &entry,
                SLOW_FACTORY
                    .replace("MARKER_PATH", &serde_json::to_string(&hook_marker).unwrap())
                    .replace("HOOK_MARKER_TEXT", HOOK_MARKER),
            )
            .unwrap();
            let adapter = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../extensions/octet-pi-compat")
                .canonicalize()
                .expect("checked-in adapter path");
            let configured = Command::new("node")
                .arg(adapter.join("configure.mjs"))
                .args(["--reviewed", "--output"])
                .arg(extension_root.join("octet-pi-compat"))
                .arg(&entry)
                .current_dir(&workspace)
                .env("PI_OFFLINE", "1")
                .output()
                .expect("Node and adapter dependencies are required");
            assert!(
                configured.status.success(),
                "fixture configure failed: {}",
                String::from_utf8_lossy(&configured.stderr)
            );
        }
        let trace = canonical.join("trace.log");
        Self {
            _root: root,
            home,
            workspace,
            sessions,
            extension_root,
            hook_marker,
            trace,
        }
    }

    fn command(&self, binary: &Path, with_fixture: bool) -> Command {
        let mut command = Command::new(binary);
        command
            .args([
                "--offline",
                "--no-context-files",
                "--color",
                "never",
                "--model",
                "custom/probe",
                "--workspace",
            ])
            .arg(&self.workspace)
            .arg("--session-dir")
            .arg(&self.sessions)
            .current_dir(&self.workspace)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("PWD", &self.workspace)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LANG", "C.UTF-8")
            .env("OCTET_COLOR_SCHEME", "dark")
            .env("OCTET_STARTUP_TRACE", "1");
        if with_fixture {
            command
                .arg("--extension-dir")
                .arg(&self.extension_root)
                .args(["--enable-extension", "octet-pi-compat"])
                .args(["--trust-extension", "octet-pi-compat"]);
        }
        command
    }
}

struct OctetRun {
    child: Child,
    pty: Pty,
    trace: PathBuf,
}

impl Drop for OctetRun {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                self.pty.read_available();
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

impl OctetRun {
    fn spawn(fixture: &Fixture, with_fixture: bool) -> Self {
        let pty = Pty::open();
        // stderr is a pipe-shaped file: the trace is an off-screen channel and
        // a terminal stderr would route it into the TUI diagnostics queue.
        let trace_file = File::create(&fixture.trace).expect("trace file");
        let mut command = fixture.command(Path::new(env!("CARGO_BIN_EXE_octet")), with_fixture);
        command
            .stdin(duplicate_stdio(pty.slave.as_raw_fd()))
            .stdout(duplicate_stdio(pty.slave.as_raw_fd()))
            .stderr(Stdio::from(trace_file));
        let tty_fd = pty.slave.as_raw_fd();
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
        let child = command.spawn().expect("spawn octet under PTY");
        Self {
            child,
            pty,
            trace: fixture.trace.clone(),
        }
    }

    fn trace_text(&self) -> String {
        fs::read_to_string(&self.trace).unwrap_or_default()
    }

    /// The phase value in micros, or `None` when the phase has not run yet.
    fn phase_micros(&self, phase: &str) -> Option<u128> {
        let needle = format!("octet-startup: {phase} elapsed=");
        for line in self.trace_text().lines() {
            if let Some(rest) = line.strip_prefix(&needle) {
                let value = rest.strip_suffix("us")?;
                return value.parse().ok();
            }
        }
        None
    }

    /// One adapter-forwarded phase instant, when the child reported it.
    fn adapter_phase_micros(&self, phase: &str) -> Option<u128> {
        self.phase_micros(phase)
    }

    /// Waits for a phase boundary, reading the PTY so the child never blocks on
    /// a full terminal buffer.
    fn await_phase(&mut self, phase: &str, timeout: Duration) -> u128 {
        let deadline = Instant::now() + timeout;
        loop {
            self.pty.read_available();
            if let Some(value) = self.phase_micros(phase) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "phase {phase} did not run within {timeout:?}; trace: {}",
                self.trace_text()
            );
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "octet exited before {phase}; trace: {}",
                self.trace_text()
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn shutdown(mut self) {
        self.pty.write_input(&[4]); // Ctrl-D
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.pty.read_available();
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Headline acceptance: with a factory that sleeps 2 s, the first interactive
/// frame is drawn immediately and a prompt submitted right away still runs the
/// factory's `before_prompt` hook before the provider request.
#[test]
fn slow_fixture_factory_never_blocks_the_first_frame_and_hook_order_holds() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let provider = MockProvider::start();

    // Baseline: the same binary with no extensions at all. Measured on both
    // sides of the fixture run so a load spike on this shared machine cannot
    // make one side look artificially fast: the budget is 2x the slower
    // octet-only frame, exactly the order's "2x the octet-only time".
    let baseline = Fixture::new(false, None);
    let mut octet = OctetRun::spawn(&baseline, false);
    let baseline_before = octet.await_phase("frame.ready", FRAME_BUDGET);
    assert!(
        baseline_before < 1_000_000,
        "octet-only frame.ready unexpectedly slow: {baseline_before}us"
    );
    octet.shutdown();

    // Fixture: one reviewed factory that blocks for 2 s and installs a hook.
    let fixture = Fixture::new(true, Some(&provider.url));
    let hook_marker = fixture.hook_marker.clone();
    let mut octet = OctetRun::spawn(&fixture, true);
    let frame_ready = octet.await_phase("frame.ready", FRAME_BUDGET);

    // The composer accepts input as soon as the frame is ready; submit before
    // the 2 s factory can possibly have registered anything.
    octet.pty.write_input(b"p4 prompt before extensions\r");
    assert!(
        !hook_marker.exists(),
        "fixture hook ran before the prompt was submitted"
    );

    // The hook must run for the submitted prompt, so the provider sees the
    // system prompt it installed.
    let deadline = Instant::now() + Duration::from_secs(20);
    while provider.user_prompts().is_empty() && Instant::now() < deadline {
        octet.pty.read_available();
        thread::sleep(Duration::from_millis(10));
    }
    let prompts = provider.user_prompts();
    assert_eq!(
        prompts,
        vec!["p4 prompt before extensions".to_owned()],
        "the submitted prompt must reach the provider exactly once"
    );
    let systems = provider.system_prompts();
    assert!(
        systems.iter().any(|system| system.contains(HOOK_MARKER)),
        "the before_prompt hook must run before the provider request: {systems:?}"
    );
    assert!(
        hook_marker.exists(),
        "the before_prompt hook must run for the submitted prompt"
    );
    let observed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&hook_marker).unwrap()).unwrap();
    assert_eq!(
        observed["prompt"],
        serde_json::json!("p4 prompt before extensions")
    );

    // `extensions.settled` (last handshake installed) and the adapter's own
    // `initialize` line prove the extension attached after the frame was drawn.
    let settled = octet.await_phase("extensions.settled", FRAME_BUDGET);
    let initialized = octet
        .adapter_phase_micros("extensions.process.initialize:octet-pi-compat")
        .expect("the fixture adapter reports its initialize duration");
    octet.shutdown();

    // The second octet-only frame, taken immediately after the fixture run.
    let baseline_after = {
        let baseline = Fixture::new(false, None);
        let mut octet = OctetRun::spawn(&baseline, false);
        let value = octet.await_phase("frame.ready", FRAME_BUDGET);
        octet.shutdown();
        value
    };
    let baseline_frame_ready = baseline_before.max(baseline_after);

    // The order's budget: no more than 2x the same binary's octet-only time.
    assert!(
        frame_ready <= 2 * baseline_frame_ready,
        "frame.ready with the slow fixture ({frame_ready}us) must stay within 2x the \
         octet-only time ({baseline_frame_ready}us; before {baseline_before}us, after {baseline_after}us)"
    );
    // And the fixture's own 2 s can never be inside the first frame.
    assert!(
        frame_ready < 1_500_000,
        "frame.ready with the slow fixture was {frame_ready}us"
    );
    assert!(
        settled > frame_ready,
        "extensions.settled ({settled}us) must follow the frame ({frame_ready}us)"
    );
    assert!(
        initialized > frame_ready,
        "the fixture handshake ({initialized}us) must settle after the frame ({frame_ready}us)"
    );
}
