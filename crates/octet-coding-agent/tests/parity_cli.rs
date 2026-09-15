//! Process-boundary coverage for the additive invocation options (parity rows
//! 5.1, 5.2, 5.3, 5.4, 5.5, 5.6, 5.7, 5.8 and roadmap #180).
//!
//! Every test owns an isolated HOME/workspace/session directory and talks only
//! to a loopback fixture on 127.0.0.1. No credentials, no ambient provider
//! state, and no network discovery are involved.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const ASSISTANT_TEXT: &str = "fixture response done";

const SSE_BODY: &str = concat!(
    "data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"fixture response done\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
);

/// One loopback OpenAI-compatible endpoint. It answers `/v1/models` with exactly
/// the declared inventory and every chat completion with one streaming turn, and
/// it records each request body for assertion.
struct LoopbackApi {
    url: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LoopbackApi {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        listener.set_nonblocking(true).expect("nonblocking listener");
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
                let (content_type, body): (&str, String) =
                    if headers.starts_with("GET /v1/models ") {
                        (
                            "application/json",
                            r#"{"data":[{"id":"probe"},{"id":"alpha-model"}]}"#.to_owned(),
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

fn last_user_text(request: &serde_json::Value) -> String {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|message| message["role"] == "user")
        .map(|message| match &message["content"] {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    sessions: PathBuf,
}

impl Fixture {
    fn new(api: Option<&str>) -> Self {
        let root = tempfile::tempdir().expect("fixture tempdir");
        let canonical = root.path().canonicalize().expect("canonical fixture root");
        let home = canonical.join("home");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        if let Some(url) = api {
            let record = serde_json::json!({
                "base_url": url,
                "api_key": "",
                "api_name": "probe",
                "headers": [],
                "auto_discover": false,
                "models": [
                    {"api_name": "probe"},
                    {"api_name": "alpha-model"},
                    {"api_name": "vision-probe", "vision": true},
                ],
            });
            let credential = home.join(".octet/credentials/custom.json");
            std::fs::write(&credential, record.to_string()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mut permissions = std::fs::metadata(&credential).unwrap().permissions();
                permissions.set_mode(0o600);
                std::fs::set_permissions(&credential, permissions).unwrap();
            }
        }
        Self {
            _root: root,
            home,
            workspace,
            sessions,
        }
    }

    /// Write a synthetic, non-localhost OpenAI Codex subscription credential
    /// into the isolated HOME so the catalog really carries Codex models. No
    /// network access is involved: the credential only unlocks the checked-in
    /// fallback inventory while `--offline` is in force.
    fn write_codex_credential(&self, plan: &str) {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;

        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_fixture",
                "chatgpt_plan_type": plan,
                "localhost": false
            }
        });
        let access = format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        );
        let bytes = serde_json::to_vec(&serde_json::json!({
            "tokens": {
                "access_token": access,
                "refresh_token": "refresh",
                "account_id": "acct_fixture"
            },
            "expires_at": u64::MAX
        }))
        .unwrap();
        let path = self.home.join(".octet/credentials/codex.json");
        std::fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(&path, permissions).unwrap();
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

    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
        let mut command = self.command();
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn isolated octet");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().expect("collect isolated octet")
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The workspace-scoped session directory is a hash of the workspace path, so
/// locate the transcript by name instead of assuming a flat layout.
fn session_transcript(fixture: &Fixture, id: &str) -> Option<PathBuf> {
    fn walk(directory: &std::path::Path, id: &str) -> Option<PathBuf> {
        for entry in std::fs::read_dir(directory).ok()?.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = walk(&path, id) {
                    return Some(found);
                }
            } else if path.file_name().and_then(|name| name.to_str()) == Some(&format!("{id}.jsonl"))
            {
                return Some(path);
            }
        }
        None
    }
    walk(&fixture.sessions, id)
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
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

/// 5.1 — `--mode json` writes a session-header-first, delta-only JSONL stream and
/// runs every positional prompt sequentially.
#[test]
fn json_mode_streams_a_session_header_first_delta_only_event_sequence() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    let output = fixture.run(&[
        "--model",
        "custom/probe",
        "--mode",
        "json",
        "first prompt",
        "second prompt",
    ]);
    assert_success(&output);
    let stdout = stdout_of(&output);
    let events = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSONL event"))
        .collect::<Vec<_>>();

    assert_eq!(
        events[0]["type"], "session",
        "the session header must be the first JSONL record"
    );
    assert_eq!(events[0]["format"], "octet-json-events");
    let types = events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    for expected in ["agent_start", "turn_start", "message_start", "message_end", "agent_end"] {
        assert!(types.contains(&expected.to_owned()), "missing {expected} in {types:?}");
    }
    for event in &events {
        if event["type"] == "message_update" {
            assert!(
                event.get("message").is_none(),
                "JSON mode must not repeat cumulative snapshots: {event}"
            );
            assert!(
                event["assistantMessageEvent"].get("partial").is_none(),
                "delta records must not embed the partial message: {event}"
            );
        }
    }
    assert!(stdout.contains(ASSISTANT_TEXT), "assistant text must be streamed");
    assert_eq!(
        api.chat_requests().len(),
        2,
        "both sequential prompts must be submitted"
    );
}

/// 5.5 and 5.6 — piped stdin, `@file` expansion and the remaining positional
/// prompts share the first request, and later prompts run in order.
#[test]
fn piped_stdin_and_files_join_the_first_prompt_before_the_remaining_prompts() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    std::fs::write(fixture.workspace.join("context.txt"), "file context body\n").unwrap();
    let output = fixture.run_with_stdin(
        &[
            "--model",
            "custom/probe",
            "--print",
            "@context.txt",
            "first prompt",
            "second prompt",
        ],
        "piped stdin body\n",
    );
    assert_success(&output);
    assert!(
        stdout_of(&output).contains(ASSISTANT_TEXT),
        "print mode still streams the assistant turn: {}",
        stdout_of(&output)
    );
    let requests = api.chat_requests();
    assert_eq!(requests.len(), 2, "one request per sequential prompt");
    let first = last_user_text(&requests[0]);
    assert!(first.contains("piped stdin body"), "piped stdin reached the model: {first}");
    assert!(first.contains("file context body"), "@file content reached the model: {first}");
    assert!(first.contains("<file name="), "@file is wrapped as an explicit file part: {first}");
    assert!(first.ends_with("first prompt"), "the first positional prompt joins it: {first}");
    assert_eq!(last_user_text(&requests[1]), "second prompt");
}

