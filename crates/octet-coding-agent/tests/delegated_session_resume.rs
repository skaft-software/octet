//! Process-boundary coverage for `octet --resume agent-session:<sha256>`.
//!
//! The host publishes exactly one handle for a session-owned delegated child:
//! the opaque, path-free `agent-session:<sha256>` reference
//! (`octet_agent::delegated_session_reference`). These tests prove the
//! *launcher* half of the contract at the real process boundary:
//!
//! * a launchable (settled) worker resolves through
//!   `octet_agent::delegation::resolve_launchable_child_session` and opens **that
//!   child's own transcript**, replaying the child's history — never the parent's
//!   and never a stale session;
//! * a worker parked at the approval boundary, a worker still live in the owning
//!   process, a vanished transcript, an unknown handle, a malformed handle, and a
//!   missing roster each fail closed with their own bounded reason, and an
//!   unattended `--resume` of a parked worker mutates nothing;
//! * a malformed or traversal-bearing reference is rejected before any
//!   filesystem work, a forged roster entry cannot escape the store's private
//!   delegation directory, and no credential-shaped secret reaches any handle or
//!   any error;
//! * ordinary `--resume <session-id>` and the session picker are unchanged.
//!
//! Every test owns an isolated HOME/workspace/session directory and talks only to
//! a loopback fixture on 127.0.0.1. No credentials, no ambient provider state,
//! and no network discovery are involved.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const ASSISTANT_TEXT: &str = "worker pane fixture response";
/// Credential-shaped value carried by the fixture HOME credential and by the
/// durable roster's free-text diagnostic. It must never appear in a handle, in a
/// refusal reason, or in the output of any run.
const WORKER_SECRET: &str = "sk-worker-secret-7d41b90ce8f2";
const WORKER_SECRET_ENV: &str = "OCTET_TEST_WORKER_SECRET";
/// A marker only the child transcript carries, and one only the parent carries:
/// the resumed request proves which transcript was actually replayed.
const CHILD_MARKER: &str = "worker-only-marker-1f0c";
const PARENT_MARKER: &str = "parent-only-marker-2e7b";
const WORKER_PROMPT: &str = "continue the worker task";

const SSE_BODY: &str = concat!(
    "data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"worker pane fixture response\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
);

/// One loopback OpenAI-compatible endpoint: the declared model inventory plus a
/// single streaming turn, with every request body recorded for assertion.
struct LoopbackApi {
    url: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LoopbackApi {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let mut socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                };
                socket.set_nonblocking(false).expect("blocking socket");
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout");
                let mut request = Vec::new();
                let header_end = loop {
                    let mut bytes = [0; 1024];
                    let read = match socket.read(&mut bytes) {
                        Ok(0) => break 0,
                        Ok(read) => read,
                        Err(_) => break 0,
                    };
                    request.extend_from_slice(&bytes[..read]);
                    assert!(request.len() <= 256 * 1024, "bounded loopback request");
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                if header_end == 0 {
                    continue;
                }
                let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let mut bytes = [0; 1024];
                    let read = socket.read(&mut bytes).expect("request body");
                    assert!(read > 0, "request body ended early");
                    request.extend_from_slice(&bytes[..read]);
                }
                if length > 0 {
                    if let Ok(body) = serde_json::from_slice::<serde_json::Value>(
                        &request[header_end..header_end + length],
                    ) {
                        recorded.lock().unwrap().push(body);
                    }
                }
                let (content_type, body): (&str, String) = if headers.starts_with("GET /v1/models ")
                {
                    (
                        "application/json",
                        r#"{"data":[{"id":"probe"}]}"#.to_owned(),
                    )
                } else {
                    ("text/event-stream", SSE_BODY.to_owned())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                if socket.write_all(response.as_bytes()).is_err() {
                    continue;
                }
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

    /// Every recorded chat request, oldest first.
    fn chat_requests(&self) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["messages"].is_array())
            .cloned()
            .collect()
    }
}

