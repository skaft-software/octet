//! Real-binary, loopback-only coverage for the non-interactive provider setup CLI.
//!
//! Each case gives the child an isolated HOME, workspace, and session directory.
//! The only online setup traffic goes to a fixture listener owned by the test;
//! no model request, live credential, or stdin prompt is permitted.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use tempfile::TempDir;

const SECRET: &str = "cli-secret-never-render";
const WAIT: Duration = Duration::from_secs(8);
const MAX_CAPTURE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy)]
enum ServerReply {
    Models,
    Unauthorized,
    Close,
}

/// A bounded HTTP fixture for one explicitly selected `/models` endpoint.
struct SetupServer {
    address: SocketAddr,
    requests: Arc<AtomicUsize>,
    captured_request: Arc<Mutex<Vec<u8>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SetupServer {
    fn start(reply: ServerReply) -> Self {
        Self::start_with_channels(reply, None, None)
    }

    fn delayed(reply: ServerReply) -> (Self, Receiver<()>, Sender<()>) {
        let (observed_tx, observed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = Self::start_with_channels(reply, Some(observed_tx), Some(release_rx));
        (server, observed_rx, release_tx)
    }

    fn start_with_channels(
        reply: ServerReply,
        observed: Option<Sender<()>>,
        release: Option<Receiver<()>>,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind setup fixture");
        listener
            .set_nonblocking(true)
            .expect("make setup fixture nonblocking");
        let address = listener.local_addr().expect("setup fixture address");
        let requests = Arc::new(AtomicUsize::new(0));
        let captured_request = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_capture = Arc::clone(&captured_request);
        let thread_stopped = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            let (mut stream, _) = loop {
                if thread_stopped.load(Ordering::Acquire) {
                    return;
                }
                match listener.accept() {
                    Ok(accepted) => break accepted,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => return,
                }
            };
            if thread_stopped.load(Ordering::Acquire) {
                return;
            }
            // Accepted sockets can inherit O_NONBLOCK on macOS. The bounded
            // header reader relies on blocking I/O with a read timeout; otherwise
            // it can release a response before any request bytes arrive and close
            // with unread data, intermittently resetting the client connection.
            stream
                .set_nonblocking(false)
                .expect("blocking fixture connection");
            thread_requests.fetch_add(1, Ordering::SeqCst);
            let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
            let request = read_headers(&mut stream);
            *thread_capture.lock().expect("capture lock") = request;
            if let Some(observed) = observed {
                let _ = observed.send(());
            }
            if let Some(release) = release {
                loop {
                    if thread_stopped.load(Ordering::Acquire) {
                        return;
                    }
                    match release.recv_timeout(Duration::from_millis(20)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            }
            if thread_stopped.load(Ordering::Acquire) {
                return;
            }
            match reply {
                ServerReply::Close => {}
                ServerReply::Models | ServerReply::Unauthorized => {
                    let (status, body): (&str, &[u8]) = match reply {
                        ServerReply::Models => (
                            "200 OK",
                            br#"{"data":[{"id":"fixture-model","name":"Fixture Model"}]}"#,
                        ),
                        ServerReply::Unauthorized => {
                            ("401 Unauthorized", br#"{"error":"unauthorized"}"#)
                        }
                        ServerReply::Close => unreachable!(),
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                }
            }
        });
        Self {
            address,
            requests,
            captured_request,
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

    fn request_text(&self) -> String {
        let request = self.captured_request.lock().expect("capture lock");
        String::from_utf8_lossy(&request).into_owned()
    }
}

impl Drop for SetupServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Wake the nonblocking accept loop without sending a second setup
        // request. The worker checks `stopped` after accepting this connection.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_headers(stream: &mut TcpStream) -> Vec<u8> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    while request.len() < 16 * 1024 && !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(_) => break,
        }
    }
    request
}

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    workspace: PathBuf,
    sessions: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("CLI fixture tempdir");
        let root_path = root.path().canonicalize().expect("canonical fixture root");
        let home = root_path.join("home");
        let workspace = root_path.join("workspace");
        let sessions = root_path.join("sessions");
        fs::create_dir_all(home.join(".octet/credentials")).expect("fixture HOME");
        fs::create_dir_all(&workspace).expect("fixture workspace");
        fs::create_dir_all(&sessions).expect("fixture sessions");
        Self {
            _root: root,
            home,
            workspace,
            sessions,
        }
    }

    fn base_command(&self) -> Command {
        let path = if cfg!(windows) {
            r"C:\Windows\System32"
        } else {
            "/usr/bin:/bin"
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", path)
            .env("PWD", &self.workspace)
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8")
            .current_dir(&self.workspace)
            .args(["--workspace"])
            .arg(&self.workspace)
            .args(["--session-dir"])
            .arg(&self.sessions)
            .args(["--no-context-files", "--color", "never"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn setup(&self, endpoint: Option<&str>, arguments: &[&str]) -> ProcessOutput {
        self.setup_with_env(endpoint, arguments, &[])
    }

    fn setup_with_env(
        &self,
        endpoint: Option<&str>,
        arguments: &[&str],
        environment: &[(&str, &str)],
    ) -> ProcessOutput {
        let mut command = self.base_command();
        command.arg("setup");
        if let Some(endpoint) = endpoint {
            command.args(["--endpoint", endpoint]);
        }
        command.args(arguments);
        for (name, value) in environment {
            command.env(name, value);
        }
        capture_command(command)
    }

    fn registry_path(&self) -> PathBuf {
        self.home.join(".octet/credentials/custom.json")
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".octet/config.toml")
    }
}

struct CapturedChild {
    child: Child,
    stdout: fs::File,
    stderr: fs::File,
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn spawn_captured(mut command: Command) -> CapturedChild {
    let stdout = tempfile::tempfile().expect("create CLI stdout capture");
    let stderr = tempfile::tempfile().expect("create CLI stderr capture");
    let stdout_child = stdout.try_clone().expect("clone CLI stdout capture");
    let stderr_child = stderr.try_clone().expect("clone CLI stderr capture");
    command
        .stdout(Stdio::from(stdout_child))
        .stderr(Stdio::from(stderr_child));
    configure_child_process_group(&mut command);
    let child = command.spawn().expect("spawn octet fixture");
    CapturedChild {
        child,
        stdout,
        stderr,
    }
}

impl CapturedChild {
    fn wait(mut self) -> ProcessOutput {
        let status = wait_for_child(&mut self.child);
        let stdout = read_capture(&mut self.stdout, "stdout");
        let stderr = read_capture(&mut self.stderr, "stderr");
        ProcessOutput {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for CapturedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            terminate_child(&mut self.child);
            let _ = self.child.wait();
        }
    }
}

fn configure_child_process_group(command: &mut Command) {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(not(unix))]
    let _ = command;
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
        if result == -1 {
            let _ = child.kill();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

fn wait_for_child(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().expect("poll octet fixture") {
            return status;
        }
        if Instant::now() >= deadline {
            terminate_child(child);
            let _ = child.wait();
            panic!("octet fixture child did not exit within {WAIT:?}");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn read_capture(file: &mut fs::File, name: &str) -> String {
    let length = file.metadata().expect("stat CLI output capture").len();
    assert!(
        length <= MAX_CAPTURE_BYTES as u64,
        "CLI {name} exceeded {MAX_CAPTURE_BYTES}-byte capture limit"
    );
    file.seek(SeekFrom::Start(0))
        .expect("rewind CLI output capture");
    let mut bytes = Vec::new();
    file.take(MAX_CAPTURE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .expect("read CLI output capture");
    assert!(
        bytes.len() <= MAX_CAPTURE_BYTES,
        "CLI {name} exceeded {MAX_CAPTURE_BYTES}-byte capture limit"
    );
    String::from_utf8_lossy(&bytes).into_owned()
}

fn capture_command(command: Command) -> ProcessOutput {
    spawn_captured(command).wait()
}

fn assert_no_prompt(output: &ProcessOutput) {
    for stream in [&output.stdout, &output.stderr] {
        assert!(
            !stream.contains("Set up a provider"),
            "setup wizard leaked: {stream}"
        );
        assert!(
            !stream.contains("Enter "),
            "interactive input leaked: {stream}"
        );
    }
}

fn assert_secret_free(output: &ProcessOutput) {
    assert!(!output.stdout.contains(SECRET), "secret appeared on stdout");
    assert!(!output.stderr.contains(SECRET), "secret appeared on stderr");
}

fn assert_no_provider_state(fixture: &Fixture) {
    assert!(
        !fixture.registry_path().exists(),
        "setup unexpectedly wrote a registry"
    );
    assert!(
        !fixture.config_path().exists(),
        "setup unexpectedly wrote a model preference"
    );
}

fn write_concurrent_registry(fixture: &Fixture) {
    let registry = br#"{
  "version": 1,
  "providers": {
    "other": {
      "label": "Other",
      "base_url": "http://127.0.0.1:9/v1/",
      "api_key": "",
      "api_name": "other-model",
      "models": [],
      "auto_discover": false,
      "auth": {"kind": "none"}
    }
  }
}
"#;
    fs::write(fixture.registry_path(), registry).expect("write competing registry");
    #[cfg(unix)]
    fs::set_permissions(fixture.registry_path(), fs::Permissions::from_mode(0o600))
        .expect("protect competing registry");
}

#[test]
fn cli_setup_discovers_selected_model_uses_env_credential_and_matches_tui_receipt() {
    let fixture = Fixture::new();
    let server = SetupServer::start(ServerReply::Models);
    let endpoint = server.url();
    let mut command = fixture.base_command();
    command
        .args(["setup", "--endpoint"])
        .arg(endpoint)
        .args([
            "--provider",
            "custom",
            "--label",
            "Fixture Provider",
            "--api-key-env",
            "EXAMPLE_API_KEY",
            "--model",
            "fixture-model",
            "--yes",
        ])
        .env("EXAMPLE_API_KEY", SECRET);
    let output = capture_command(command);

    assert!(
        output.status.success(),
        "CLI setup failed: {}",
        output.stderr
    );
    assert_eq!(
        server.requests(),
        1,
        "setup should probe one selected endpoint"
    );
    let request = server.request_text().to_ascii_lowercase();
    assert!(
        request.contains("get /v1/models"),
        "unexpected probe request: {request}"
    );
    assert!(
        request.contains("authorization: bearer cli-secret-never-render"),
        "environment credential was not sent to the selected endpoint: {request}"
    );
    for expected in [
        "Provider setup ready",
        "provider: Fixture Provider (custom)",
        "model: custom/custom/fixture-model",
        "traffic: direct only",
        "credentials: Bearer credential is read from EXAMPLE_API_KEY at runtime; its value is not stored",
        "workspace trust:",
        "tool authority:",
        "OS isolation: none",
    ] {
        assert!(
            output.stdout.contains(expected),
            "CLI receipt omitted {expected:?}: {}",
            output.stdout
        );
    }
    assert_secret_free(&output);
    assert_no_prompt(&output);

    let registry = fs::read_to_string(fixture.registry_path()).expect("saved custom registry");
    assert!(registry.contains("fixture-model"));
    assert!(registry.contains("EXAMPLE_API_KEY"));
    assert!(!registry.contains(SECRET));
    let config = fs::read_to_string(fixture.config_path()).expect("saved model preference");
    assert!(config.contains("custom/custom/fixture-model"));
}

#[test]
fn cli_setup_manual_review_cancel_and_offline_paths_do_not_probe_or_prompt() {
    let fixture = Fixture::new();

    let review = fixture.setup(
        None,
        &[
            "--preset",
            "lm-studio",
            "--offline",
            "--manual-model",
            "review-model",
        ],
    );
    assert!(review.status.success(), "review failed: {}", review.stderr);
    assert!(review.stdout.contains("review: not saved"));
    assert!(review.stdout.contains("custom/local/review-model"));
    assert_no_provider_state(&fixture);
    assert_no_prompt(&review);

    let cancelled = fixture.setup(None, &["--cancel"]);
    assert!(
        cancelled.status.success(),
        "cancel failed: {}",
        cancelled.stderr
    );
    assert!(cancelled
        .stdout
        .contains("setup cancelled; no provider state was written"));
    assert_no_provider_state(&fixture);
    assert_no_prompt(&cancelled);

    let no_probe_server = SetupServer::start(ServerReply::Models);
    let endpoint = no_probe_server.url();
    let offline = fixture.setup(Some(&endpoint), &["--offline"]);
    assert!(
        !offline.status.success(),
        "offline discovery unexpectedly succeeded"
    );
    assert!(
        offline.stderr.contains("offline"),
        "offline diagnostic: {}",
        offline.stderr
    );
    assert_eq!(
        no_probe_server.requests(),
        0,
        "offline setup contacted the endpoint"
    );
    assert_no_provider_state(&fixture);
    assert_no_prompt(&offline);

    let committed = fixture.setup(
        None,
        &[
            "--preset",
            "lm-studio",
            "--offline",
            "--manual-model",
            "manual-model",
            "--no-auth",
            "--yes",
        ],
    );
    assert!(
        committed.status.success(),
        "manual setup failed: {}",
        committed.stderr
    );
    assert!(committed.stdout.contains("custom/local/manual-model"));
    assert!(fixture.registry_path().exists());
    assert!(fixture.config_path().exists());
    assert_secret_free(&committed);
    assert_no_prompt(&committed);
}

#[test]
fn cli_setup_reports_unreachable_and_auth_failures_without_writing_state() {
    let fixture = Fixture::new();

    let unreachable_server = SetupServer::start(ServerReply::Close);
    let unreachable = fixture.setup(
        Some(&unreachable_server.url()),
        &["--provider", "unreachable", "--yes"],
    );
    assert!(!unreachable.status.success());
    assert!(
        unreachable.stderr.contains("could not be reached"),
        "unreachable diagnostic: {}",
        unreachable.stderr
    );
    assert_no_provider_state(&fixture);
    assert_no_prompt(&unreachable);

    let auth_server = SetupServer::start(ServerReply::Unauthorized);
    let auth = fixture.setup_with_env(
        Some(&auth_server.url()),
        &[
            "--provider",
            "auth",
            "--api-key-env",
            "EXAMPLE_API_KEY",
            "--yes",
        ],
        &[("EXAMPLE_API_KEY", SECRET)],
    );
    assert!(!auth.status.success());
    assert!(
        auth.stderr.contains("rejected authentication"),
        "authentication diagnostic: {}",
        auth.stderr
    );
    assert_eq!(auth_server.requests(), 1);
    assert_secret_free(&auth);
    assert_no_provider_state(&fixture);
    assert_no_prompt(&auth);
}

#[test]
fn setup_fixture_waits_for_complete_request_headers_before_releasing_response() {
    let (server, observed, release) = SetupServer::delayed(ServerReply::Models);
    let mut client = TcpStream::connect(server.address).expect("connect delayed fixture");
    client
        .set_read_timeout(Some(WAIT))
        .expect("bounded fixture read");
    assert!(matches!(
        observed.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    client
        .write_all(b"GET /v1/models HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    assert!(matches!(
        observed.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    client.write_all(b"\r\n").unwrap();
    observed
        .recv_timeout(WAIT)
        .expect("complete headers observed");
    release.send(()).unwrap();
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("fixture response");
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(server.request_text().ends_with("\r\n\r\n"));
}

#[test]
fn cli_setup_rejects_a_concurrent_registry_change_instead_of_overwriting_it() {
    let fixture = Fixture::new();
    let (server, observed, release) = SetupServer::delayed(ServerReply::Models);
    let endpoint = server.url();
    let mut command = fixture.base_command();
    command.args(["setup", "--endpoint"]).arg(endpoint).args([
        "--provider",
        "race",
        "--model",
        "fixture-model",
        "--yes",
    ]);
    let child = spawn_captured(command);
    observed
        .recv_timeout(WAIT)
        .expect("setup did not reach the selected endpoint");
    write_concurrent_registry(&fixture);
    release.send(()).expect("release delayed setup response");
    let output = child.wait();

    assert!(!output.status.success());
    assert!(
        output
            .stderr
            .contains("custom provider registry changed during setup"),
        "concurrency diagnostic: {}",
        output.stderr
    );
    assert_eq!(server.requests(), 1);
    let registry = fs::read_to_string(fixture.registry_path()).expect("competing registry remains");
    assert!(registry.contains("\"other\""));
    assert!(!registry.contains("\"race\""));
    assert!(!fixture.config_path().exists());
    assert_secret_free(&output);
    assert_no_prompt(&output);
}

#[test]
fn print_and_rpc_unresolved_startup_are_actionable_and_noninteractive() {
    let cases = [
        ("print", vec!["--offline", "--print", "hello"]),
        ("rpc", vec!["--offline", "--mode", "rpc"]),
    ];
    for (name, arguments) in cases {
        let fixture = Fixture::new();
        let mut command = fixture.base_command();
        command.args(arguments);
        let output = capture_command(command);
        assert!(!output.status.success(), "{name} unexpectedly started");
        assert!(
            output.stderr.contains("octet setup --yes"),
            "{name} omitted setup recovery command: {}",
            output.stderr
        );
        assert!(
            output.stderr.contains("available:"),
            "{name} omitted bounded availability details: {}",
            output.stderr
        );
        assert!(
            output.stdout.trim().is_empty(),
            "{name} wrote response stdout"
        );
        assert!(!output.stderr.contains("Set up a provider"));
        assert_no_provider_state(&fixture);
        assert_secret_free(&output);
        assert_no_prompt(&output);
    }
}

/// Process-isolated phase ordering: an unavailable selected model still brackets
/// its own inventory work before the conservative fleet repair. No credentials,
/// inference request, or timing threshold is involved.
#[test]
fn selected_route_trace_brackets_inventory_after_the_cheap_base_phase() {
    for (model, selected_inventory) in [("openai/gpt-4o-mini", true), ("codex/gpt-6-astra", false)]
    {
        let fixture = Fixture::new();
        let mut command = fixture.base_command();
        command
            .args(["--model", model, "--mode", "rpc"])
            .env("OCTET_STARTUP_TRACE", "1")
            .env("AWS_EC2_METADATA_DISABLED", "true");
        let output = capture_command(command);
        assert!(
            !output.status.success(),
            "an unconfigured route must not run"
        );
        let phases: Vec<_> = output
            .stderr
            .lines()
            .filter_map(|line| line.strip_prefix("octet-startup: "))
            .filter_map(|line| line.split_whitespace().next())
            .take_while(|phase| *phase != "catalog.fallback")
            .collect();
        let expected = if selected_inventory {
            vec![
                "catalog.base",
                "catalog.selected",
                "catalog.codex",
                "catalog.copilot",
            ]
        } else {
            vec!["catalog.base", "catalog.codex", "catalog.copilot"]
        };
        assert_eq!(phases, expected, "{}", output.stderr);
        assert_no_prompt(&output);
        assert_secret_free(&output);
    }
}

/// RPC can create multiple sessions in one ephemeral invocation. Seed durable
/// usage while each session is idle; no inference or live credential is needed.
#[test]
fn no_session_rpc_preserves_both_sessions_accounting_before_discarding_transcripts() {
    use octet_ai::{Cost, EndpointId, ModelId, Usage};
    use std::io::BufRead as _;

    let fixture = Fixture::new();
    fs::write(
        fixture.registry_path(),
        serde_json::json!({
            "base_url":"http://127.0.0.1:9/v1/", "api_key":"", "api_name":"probe",
            "auto_discover":false, "models":[{"api_name":"probe"}],
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    fs::set_permissions(fixture.registry_path(), fs::Permissions::from_mode(0o600)).unwrap();
    let mut command = fixture.base_command();
    command
        .args([
            "--offline",
            "--no-tools",
            "--model",
            "custom/probe",
            "--mode",
            "rpc",
            "--no-session",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let stderr = tempfile::tempfile().unwrap();
    command.stderr(Stdio::from(stderr.try_clone().unwrap()));
    configure_child_process_group(&mut command);
    let mut child = command.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut captured = CapturedChild {
        child,
        stdout: tempfile::tempfile().unwrap(),
        stderr,
    };
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in io::BufReader::new(stdout.take(MAX_CAPTURE_BYTES as u64)).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let response = |captured: &mut CapturedChild, id: &str, kind: &str| {
        writeln!(
            captured.child.stdin.as_mut().unwrap(),
            "{}",
            serde_json::json!({"id":id,"type":kind})
        )
        .unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let line = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("bounded RPC response");
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                assert_eq!(value["success"], true, "{value}");
                break value;
            }
        }
    };
    let mut transcripts = Vec::new();
    for index in 0..2 {
        if index != 0 {
            response(&mut captured, "new", "new_session");
        }
        let state = response(&mut captured, "state", "get_state");
        let path = PathBuf::from(state["data"]["sessionFile"].as_str().unwrap());
        let mut session = octet_agent::Session::open(path.clone()).unwrap();
        session
            .record_terminal_gate_usage(
                EndpointId("custom".into()),
                ModelId("probe".into()),
                Usage {
                    input_tokens: 40,
                    output_tokens: 10,
                    total_tokens: 50,
                    ..Usage::default()
                },
                Some(Cost {
                    total: 7,
                    ..Cost::default()
                }),
                Some(true),
            )
            .unwrap();
        if index == 0 {
            session
                .record_usage_uncertainty(
                    EndpointId("custom".into()),
                    ModelId("probe".into()),
                    "rpc-fixture",
                )
                .unwrap();
        }
        transcripts.push(path);
    }
    assert_ne!(transcripts[0], transcripts[1]);
    drop(captured.child.stdin.take());
    let output = captured.wait();
    reader.join().unwrap();
    assert!(output.status.success(), "{}", output.stderr);
    assert!(transcripts.iter().all(|path| !path.exists()));
    let ledgers: Vec<_> = fs::read_dir(&fixture.sessions)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join(".accounting/ephemeral-sessions.jsonl")
        })
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(ledgers.len(), 1);
    let ledger = fs::read_to_string(&ledgers[0]).unwrap();
    assert_eq!(ledger.lines().count(), 1);
    let record: serde_json::Value = serde_json::from_str(ledger.trim()).unwrap();
    assert_eq!(record["usage_records"].as_array().unwrap().len(), 2);
    assert_eq!(record["session_cost_microdollars"], 14);
    assert_eq!(
        record["usage_uncertainty_records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(record["has_uncertain_usage"], true);
}