/// 5.2 — `--list-models` is credential-filtered, sorted, and accepts an optional
/// fuzzy search.
#[test]
fn list_models_lists_the_credential_scoped_catalog_and_filters_by_search() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    let all = fixture.run(&["--list-models"]);
    assert_success(&all);
    let listing = stdout_of(&all);
    assert!(
        listing.lines().next().unwrap_or_default().contains("PROVIDER"),
        "listing header: {listing}"
    );
    assert!(listing.contains("probe") && listing.contains("alpha-model"), "listing: {listing}");
    let probe_index = listing.find("custom/probe").unwrap();
    let alpha_index = listing.find("custom/alpha-model").unwrap();
    assert!(alpha_index < probe_index, "listing must be sorted ascending: {listing}");

    let filtered = fixture.run(&["--list-models", "alphamod"]);
    assert_success(&filtered);
    let filtered = stdout_of(&filtered);
    assert!(filtered.contains("alpha-model"), "fuzzy search: {filtered}");
    assert!(!filtered.contains("custom/probe"), "search must filter: {filtered}");

    let empty = fixture.run(&["--list-models", "no-such-model"]);
    assert_success(&empty);
    assert_eq!(stdout_of(&empty).trim(), "No matching available models.");
}

/// 5.3 — `--session-id` is exact project identity and creates a missing session;
/// `--name` trims and rejects an empty name.
#[test]
fn session_id_creates_the_exact_session_and_name_trims_or_rejects_empty() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    let run = fixture.run(&[
        "--model",
        "custom/probe",
        "--print",
        "--session-id",
        "review-1",
        "--name",
        "  Release review  ",
        "hello",
    ]);
    assert_success(&run);
    assert!(
        session_transcript(&fixture, "review-1").is_some(),
        "the exact session id must create its transcript"
    );

    let inspect = fixture.run(&["sessions", "inspect", "review-1"]);
    assert_success(&inspect);
    assert!(
        stdout_of(&inspect).contains("Name: Release review"),
        "the trimmed name must be persisted: {}",
        stdout_of(&inspect)
    );

    let empty = fixture.run(&["--model", "custom/probe", "--print", "--name", "   ", "hello"]);
    assert!(!empty.status.success(), "an empty name must fail");
    assert!(
        stderr_of(&empty).contains("--name requires a non-empty name"),
        "empty name diagnostic: {}",
        stderr_of(&empty)
    );
}