impl Drop for LoopbackApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    sessions: PathBuf,
}

impl Fixture {
    fn new(api: &str) -> Self {
        let root = tempfile::tempdir().expect("fixture tempdir");
        let canonical = root.path().canonicalize().expect("canonical fixture root");
        let home = canonical.join("home");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        let record = serde_json::json!({
            "base_url": api,
            "api_key": WORKER_SECRET,
            "api_name": "probe",
            "headers": [],
            "auto_discover": false,
            "models": [{"api_name": "probe"}],
        });
        let credential = home.join(".octet/credentials/custom.json");
        std::fs::write(&credential, record.to_string()).unwrap();
        set_mode(&credential, 0o600);
        Self {
            _root: root,
            home,
            workspace,
            sessions,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command
            .current_dir(&self.workspace)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &self.workspace)
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8")
            // A set environment secret: it must never be echoed by a handle
            // refusal or a launch.
            .env(WORKER_SECRET_ENV, WORKER_SECRET)
            .args(["--offline", "--no-context-files", "--no-tools"])
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--session-dir")
            .arg(&self.sessions)
            .args(["--color", "never"]);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run isolated octet")
    }

    /// The workspace-scoped session directory the store owns (`.serve` and other
    /// dot-directories under the session root are not session stores).
    fn store_directory(&self) -> PathBuf {
        let mut directories = std::fs::read_dir(&self.sessions)
            .expect("session root")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        directories.sort();
        assert_eq!(
            directories.len(),
            1,
            "one workspace-scoped store directory: {directories:?}"
        );
        directories.pop().unwrap()
    }

    /// The store's private delegation directory, where the host lays out
    /// `<session-dir>/.delegation/team-*/`.
    fn delegation_directory(&self) -> PathBuf {
        self.store_directory().join(".delegation")
    }

    fn transcript(&self, file_name: &str) -> Option<PathBuf> {
        fn walk(directory: &Path, file_name: &str) -> Option<PathBuf> {
            for entry in std::fs::read_dir(directory).ok()?.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = walk(&path, file_name) {
                        return Some(found);
                    }
                } else if path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
                    return Some(path);
                }
            }
            None
        }
        walk(&self.sessions, file_name)
    }

    /// Create an ordinary parent session with a run that really reached the
    /// fixture provider.
    fn create_parent_session(&self) -> PathBuf {
        let output = self.run(&[
            "--model",
            "custom/probe",
            "--print",
            "--session-id",
            "parent",
            PARENT_MARKER,
        ]);
        assert_success(&output);
        self.transcript("parent.jsonl")
            .expect("the parent transcript exists")
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn combined(output: &Output) -> String {
    format!("{}\n{}", stdout_of(output), stderr_of(output))
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "octet failed: status={:?} stdout={} stderr={}",
        output.status,
        stdout_of(output),
        stderr_of(output)
    );
}

fn assert_refused(output: &Output, expected_reason: &str) {
    assert!(
        !output.status.success(),
        "the handle must fail closed: stdout={}",
        stdout_of(output)
    );
    assert!(
        stderr_of(output).contains(expected_reason),
        "expected {expected_reason:?} in: {}",
        stderr_of(output)
    );
}

/// No credential, token, or session secret may appear in any output.
fn assert_no_secret(output: &Output) {
    let text = combined(output);
    assert!(
        !text.contains(WORKER_SECRET),
        "a credential-shaped secret reached the process boundary: {text}"
    );
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

/// One owner-only directory, the mode the host's private `team-*` directory has.
fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    set_mode(path, 0o700);
}

/// A real child transcript with its own distinctive history.
fn create_worker_transcript(path: &Path, marker: &str) {
    let mut session = octet_agent::Session::create(path).unwrap();
    session
        .append(octet_agent::EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text(marker.into())],
            },
        )))
        .unwrap();
}

/// The durable roster exactly as the host writes it: one owner-only
/// `fleet.json` in the store's private delegation directory.
fn write_roster(delegation: &Path, session_path: &Path, status: serde_json::Value, detached: bool) {
    let record = serde_json::json!({
        "agent_id": "agent-1",
        "agent_path": "/root/worker",
        "parent_id": "agent-0",
        "depth": 1,
        "task_name": "worker task",
        "display_task_name": "worker task",
        "session_path": session_path,
        "status": status,
        "detached": detached,
        "created_at_ms": 1,
        "started_at_ms": 2,
        "completed_at_ms": null,
        "turn_count": 0,
        "tool_call_count": 0,
        "usage": {
            "input_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "cache_write_1h_tokens": 0,
            "output_tokens": 0,
            "reasoning_tokens": 0,
            "total_tokens": 0,
        },
        "usage_uncertain": false,
        "cost": null,
        "cost_microdollars": null,
        "deadline_at_ms": null,
        "turn_limit": null,
        "extension_principal": null,
        "extension_profile": null,
        "extension_idempotency_key": null,
        "extension_fingerprint": null,
        "extension_policy": null,
        // Free text a forged roster could carry: it must never be relayed.
        "durable_diagnostic": format!("credential {WORKER_SECRET} must not leak"),
    });
    let fleet = serde_json::json!({
        "version": 1,
        "root_session": delegation.join("parent.jsonl"),
        "records": [record],
    });
    let path = delegation.join("fleet.json");
    std::fs::write(&path, fleet.to_string()).unwrap();
    set_mode(&path, 0o600);
}

/// Install one worker: an owner-only `team-<name>/<child>.jsonl` plus its roster
/// entry, and return `(child transcript, host-published handle)`.
fn install_worker(
    fixture: &Fixture,
    team_name: &str,
    child_name: &str,
    marker: &str,
    status: serde_json::Value,
    detached: bool,
) -> (PathBuf, String) {
    let delegation = fixture.delegation_directory();
    private_directory(&delegation.join(team_name));
    let child = delegation.join(team_name).join(child_name);
    create_worker_transcript(&child, marker);
    let handle = octet_agent::delegated_session_reference(&child).expect("host handle");
    write_roster(&delegation, &child, status, detached);
    (child, handle)
}

fn detached() -> serde_json::Value {
    serde_json::json!({"state": "detached"})
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// A launchable worker handle opens that child's session: the child's own
/// history is replayed, the child transcript grows, and the parent is untouched.
#[test]
fn a_launchable_worker_handle_resumes_that_child_session() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(&api.url);
    let parent = fixture.create_parent_session();
    let (child, handle) = install_worker(
        &fixture,
        "team-alpha",
        "0001-worker.jsonl",
        CHILD_MARKER,
        detached(),
        true,
    );

    // The handle is opaque, path-free, and credential-free.
    assert_eq!(handle.len(), "agent-session:".len() + 64);
    assert!(handle.starts_with("agent-session:"));
    assert!(!handle.contains('/'));
    assert!(!handle.contains(WORKER_SECRET));

    // A read-only resolution names exactly the child transcript.
    let inspect = fixture.run(&["sessions", "inspect", &handle]);
    assert_success(&inspect);
    assert_no_secret(&inspect);
    let inspected = stdout_of(&inspect);
    assert!(
        inspected.contains(&format!("Path: {}", child.display())),
        "the handle must resolve to the child transcript: {inspected}"
    );
    assert!(
        !inspected.contains(&format!("Path: {}", parent.display())),
        "a worker handle must never resolve to the parent: {inspected}"
    );

    let before = read(&child);
    let parent_before = read(&parent);
    let requests_before = api.chat_requests().len();
    let resume = fixture.run(&[
        "--model",
        "custom/probe",
        "--resume",
        &handle,
        "--print",
        WORKER_PROMPT,
    ]);
    assert_success(&resume);
    assert_no_secret(&resume);
    assert!(
        stdout_of(&resume).contains(ASSISTANT_TEXT),
        "the resumed child ran a real turn: {}",
        stdout_of(&resume)
    );

    // The child transcript grew; the parent is byte-identical.
    let after = read(&child);
    assert!(
        after.starts_with(&before),
        "the child transcript must be appended, not replaced"
    );
    assert!(
        after.contains(WORKER_PROMPT),
        "the worker pane prompt landed in the child transcript: {after}"
    );
    assert_eq!(
        read(&parent),
        parent_before,
        "resuming a worker must never write the parent transcript"
    );

    // The replayed request is the child's own history, not the parent's.
    let requests = api.chat_requests();
    let resumed = requests[requests_before..]
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        resumed.contains(CHILD_MARKER),
        "the child's own history must be replayed: {resumed}"
    );
    assert!(
        !resumed.contains(PARENT_MARKER),
        "the parent's history must never be replayed into a worker pane: {resumed}"
    );
    assert!(resumed.contains(WORKER_PROMPT), "{resumed}");
}