/// Every file named `name` under the isolated session root.
fn files_named(fixture: &Fixture, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![fixture.sessions.clone()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Every JSONL transcript under the isolated session root, excluding the
/// conversation-free ephemeral accounting ledger.
fn session_transcripts(fixture: &Fixture) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![fixture.sessions.clone()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path.file_name().and_then(|value| value.to_str())
                    != Some("ephemeral-sessions.jsonl")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// 5.4 — `--no-session` keeps the workspace free of transcripts while durable
/// usage, cost and uncertainty accounting survive the run.
#[test]
fn no_session_discards_the_transcript_but_keeps_durable_accounting() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    let run = fixture.run(&["--model", "custom/probe", "--no-session", "--print", "hello"]);
    assert_success(&run);
    assert!(
        stdout_of(&run).contains(ASSISTANT_TEXT),
        "the ephemeral run still answered: {}",
        stdout_of(&run)
    );

    // The conversation is gone: no transcript anywhere in the session root.
    let transcripts = session_transcripts(&fixture);
    assert!(
        transcripts.is_empty(),
        "an ephemeral run must not persist a transcript: {transcripts:?}"
    );

    // ... but the accounting ledger kept the provider usage.
    let ledger = files_named(&fixture, "ephemeral-sessions.jsonl");
    assert_eq!(ledger.len(), 1, "one workspace accounting ledger: {ledger:?}");
    let lines = std::fs::read_to_string(&ledger[0]).unwrap();
    assert_eq!(lines.lines().count(), 1, "{lines}");
    let record: serde_json::Value = serde_json::from_str(lines.trim()).unwrap();
    assert_eq!(
        record["usage_records"].as_array().map(Vec::len),
        Some(1),
        "provider usage survives the discarded transcript: {record}"
    );
    assert_eq!(record["has_uncertain_usage"], serde_json::json!(false));

    // The same accounting is readable through the CLI, and it reports no
    // transcripts for the workspace.
    let listed = fixture.run(&["sessions", "accounting"]);
    assert_success(&listed);
    let report = stdout_of(&listed);
    assert!(report.contains("Runs: 1"), "{report}");
    assert!(report.contains("Usage: 1 record(s)"), "{report}");
    assert!(report.contains("Uncertainty: none recorded"), "{report}");

    let sessions = fixture.run(&["sessions", "list"]);
    assert_success(&sessions);
    assert!(
        stdout_of(&sessions).contains("No sessions"),
        "the ephemeral session is not listed: {}",
        stdout_of(&sessions)
    );
}

/// 5.4 — `--no-session` is refused where it cannot be honoured, and a run that
/// never started records no accounting.
#[test]
fn no_session_fails_closed_for_an_interactive_frontend_and_records_nothing() {
    let fixture = Fixture::new(None);
    let interactive = fixture.run(&["--no-session"]);
    assert!(!interactive.status.success(), "a TUI run cannot be ephemeral");
    let stderr = stderr_of(&interactive);
    assert!(
        stderr.contains("--no-session requires a headless frontend"),
        "diagnostic: {stderr}"
    );

    let named = fixture.run(&["--no-session", "--print", "--name", "ephemeral", "hello"]);
    assert!(!named.status.success(), "an ephemeral run cannot name a session");
    assert!(
        stderr_of(&named).contains("--no-session cannot name a session"),
        "diagnostic: {}",
        stderr_of(&named)
    );

    let accounting = fixture.run(&["sessions", "accounting"]);
    assert_success(&accounting);
    assert!(
        stdout_of(&accounting).contains("No ephemeral runs recorded"),
        "nothing was recorded: {}",
        stdout_of(&accounting)
    );
}

/// The Codex context note never fires for a non-Codex session, even when the
/// catalog is full of Codex models, and it is not re-emitted per turn.
#[test]
fn codex_context_notes_are_not_emitted_for_a_non_codex_session_or_per_turn() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    fixture.write_codex_credential("plus");

    // Precondition: the catalog really does carry Codex models.
    let listed = fixture.run(&["--list-models", "gpt-5.6"]);
    assert_success(&listed);
    assert!(
        stdout_of(&listed).contains("gpt-5.6-sol"),
        "the fixture HOME must register the Codex inventory: {}",
        stdout_of(&listed)
    );

    // Two turns in one session on a non-Codex effective model.
    let run = fixture.run(&[
        "--model",
        "custom/probe",
        "--print",
        "first turn",
        "second turn",
    ]);
    assert_success(&run);
    let stderr = stderr_of(&run);
    assert!(
        !stderr.contains("Codex model"),
        "a non-Codex session must print no Codex note: {stderr}"
    );
    assert!(!stderr.contains("context window — advertised"), "{stderr}");
    assert!(!stderr.contains("is budgeted at"), "{stderr}");
    let events = api.chat_requests().len();
    assert_eq!(events, 2, "both turns really ran: {events}");
}

/// 5.9 — `sessions search` uses the disposable entry index incrementally (a
/// repeat search does not re-index) and reports the change when the index
/// advances.
#[test]
fn sessions_search_is_incremental_and_reports_the_index_change() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    for (id, prompt) in [("search-a", "alpha needle one"), ("search-b", "beta needle two")] {
        let run = fixture.run(&["--model", "custom/probe", "--print", "--session-id", id, prompt]);
        assert_success(&run);
    }

    let cold = fixture.run(&["sessions", "search", "needle"]);
    assert_success(&cold);
    let listing = stdout_of(&cold);
    assert!(
        listing.contains("search-a") && listing.contains("search-b"),
        "both sessions match: {listing}"
    );
    assert!(
        stderr_of(&cold).contains("Indexed 2 session(s)"),
        "the cold search indexes both sessions: {}",
        stderr_of(&cold)
    );

    let warm = fixture.run(&["sessions", "search", "needle"]);
    assert_success(&warm);
    assert!(
        !stderr_of(&warm).contains("Indexed"),
        "a repeat search must not re-index: {}",
        stderr_of(&warm)
    );

    let changed = fixture.run(&[
        "--model",
        "custom/probe",
        "--print",
        "--session-id",
        "search-a",
        "gamma needle three",
    ]);
    assert_success(&changed);
    let delta = fixture.run(&["sessions", "search", "needle"]);
    assert_success(&delta);
    assert!(
        stderr_of(&delta).contains("Indexed 1 session(s)"),
        "only the changed session is re-read: {}",
        stderr_of(&delta)
    );

    let miss = fixture.run(&["sessions", "search", "zzz-no-match"]);
    assert_success(&miss);
    assert!(
        stdout_of(&miss).contains("No session entries match"),
        "a miss is explicit: {}",
        stdout_of(&miss)
    );
}