/// A worker parked at the approval boundary is not openable for unattended
/// mutation: the refusal is bounded, the model is never contacted, and the
/// transcript is byte-identical afterwards.
#[test]
fn a_parked_worker_handle_cannot_be_opened_for_unattended_mutation() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(&api.url);
    fixture.create_parent_session();
    let (child, handle) = install_worker(
        &fixture,
        "team-parked",
        "0001-worker.jsonl",
        CHILD_MARKER,
        serde_json::json!({"state": "awaiting_approval", "reason": "approval is unavailable"}),
        false,
    );
    let before = read(&child);
    let requests_before = api.chat_requests().len();

    let resume = fixture.run(&[
        "--model",
        "custom/probe",
        "--resume",
        &handle,
        "--print",
        WORKER_PROMPT,
    ]);
    assert_refused(&resume, "parked at the approval boundary");
    assert_no_secret(&resume);
    assert!(
        stderr_of(&resume).contains("unattended mutation"),
        "the reason must be actionable: {}",
        stderr_of(&resume)
    );
    assert!(
        !combined(&resume).contains(ASSISTANT_TEXT),
        "no turn may run for a parked worker"
    );
    assert_eq!(
        read(&child),
        before,
        "a parked worker's transcript must not be mutated"
    );
    assert_eq!(
        api.chat_requests().len(),
        requests_before,
        "the model must never be contacted for a parked worker"
    );

    let inspect = fixture.run(&["sessions", "inspect", &handle]);
    assert_refused(&inspect, "parked at the approval boundary");
    assert_no_secret(&inspect);
}