/// Row 1 — the Codex context-window flag is an explicit opt-in override of the
/// deliberate 272K cap: above the cap without the acknowledgement it fails
/// closed before any run, and zero is refused.
#[test]
fn codex_context_window_override_fails_closed_without_acknowledgement() {
    let fixture = Fixture::new(None);

    let unacknowledged = fixture.run(&["--codex-context-window", "500000", "--print", "hello"]);
    assert!(!unacknowledged.status.success(), "an unacknowledged raise must fail closed");
    let stderr = stderr_of(&unacknowledged);
    assert!(stderr.contains("double-priced"), "diagnostic names the cost cliff: {stderr}");
    assert!(stderr.contains("acknowledge-cost-cliff"), "diagnostic names the flag: {stderr}");

    let zero = fixture.run(&["--codex-context-window", "0", "--print", "hello"]);
    assert!(!zero.status.success(), "zero must fail closed");
    assert!(stderr_of(&zero).contains("greater than zero"), "{}", stderr_of(&zero));

    let oversized = fixture.run(&["--codex-context-window", "2000000", "--print", "hello"]);
    assert!(!oversized.status.success(), "above every entitlement must fail closed");
}

/// Row 1 — with the acknowledgement the opt-in is accepted and a non-Codex run
/// is unaffected (the deliberate default stays untouched without the flag).
#[test]
fn codex_context_window_override_is_accepted_with_the_acknowledgement() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    let accepted = fixture.run(&[
        "--model",
        "custom/probe",
        "--codex-context-window",
        "500000",
        "--codex-context-window-acknowledge-cost-cliff",
        "--print",
        "hello",
    ]);
    assert_success(&accepted);
    assert!(stdout_of(&accepted).contains(ASSISTANT_TEXT));
}

/// 5.10 — `catalog publish` fails closed on a checksum mismatch, publishes the
/// exact bytes once every gate agrees, and refuses to replace the immutable
/// destination afterwards.
#[test]
fn catalog_publish_gates_are_fail_closed_and_the_path_is_immutable() {
    let fixture = Fixture::new(None);
    let document = r#"{"schema":"octet-catalog-1","min_client_version":"0.1.0","required_providers":["openai"],"entries":[{"provider":"openai","model":"gpt-5"}]}"#;
    let source = fixture.workspace.join("catalog.json");
    std::fs::write(&source, document).unwrap();
    let destination = fixture.workspace.join("published.catalog.json");
    let source = source.to_str().unwrap().to_owned();
    let destination = destination.to_str().unwrap().to_owned();

    let wrong_checksum = "00".repeat(32);
    let publish = |checksum: &str| {
        fixture.run(&[
            "catalog",
            "publish",
            &source,
            "--destination",
            &destination,
            "--min-client-version",
            "0.1.0",
            "--require-provider",
            "openai",
            "--expected-count",
            "1",
            "--expected-checksum",
            checksum,
        ])
    };

    let refused = publish(&wrong_checksum);
    assert!(!refused.status.success(), "a bad checksum must refuse to publish");
    let stderr = stderr_of(&refused);
    assert!(stderr.contains("checksum"), "diagnostic: {stderr}");
    assert!(!std::path::Path::new(&destination).exists(), "a refusal leaves no catalog");

    // The refusal reports the computed digest; a matching checksum now publishes.
    let computed = stderr
        .split("computed ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .expect("the refusal reports the computed checksum")
        .to_owned();
    assert_eq!(computed.len(), 64, "sha256 hex: {computed}");
    let published = publish(&computed);
    assert_success(&published);
    assert!(stdout_of(&published).contains("Published catalog"));
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), document);

    let immutable = publish(&computed);
    assert!(!immutable.status.success(), "an existing catalog must never be replaced");
    assert!(stderr_of(&immutable).contains("immutable-path"), "{}", stderr_of(&immutable));
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), document);
}

/// 5.7 — `--models` matches `provider/model` or the bare model id with globs,
/// selects the first scoped model as the default, and warns (without discarding
/// the rest of the scope) for a pattern that matches nothing.
#[test]
fn models_patterns_scope_the_catalog_and_warn_on_a_miss() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    let scoped = fixture.run(&["--models", "custom/alpha-*", "--print", "hello"]);
    assert_success(&scoped);
    assert!(
        stderr_of(&scoped).contains("Model scope selected custom/alpha-model"),
        "the first scoped model becomes the default: {}",
        stderr_of(&scoped)
    );
    let request = &api.chat_requests()[0];
    assert_eq!(
        request["model"], "alpha-model",
        "the scoped model is submitted to the provider: {request}"
    );

    // The provider-qualified form and a bare id glob resolve the same scope.
    for pattern in ["custom-openai/custom/alpha-*", "*alpha-model"] {
        let qualified = fixture.run(&["--models", pattern, "--print", "hello"]);
        assert_success(&qualified);
        assert_eq!(api.chat_requests()[1]["model"], "alpha-model");
    }

    // A miss is a warning; the remaining scope still applies.
    let miss = fixture.run(&[
        "--models",
        "no-such-provider/*,custom/probe",
        "--print",
        "hello",
    ]);
    assert_success(&miss);
    assert!(
        stderr_of(&miss).contains("no credential-configured models match --models pattern"),
        "diagnostic: {}",
        stderr_of(&miss)
    );

    let empty = fixture.run(&["--models", " , ", "--print", "hello"]);
    assert!(!empty.status.success(), "empty patterns must fail closed");
    assert!(
        stderr_of(&empty).contains("at least one non-empty comma-separated pattern"),
        "diagnostic: {}",
        stderr_of(&empty)
    );
}