/// Live-in-owning-process, vanished transcript, unknown handle, malformed
/// handle, and missing roster each refuse with their own bounded reason.
#[test]
fn every_unlaunchable_worker_handle_refuses_with_its_own_bounded_reason() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(&api.url);
    fixture.create_parent_session();
    let (child, handle) = install_worker(
        &fixture,
        "team-states",
        "0001-worker.jsonl",
        CHILD_MARKER,
        detached(),
        true,
    );
    let delegation = fixture.delegation_directory();
    let mut reasons = Vec::new();

    for (state, expected) in [("running", "live worker"), ("pending", "live worker")] {
        write_roster(
            &delegation,
            &child,
            serde_json::json!({"state": state}),
            false,
        );
        let inspect = fixture.run(&["sessions", "inspect", &handle]);
        assert_refused(&inspect, expected);
        assert_no_secret(&inspect);
        let stderr = stderr_of(&inspect);
        assert!(
            stderr.contains(state),
            "the reason names the bounded roster state: {stderr}"
        );
        assert!(
            stderr.contains("one session has one writer"),
            "the reason is actionable: {stderr}"
        );
        reasons.push(stderr);
    }

    // Vanished transcript.
    std::fs::remove_file(&child).unwrap();
    write_roster(&delegation, &child, detached(), true);
    let vanished = fixture.run(&["sessions", "inspect", &handle]);
    assert_refused(&vanished, "transcript is gone");
    assert_no_secret(&vanished);
    reasons.push(stderr_of(&vanished));

    // Unknown handle: the roster is readable and does not know it.
    write_roster(&delegation, &child, detached(), true);
    let unknown = format!("agent-session:{}", "0".repeat(64));
    let unknown_run = fixture.run(&["sessions", "inspect", &unknown]);
    assert_refused(&unknown_run, "does not know this worker handle");
    assert_no_secret(&unknown_run);
    reasons.push(stderr_of(&unknown_run));

    // Missing roster: an explicit refusal, not an empty success.
    std::fs::remove_file(delegation.join("fleet.json")).unwrap();
    let missing = fixture.run(&["sessions", "inspect", &handle]);
    assert_refused(&missing, "no readable session-owned delegation roster");
    assert_no_secret(&missing);
    reasons.push(stderr_of(&missing));
    assert!(
        !combined(&missing).contains("fleet.json"),
        "the roster path is never echoed: {}",
        combined(&missing)
    );

    for (index, reason) in reasons.iter().enumerate() {
        assert!(
            reason.len() <= 1024,
            "reason {index} is bounded: {} bytes",
            reason.len()
        );
        for (other_index, other) in reasons.iter().enumerate() {
            if index != other_index {
                assert_ne!(reason, other, "reasons must be distinct");
            }
        }
    }
    // No reason leaks a path, and none leaks the roster's free text.
    assert!(
        !reasons.join("").contains(WORKER_SECRET),
        "no refusal may relay roster free text"
    );
    assert!(
        !reasons
            .join("")
            .contains(&delegation.to_string_lossy().into_owned()),
        "no refusal may relay the delegation path"
    );
}

/// A malformed or traversal-bearing reference is rejected before any filesystem
/// work, and a forged roster entry cannot escape the delegation directory.
#[test]
fn a_malformed_handle_is_rejected_before_path_work_and_a_forged_entry_cannot_escape() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(&api.url);
    fixture.create_parent_session();

    // No delegation directory, no roster: the rejection is the shape check, not
    // a filesystem verdict, so no metacharacter, control byte, or path component
    // can reach a path join.
    let hostile = [
        "agent-session:$(id)".to_owned(),
        "agent-session:../../etc/passwd".to_owned(),
        "agent-session:a;rm -rf /.jsonl".to_owned(),
        "agent-session:/tmp/0001-worker.jsonl".to_owned(),
        "agent-session:team-alpha/0001-worker.jsonl".to_owned(),
        format!("agent-session:{}", "A".repeat(64)),
        format!("agent-session:{}", "a".repeat(63)),
        format!("agent-session:{}x", "a".repeat(64)),
    ];
    assert!(!fixture.delegation_directory().exists());
    for value in &hostile {
        let inspect = fixture.run(&["sessions", "inspect", value]);
        assert_refused(
            &inspect,
            "not a launchable worker handle: expected `agent-session:` followed by exactly 64 lowercase hex digits",
        );
        assert_no_secret(&inspect);
        let resume = fixture.run(&["--model", "custom/probe", "--resume", value, "--print", "x"]);
        assert!(
            !resume.status.success(),
            "a malformed handle must never start a session: {}",
            combined(&resume)
        );
        assert!(
            stderr_of(&resume).contains("not a launchable worker handle"),
            "{}",
            stderr_of(&resume)
        );
        assert_no_secret(&resume);
    }
    assert!(
        !fixture.delegation_directory().exists(),
        "a malformed handle must not create or touch the delegation directory"
    );

    // A forged entry naming a transcript outside the private delegation
    // directory hashes to the same handle (the token covers only the two
    // trailing components) but must still be refused as a path escape.
    let store_directory = fixture.store_directory();
    let escape_team = store_directory.join("team-escape");
    private_directory(&escape_team);
    let escaped = escape_team.join("0001-worker.jsonl");
    create_worker_transcript(&escaped, CHILD_MARKER);
    let escaped_handle = octet_agent::delegated_session_reference(&escaped).unwrap();
    assert_eq!(
        octet_agent::delegated_session_reference(
            &fixture
                .delegation_directory()
                .join("team-escape")
                .join("0001-worker.jsonl")
        )
        .unwrap(),
        escaped_handle,
        "the forged entry must carry the same handle, or the test proves nothing"
    );
    private_directory(&fixture.delegation_directory());
    write_roster(&fixture.delegation_directory(), &escaped, detached(), true);
    let forged = fixture.run(&["sessions", "inspect", &escaped_handle]);
    assert_refused(
        &forged,
        "outside this session store's private delegation directory",
    );
    assert_no_secret(&forged);
    assert!(
        !combined(&forged).contains(&store_directory.to_string_lossy().into_owned()),
        "the refusal names no path: {}",
        combined(&forged)
    );
    let refused_resume = fixture.run(&[
        "--model",
        "custom/probe",
        "--resume",
        &escaped_handle,
        "--print",
        "x",
    ]);
    assert_refused(
        &refused_resume,
        "outside this session store's private delegation directory",
    );
    assert!(
        !combined(&refused_resume).contains(ASSISTANT_TEXT),
        "a forged entry must not open any session"
    );

    // The legitimate child beside the forged entry still resolves: the
    // confinement refuses the escape, not delegation itself.
    let (child, handle) = install_worker(
        &fixture,
        "team-alpha",
        "0001-worker.jsonl",
        CHILD_MARKER,
        detached(),
        true,
    );
    let inspect = fixture.run(&["sessions", "inspect", &handle]);
    assert_success(&inspect);
    assert!(
        stdout_of(&inspect).contains(&format!("Path: {}", child.display())),
        "{}",
        stdout_of(&inspect)
    );
}