/// 5.1 — RPC keeps exclusive ownership of stdin: positional prompts are refused
/// rather than silently mixed with the JSONL command stream.
#[test]
fn rpc_mode_rejects_positional_prompts_instead_of_sharing_stdin() {
    let fixture = Fixture::new(None);
    let output = fixture.run(&["--mode", "rpc", "hello"]);
    assert!(!output.status.success(), "positional RPC prompts must fail closed");
    assert!(
        stderr_of(&output).contains("RPC input must be sent as JSONL prompt commands on stdin"),
        "diagnostic: {}",
        stderr_of(&output)
    );
}

/// 5.5 — an admitted inline image becomes a bounded media part for a
/// vision-capable model, octet's explicit admission policy still refuses that
/// media for a model without image input, and a payload that is neither UTF-8
/// text nor a supported image fails before submission.
#[test]
fn file_media_is_admitted_only_for_recognized_images_and_vision_models() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    std::fs::write(
        fixture.workspace.join("pixel.png"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR",
    )
    .unwrap();
    let output = fixture.run(&[
        "--model",
        "custom/vision-probe",
        "--print",
        "@pixel.png",
        "describe the image",
    ]);
    assert_success(&output);
    let request = api.chat_requests()[0].to_string();
    assert!(
        request.contains("data:image/png;base64,"),
        "the image must be submitted as bounded base64 media: {request}"
    );
    assert_eq!(api.chat_requests().len(), 1);

    let unsupported = fixture.run(&["--model", "custom/probe", "--print", "@pixel.png", "hello"]);
    assert!(
        !unsupported.status.success(),
        "a model without image input must refuse the media"
    );
    assert!(
        stderr_of(&unsupported).contains("Image input is unsupported"),
        "diagnostic: {}",
        stderr_of(&unsupported)
    );

    std::fs::write(fixture.workspace.join("binary.dat"), b"\xff\xfe\x00\x01\x02").unwrap();
    let rejected = fixture.run(&["--model", "custom/probe", "--print", "@binary.dat", "hello"]);
    assert!(!rejected.status.success(), "unknown binary input must fail closed");
    assert!(
        stderr_of(&rejected).contains("neither UTF-8 text nor a supported image"),
        "diagnostic: {}",
        stderr_of(&rejected)
    );
}

/// 5.8 — the HTML session export is one script-free file with an inert CSP.
#[test]
fn sessions_export_html_is_a_single_script_free_self_contained_file() {
    let api = LoopbackApi::start();
    let fixture = Fixture::new(Some(&api.url));
    let run = fixture.run(&[
        "--model",
        "custom/probe",
        "--print",
        "--session-id",
        "html-export",
        "hello",
    ]);
    assert_success(&run);

    let output_path = fixture.workspace.join("exported.html");
    let export = fixture.run(&[
        "sessions",
        "export",
        "html-export",
        "--format",
        "html",
        "--output",
        output_path.to_str().unwrap(),
    ]);
    assert_success(&export);
    let html = std::fs::read_to_string(&output_path).expect("HTML export");
    assert!(html.starts_with("<!doctype html>"), "single self-contained document");
    assert!(html.contains("default-src 'none'; img-src data:"), "inert CSP: {html}");
    assert!(!html.contains("<script"), "no scripts may be emitted");
    assert!(!html.contains("onerror"), "no inline handlers may be emitted");
    assert_eq!(
        std::fs::read_dir(&fixture.workspace)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".html"))
            .count(),
        1,
        "the export is exactly one file"
    );
}

/// #180 — `octet serve` accepts a startup session name. The installed extension
/// runtime owns its own launch protocol, so a build without the embedded Serve
/// runtime fails closed instead of silently dropping the requested name.
#[cfg(not(feature = "serve"))]
#[test]
fn serve_name_fails_closed_without_the_embedded_serve_runtime() {
    let fixture = Fixture::new(None);
    let parsed = fixture.run(&["serve", "--help"]);
    assert_success(&parsed);
    let help = stdout_of(&parsed);
    assert!(help.contains("--name"), "serve must accept a startup name: {help}");

    let output = fixture.run(&["serve", "--name", "release review"]);
    assert!(!output.status.success(), "the name must not be silently ignored");
    assert!(
        stderr_of(&output).contains("serve --name requires"),
        "diagnostic: {}",
        stderr_of(&output)
    );
}