/// Ordinary `--resume <session-id>` and the session picker are untouched: an id
/// still resolves to its own transcript, the picker lists only ordinary
/// sessions, and the picker's own bounded refusal is unchanged.
#[test]
fn ordinary_resume_and_the_session_picker_are_unchanged() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(&api.url);
    let parent = fixture.create_parent_session();
    let (child, handle) = install_worker(
        &fixture,
        "team-alpha",
        "0001-worker.jsonl",
        CHILD_MARKER,
        detached(),
        true,
    );
    let child_before = read(&child);

    // An ordinary id keeps exactly its previous resolution and append.
    let resumed = fixture.run(&[
        "--model",
        "custom/probe",
        "--resume",
        "parent",
        "--print",
        "second parent prompt",
    ]);
    assert_success(&resumed);
    assert_no_secret(&resumed);
    let parent_after = read(&parent);
    assert!(
        parent_after.contains("second parent prompt"),
        "an ordinary --resume still appends to its own transcript"
    );
    assert_eq!(
        read(&child),
        child_before,
        "an ordinary --resume must not touch a delegated child"
    );

    // The picker is a flat, non-recursive view of the store: the delegated child
    // is not a row, and the worker handle is never offered as a session.
    let listed = fixture.run(&["sessions", "list"]);
    assert_success(&listed);
    assert_no_secret(&listed);
    let listing = stdout_of(&listed);
    assert!(
        listing.contains("parent"),
        "the picker lists ordinary sessions: {listing}"
    );
    assert!(
        !listing.contains("0001-worker"),
        "the picker must not list a delegated child: {listing}"
    );
    assert!(!listing.contains(&handle), "{listing}");

    // The picker's own refusal for a print run without an id is unchanged.
    let picker = fixture.run(&["--model", "custom/probe", "--resume", "--print", "hello"]);
    assert!(
        !picker.status.success(),
        "a print run cannot open a picker: {}",
        combined(&picker)
    );
    assert!(
        stderr_of(&picker).contains("--resume needs a session id in print mode"),
        "{}",
        stderr_of(&picker)
    );
    assert_no_secret(&picker);
}
