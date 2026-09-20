//! Behavioral coverage for the isolated scripted and model-backed eval harness
//! (parity row 5.11).
//!
//! Every test runs the real `octet eval` command in a process boundary with an
//! isolated HOME/workspace/session root and asserts from the artifact it wrote.
//! Model-mode servers live in the test process, outside the harness; all traffic
//! is loopback-only and no test touches a live or paid endpoint.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    sessions: PathBuf,
    artifacts: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture tempdir");
        let canonical = root.path().canonicalize().expect("canonical root");
        let home = canonical.join("home");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        let artifacts = canonical.join("artifacts");
        for directory in [&home, &workspace, &sessions, &artifacts] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
        Self {
            _root: root,
            home,
            workspace,
            sessions,
            artifacts,
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
            .output()
            .expect("run the isolated octet binary")
    }

    fn write_suite(&self, name: &str, body: &str) -> PathBuf {
        let path = self.workspace.join(format!("{name}.json"));
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The newest run artifact directory the harness created.
    fn latest_run_dir(&self) -> Option<PathBuf> {
        let mut directories = std::fs::read_dir(&self.artifacts)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        directories.sort();
        directories.pop()
    }

    /// The newest run's `report.json`, parsed.
    fn latest_report(&self) -> serde_json::Value {
        let run = self.latest_run_dir().expect("a run artifact directory");
        let report = std::fs::read_to_string(run.join("report.json")).expect("report.json");
        serde_json::from_str(&report).expect("valid report JSON")
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "octet eval failed: status={:?} stdout={} stderr={}",
        output.status,
        stdout_of(output),
        stderr_of(output)
    );
}

fn number(value: &serde_json::Value, key: &str) -> f64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_else(|| panic!("{key} is a number in {value}"))
}

/// 5.11 — a run records pass/fail, latency and cost per case, plus the isolation
/// it ran under, in its own artifact directory.
#[test]
fn eval_run_records_cases_isolation_and_deltas_in_its_artifact_directory() {
    let fixture = Fixture::new();
    let baseline_suite = fixture.write_suite(
        "baseline",
        r#"{
  "schema": "octet-eval-suite-1",
  "name": "baseline suite",
  "cases": [
    {"id": "greets", "prompt": "say hello", "expect": {"contains": "hello from the eval fixture"},
     "fixture": {"response": "hello from the eval fixture", "input_tokens": 12, "output_tokens": 6},
     "budgets": {"max_latency_ms": 120000}},
    {"id": "misses", "prompt": "say goodbye", "expect": {"contains": "definitely-not-present"},
     "fixture": {"response": "hello from the eval fixture", "input_tokens": 12, "output_tokens": 6}}
  ]
}"#,
    );
    let artifacts = fixture.artifacts.to_str().unwrap().to_owned();
    let baseline_run = fixture.run(&[
        "eval",
        "run",
        baseline_suite.to_str().unwrap(),
        "--artifact-dir",
        &artifacts,
    ]);
    assert_success(&baseline_run);
    let baseline = fixture.latest_report();

    assert_eq!(baseline["schema"], "octet-eval-run-1");
    assert_eq!(baseline["totals"]["cases"], 2);
    assert_eq!(baseline["totals"]["passed"], 1);
    assert_eq!(baseline["totals"]["failed"], 1);
    assert_eq!(number(&baseline["totals"], "pass_rate_pp"), 50.0);
    assert_eq!(baseline["isolation"]["credentials"], "injected-fixture");
    assert_eq!(baseline["isolation"]["network"], "loopback-only");
    assert_eq!(baseline["isolation"]["ambient_environment"], "stripped");
    assert_eq!(baseline["isolation"]["offline"], true);
    assert!(
        baseline["isolation"]["fixture_base_url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("http://127.0.0.1:"),
        "the injected endpoint is loopback: {}",
        baseline["isolation"]["fixture_base_url"]
    );

    let cases = baseline["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0]["id"], "greets");
    assert_eq!(cases[0]["passed"], true);
    assert_eq!(cases[0]["fixture_requests"], 1);
    assert!(cases[0]["latency_ms"].as_u64().is_some(), "{cases:?}");
    assert_eq!(cases[0]["input_tokens"], 12);
    assert_eq!(cases[0]["output_tokens"], 6);
    assert!(
        cases[0]["cost_microdollars"].as_u64().is_some(),
        "{cases:?}"
    );
    assert_eq!(cases[1]["passed"], false);
    assert!(
        cases[1]["failure"]
            .as_str()
            .unwrap_or_default()
            .contains("definitely-not-present"),
        "{cases:?}"
    );

    // The run directory holds the incremental records and the text report too.
    let report_dir = fixture
        .latest_run_dir()
        .expect("the newest artifact run directory");
    let runs = std::fs::read_to_string(report_dir.join("runs.jsonl")).unwrap();
    assert_eq!(runs.lines().count(), 2, "{runs}");
    assert!(runs
        .lines()
        .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()));
    assert!(report_dir.join("report.txt").is_file());

    // A candidate suite compared with the baseline records every delta.
    let candidate_suite = fixture.write_suite(
        "candidate",
        r#"{
  "schema": "octet-eval-suite-1",
  "name": "candidate suite",
  "cases": [
    {"id": "greets", "prompt": "say hello", "expect": {"contains": "hello from the eval fixture"},
     "fixture": {"response": "hello from the eval fixture", "input_tokens": 12, "output_tokens": 40}},
    {"id": "misses", "prompt": "say goodbye", "expect": {"contains": "hello from the eval fixture"},
     "fixture": {"response": "hello from the eval fixture", "input_tokens": 12, "output_tokens": 40}}
  ]
}"#,
    );
    let baseline_report = report_dir.join("report.json");
    let candidate_run = fixture.run(&[
        "eval",
        "run",
        candidate_suite.to_str().unwrap(),
        "--artifact-dir",
        &artifacts,
        "--baseline",
        baseline_report.to_str().unwrap(),
    ]);
    assert_success(&candidate_run);
    let candidate = fixture.latest_report();

    assert_eq!(candidate["totals"]["passed"], 2);
    assert_eq!(number(&candidate["totals"], "pass_rate_pp"), 100.0);
    let deltas = &candidate["deltas"];
    assert_eq!(deltas["baseline_cases"], 2);
    assert_eq!(number(deltas, "pass_rate_pp_delta"), 50.0);
    assert_eq!(deltas["output_tokens_total_delta"], 68);
    assert_eq!(
        number(deltas, "cost_microdollars_total_delta"),
        number(&candidate["totals"], "cost_microdollars_total")
            - number(&baseline["totals"], "cost_microdollars_total")
    );
    assert_eq!(
        number(deltas, "latency_ms_mean_delta"),
        number(&candidate["totals"], "latency_ms_mean")
            - number(&baseline["totals"], "latency_ms_mean")
    );
}

/// 5.11 — the run is isolated: ambient credentials, proxies and provider base
/// URLs in the parent environment are neither forwarded nor needed.
#[test]
fn eval_run_ignores_ambient_credentials_and_uses_only_its_own_loopback_fixture() {
    let fixture = Fixture::new();
    let suite = fixture.write_suite(
        "isolated",
        r#"{"schema":"octet-eval-suite-1","cases":[
  {"id":"answers","prompt":"hello","expect":{"contains":"hello from the eval fixture"},
   "fixture":{"response":"hello from the eval fixture","input_tokens":5,"output_tokens":7}}
]}"#,
    );
    let artifacts = fixture.artifacts.to_str().unwrap().to_owned();

    // A hostile ambient environment: a live-looking key, an unroutable base
    // URL and a broken proxy. None of them may reach the isolated run.
    let mut command = fixture.command();
    let output = command
        .env("OPENAI_API_KEY", "sk-live-should-never-be-used")
        .env("OPENAI_BASE_URL", "http://127.0.0.1:1/v1")
        .env("ANTHROPIC_API_KEY", "sk-ant-should-never-be-used")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("OCTET_CODEX_CONTEXT_WINDOW", "1000000")
        .args([
            "eval",
            "run",
            suite.to_str().unwrap(),
            "--artifact-dir",
            &artifacts,
        ])
        .output()
        .expect("run the isolated octet binary");
    assert_success(&output);

    let report = fixture.latest_report();
    assert_eq!(report["totals"]["passed"], 1);
    assert_eq!(report["cases"][0]["fixture_requests"], 1);
    assert_eq!(report["cases"][0]["input_tokens"], 5);
    assert_eq!(report["cases"][0]["output_tokens"], 7);
    assert_eq!(report["isolation"]["ambient_environment"], "stripped");
    // The harness injects its credential into its own temporary HOME: the
    // caller's HOME is never written to.
    assert!(
        !fixture.home.join(".octet/credentials/custom.json").exists(),
        "the caller's HOME must stay untouched"
    );
}

/// 5.11 — a suite cannot reach a remote provider at all: the schema refuses any
/// field that could name one, and no artifact is written.
#[test]
fn eval_run_rejects_a_suite_that_tries_to_declare_a_remote_provider() {
    let fixture = Fixture::new();
    let artifacts = fixture.artifacts.to_str().unwrap().to_owned();

    for (name, body) in [
        (
            "provider",
            r#"{"schema":"octet-eval-suite-1","provider":{"base_url":"https://api.openai.com/v1"},"cases":[{"id":"a","prompt":"p"}]}"#,
        ),
        (
            "endpoint",
            r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"p","endpoint":"https://api.openai.com/v1"}]}"#,
        ),
        (
            "key",
            r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"p","api_key":"sk-live"}]}"#,
        ),
    ] {
        let suite = fixture.write_suite(name, body);
        let output = fixture.run(&[
            "eval",
            "run",
            suite.to_str().unwrap(),
            "--artifact-dir",
            &artifacts,
        ]);
        assert!(!output.status.success(), "{name} must fail closed");
        let stderr = stderr_of(&output);
        assert!(
            stderr.contains("a suite cannot name a provider, endpoint or key"),
            "{name} diagnostic: {stderr}"
        );
    }

    // A suite with the wrong schema is refused too, and nothing was recorded.
    let wrong = fixture.write_suite(
        "wrong-schema",
        r#"{"schema":"octet-eval-suite-2","cases":[{"id":"a","prompt":"p"}]}"#,
    );
    let output = fixture.run(&[
        "eval",
        "run",
        wrong.to_str().unwrap(),
        "--artifact-dir",
        &artifacts,
    ]);
    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("octet-eval-suite-1"),
        "{}",
        stderr_of(&output)
    );

    let runs = std::fs::read_dir(&fixture.artifacts)
        .unwrap()
        .filter_map(Result::ok)
        .count();
    assert_eq!(runs, 0, "a refused suite writes no artifact directory");
}

/// The harness must not write into the workspace it was pointed at: the parent's
/// session root stays untouched because each case owns a private temp HOME.
#[test]
fn eval_run_leaves_the_invoking_workspace_and_session_root_untouched() {
    let fixture = Fixture::new();
    let suite = fixture.write_suite(
        "clean",
        r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"hi","expect":{"contains":"hello"}}]}"#,
    );
    let artifacts = fixture.artifacts.to_str().unwrap().to_owned();
    let run = fixture.run(&[
        "eval",
        "run",
        suite.to_str().unwrap(),
        "--artifact-dir",
        &artifacts,
    ]);
    assert_success(&run);
    let sessions = std::fs::read_dir(&fixture.sessions)
        .unwrap()
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    assert!(
        sessions.is_empty(),
        "the harness must not record sessions in the caller's session root"
    );
}

// This endpoint is owned by the test process, not the eval harness. Its response
// depends on the actual prompt and selected wire model; suites carry no replies.
#[derive(Clone, Copy)]
enum ModelBehavior {
    Reply,
    MissingUsage,
    Refusal,
    Stall,
    Flood,
    Redirect,
}

struct ModelServer {
    base_url: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ModelServer {
    fn start(behavior: ModelBehavior) -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&requests);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = std::sync::Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                // A non-blocking listener can hand back a non-blocking socket on
                // macOS, in which case the first read returns WouldBlock before
                // the client's request arrives. Treating that as fatal closed the
                // connection mid-send and the client reported "connection closed
                // before message completed"; make the accepted socket blocking so
                // the read deadline, not scheduling luck, decides.
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let (headers, body) = loop {
                    let mut chunk = [0u8; 4096];
                    let read = match socket.read(&mut chunk) {
                        Ok(n) if n > 0 => n,
                        _ => break (String::new(), serde_json::Value::Null),
                    };
                    bytes.extend_from_slice(&chunk[..read]);
                    assert!(bytes.len() <= 512 * 1024);
                    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&bytes[..end + 4]).into_owned();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        let body = serde_json::from_slice(&bytes[end + 4..end + 4 + length])
                            .unwrap_or_default();
                        break (headers, body);
                    }
                };
                if headers.is_empty() {
                    continue;
                }
                captured
                    .lock()
                    .unwrap()
                    .push((headers.clone(), body.clone()));
                if matches!(behavior, ModelBehavior::Stall) {
                    while !stopped.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    break;
                }
                if matches!(behavior, ModelBehavior::Redirect) {
                    // Would be an unauthorized second inference request if followed.
                    let response = "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/v1/chat/completions\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(response.as_bytes());
                    continue;
                }
                let model = body["model"].as_str().unwrap_or("NO-MODEL");
                let prompt = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
                    .unwrap()["content"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let text = if matches!(behavior, ModelBehavior::Flood) {
                    "x".repeat(128 * 1024)
                } else {
                    format!("{model}: {}", prompt.to_uppercase())
                };
                let first = serde_json::json!({"id":"external-model", "model": model,
                    "choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":null}]});
                let mut last = serde_json::json!({"id":"external-model", "model":model,
                    "choices":[{"index":0,"delta":{},"finish_reason":if matches!(behavior, ModelBehavior::Refusal) {"content_filter"} else {"stop"}}]});
                if !matches!(behavior, ModelBehavior::MissingUsage) {
                    last["usage"] = serde_json::json!({"prompt_tokens":20,"completion_tokens":7,"total_tokens":27});
                }
                let body = format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n");
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = socket.write_all(response.as_bytes());
            }
        });
        Self {
            base_url,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn captured(&self) -> Vec<(String, serde_json::Value)> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ModelServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn write_profile(
    fixture: &Fixture,
    server: &ModelServer,
    model: &str,
    known_prices: bool,
) -> PathBuf {
    let mut profile = serde_json::json!({
        "schema":"octet-eval-model-1", "base_url":server.base_url, "model":model,
        "api_key":"eval-operator-private-token", "context_window":32768,"max_output_tokens":64
    });
    if known_prices {
        profile["pricing"] = serde_json::json!({"input":1000000,"output":2000000,"cache_read":1000000,"cache_write_5m":1000000});
    }
    let path = fixture.workspace.join(format!("{model}-profile.json"));
    octet_agent::secure_fs::write_private_atomic(
        &path,
        serde_json::to_string(&profile).unwrap().as_bytes(),
        16 * 1024,
    )
    .unwrap();
    path
}

fn model_run(
    fixture: &Fixture,
    suite: &std::path::Path,
    profile: &std::path::Path,
    args: &[&str],
) -> Output {
    let run = fixture
        .command()
        .env("OPENAI_API_KEY", "ambient-secret-must-not-leak")
        .env("OPENAI_BASE_URL", "http://127.0.0.1:1/v1/")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("OCTET_SYSTEM_PROMPT", "ambient-instructions-must-not-leak")
        .env("OCTET_MODEL", "ambient-model-must-not-be-used")
        .args(["eval", "run"])
        .arg(suite)
        .arg("--artifact-dir")
        .arg(&fixture.artifacts)
        .arg("--model-profile")
        .arg(profile)
        .args(args)
        .output()
        .unwrap();
    // Test-log diagnostics only, and opt-in: an intermittent isolated failure is
    // otherwise indistinguishable from a fixture gap.
    if std::env::var_os("OCTET_EVAL_TEST_DIAGNOSTICS").is_some() {
        eprintln!(
            "eval child status={:?}\nstderr:\n{}",
            run.status,
            String::from_utf8_lossy(&run.stderr)
        );
    }
    run
}

#[test]
fn eval_model_profile_selects_real_runtime_model_prompt_reply_and_private_credentials() {
    let fixture = Fixture::new();
    let server = ModelServer::start(ModelBehavior::Reply);
    // Neither the caller's config, context nor credentials may enter the child.
    std::fs::write(
        fixture.home.join(".octet/config.toml"),
        "system_prompt = 'ambient-home-instructions'\n",
    )
    .unwrap();
    std::fs::write(
        fixture.workspace.join("AGENTS.md"),
        "ambient-workspace-instructions",
    )
    .unwrap();
    let ambient = fixture.home.join(".octet/credentials/custom.json");
    octet_agent::secure_fs::write_private_atomic(&ambient, b"ambient-invalid-registry", 1024)
        .unwrap();
    for model in ["local-alpha", "local-beta"] {
        let profile = write_profile(&fixture, &server, model, true);
        let original_profile = std::fs::read(&profile).unwrap();
        let suite = fixture.write_suite(model, &serde_json::json!({"schema":"octet-eval-suite-1", "cases":[
            {"id":"literal", "prompt":"@never-read-this-file --model forged", "expect":{"equals":format!("{model}: @NEVER-READ-THIS-FILE --MODEL FORGED")},"budgets":{"max_cost_microdollars":200000}},
            {"id":"independent", "prompt":"second prompt", "expect":{"equals":format!("{model}: SECOND PROMPT")}}
        ]}).to_string());
        assert_success(&model_run(&fixture, &suite, &profile, &[]));
        let report = fixture.latest_report();
        assert_eq!(report["backend"], "model");
        assert_eq!(report["model"], format!("custom/eval/{model}"));
        assert_eq!(report["pricing"], "operator-declared");
        assert_eq!(report["totals"]["passed"], 2, "{report}");
        assert_eq!(report["totals"]["cost_microdollars_total"], 68);
        assert_eq!(report["totals"]["usage_uncertain_cases"], 0);
        assert_eq!(report["isolation"]["credentials"], "operator-profile");
        assert_eq!(report["isolation"]["fixture_base_url"], "");
        for case in report["cases"].as_array().unwrap() {
            assert_eq!(case["fixture_requests"], 0);
            assert_eq!(case["usage_records"], 1);
            assert_eq!(case["input_tokens"], 20);
            assert_eq!(case["output_tokens"], 7);
        }
        assert_eq!(std::fs::read(&profile).unwrap(), original_profile);
        let run = fixture.latest_run_dir().unwrap();
        for file in ["report.json", "report.txt", "runs.jsonl"] {
            let path = run.join(file);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("eval-operator-private-token"));
            assert!(!text.contains("ambient-secret"));
            assert!(!text.contains(&server.base_url));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
                    0
                );
                assert_eq!(
                    std::fs::metadata(&run).unwrap().permissions().mode() & 0o077,
                    0
                );
            }
        }
    }
    let requests = server.captured();
    assert_eq!(
        requests.len(),
        4,
        "no discovery, retries or hidden extra turns"
    );
    for (index, (headers, body)) in requests.iter().enumerate() {
        assert!(
            headers.starts_with("POST /v1/chat/completions "),
            "{headers}"
        );
        assert!(headers
            .to_ascii_lowercase()
            .contains("authorization: bearer eval-operator-private-token"));
        assert_eq!(
            body["model"],
            if index < 2 {
                "local-alpha"
            } else {
                "local-beta"
            }
        );
        assert_eq!(body["max_completion_tokens"], 64);
        assert!(body
            .get("tools")
            .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)));
        let encoded = body.to_string();
        assert!(!encoded.contains("ambient-"));
        if index % 2 == 1 {
            assert!(!encoded.contains("never-read-this-file"));
        }
    }
    assert_eq!(
        std::fs::read(&ambient).unwrap(),
        b"ambient-invalid-registry"
    );
    assert_eq!(std::fs::read_dir(&fixture.sessions).unwrap().count(), 0);
}

#[test]
fn eval_model_cost_budgets_refuse_before_inference_when_unknown_or_insufficient() {
    let fixture = Fixture::new();
    let server = ModelServer::start(ModelBehavior::Reply);
    let suite = fixture.write_suite("budget", r#"{"schema":"octet-eval-suite-1","cases":[{"id":"budget","prompt":"chargeable","budgets":{"max_cost_microdollars":1}}]}"#);
    for known_prices in [false, true] {
        let profile = write_profile(&fixture, &server, "local-budget", known_prices);
        assert_success(&model_run(&fixture, &suite, &profile, &[]));
        let report = fixture.latest_report();
        assert_eq!(report["totals"]["failed"], 1, "{report}");
        assert_eq!(report["cases"][0]["input_tokens"], 0);
        assert_eq!(report["cases"][0]["cost_microdollars"], 0);
        assert_eq!(
            server.captured().len(),
            0,
            "hard budget must refuse before POST"
        );
        if !known_prices {
            assert!(report["cases"][0]["failure"]
                .as_str()
                .unwrap()
                .contains("pricing is unknown"));
        }
    }
}

#[test]
fn eval_model_unknown_prices_and_missing_usage_produce_uncertain_cost_deltas() {
    let fixture = Fixture::new();
    let server = ModelServer::start(ModelBehavior::Reply);
    let suite = fixture.write_suite("deltas", r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"hello","expect":{"equals":"local-delta: HELLO"}}]}"#);
    let profile = write_profile(&fixture, &server, "local-delta", true);
    assert_success(&model_run(&fixture, &suite, &profile, &[]));
    let baseline_path = fixture.latest_run_dir().unwrap().join("report.json");
    let profile = write_profile(&fixture, &server, "local-delta", false);
    assert_success(&model_run(
        &fixture,
        &suite,
        &profile,
        &["--baseline", baseline_path.to_str().unwrap()],
    ));
    let report = fixture.latest_report();
    assert_eq!(report["cases"][0]["passed"], true);
    assert_eq!(report["cases"][0]["usage_uncertain"], true);
    assert_eq!(report["cases"][0]["input_tokens"], 20);
    assert_eq!(report["pricing"], "unknown");
    assert_eq!(report["deltas"]["usage_uncertain_cases_delta"], 1);
    assert_eq!(report["deltas"]["cost_delta_exact"], false);
    assert_eq!(report["deltas"]["cost_microdollars_total_delta"], -34);
    assert_eq!(report["deltas"]["pass_rate_pp_delta"], 0.0);
    let missing = ModelServer::start(ModelBehavior::MissingUsage);
    let profile = write_profile(&fixture, &missing, "local-delta", true);
    assert_success(&model_run(&fixture, &suite, &profile, &[]));
    assert_eq!(fixture.latest_report()["cases"][0]["usage_uncertain"], true);
    let ceiling = fixture.write_suite("missing-budget", r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"hello","budgets":{"max_cost_microdollars":200000}}]}"#);
    assert_success(&model_run(&fixture, &ceiling, &profile, &[]));
    assert_eq!(fixture.latest_report()["cases"][0]["passed"], false);
}

#[test]
fn eval_model_failed_terminal_response_preserves_usage_and_cost() {
    let fixture = Fixture::new();
    let server = ModelServer::start(ModelBehavior::Refusal);
    let profile = write_profile(&fixture, &server, "local-refusal", true);
    let suite = fixture.write_suite(
        "failure",
        r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"hello"}]}"#,
    );
    assert_success(&model_run(&fixture, &suite, &profile, &[]));
    let case = fixture.latest_report()["cases"][0].clone();
    assert_eq!(case["passed"], false, "{case}");
    assert_eq!(case["input_tokens"], 20);
    assert_eq!(case["output_tokens"], 7);
    assert_eq!(case["cost_microdollars"], 34);
    assert_eq!(case["usage_records"], 1);
    assert_eq!(case["usage_uncertain"], false);
    assert_eq!(server.captured().len(), 1);
}

#[test]
fn eval_model_walltime_output_and_redirects_are_bounded_without_false_zero_usage() {
    for behavior in [
        ModelBehavior::Stall,
        ModelBehavior::Flood,
        ModelBehavior::Redirect,
    ] {
        let fixture = Fixture::new();
        let server = ModelServer::start(behavior);
        let profile = write_profile(&fixture, &server, "local-bounded", true);
        let suite = fixture.write_suite(
            "bounds",
            r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"hello"}]}"#,
        );
        let started = std::time::Instant::now();
        assert_success(&model_run(
            &fixture,
            &suite,
            &profile,
            &["--case-timeout-ms", "4000", "--max-output-bytes", "4096"],
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        let case = fixture.latest_report()["cases"][0].clone();
        assert_eq!(case["passed"], false, "{case}");
        assert_eq!(case["usage_uncertain"], true, "{case}");
        assert_eq!(server.captured().len(), 1);
        if matches!(behavior, ModelBehavior::Stall) {
            assert!(case["failure"].as_str().unwrap().contains("wall-time"));
        }
        if matches!(behavior, ModelBehavior::Flood) {
            assert!(case["failure"].as_str().unwrap().contains("output capture"));
        }
    }
}

#[test]
fn eval_model_profile_rejects_suite_scripts_remote_routes_and_unbounded_or_nonprivate_files() {
    let fixture = Fixture::new();
    let server = ModelServer::start(ModelBehavior::Reply);
    let profile = write_profile(&fixture, &server, "local-admission", true);
    let suite = fixture.write_suite(
        "plain",
        r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"hello"}]}"#,
    );
    let scripted = fixture.write_suite("scripted", r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"hello","fixture":{"response":"fake"}}]}"#);
    assert!(!model_run(&fixture, &scripted, &profile, &[])
        .status
        .success());
    let invalid_baseline = fixture.write_suite("bad-baseline", "{}");
    assert!(!model_run(
        &fixture,
        &suite,
        &profile,
        &["--baseline", invalid_baseline.to_str().unwrap()]
    )
    .status
    .success());
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
    for (field, value) in [
        ("base_url", serde_json::json!("https://api.openai.com/v1/")),
        ("base_url", serde_json::json!("http://localhost:8000/v1/")),
        (
            "base_url",
            serde_json::json!("http://secret@127.0.0.1:8000/v1/"),
        ),
        (
            "base_url",
            serde_json::json!("http://127.0.0.1:8000/v1/?key=secret"),
        ),
        ("max_output_tokens", serde_json::json!(0)),
        ("api_key_env", serde_json::json!("OPENAI_API_KEY")),
    ] {
        let mut invalid = original.clone();
        invalid[field] = value;
        std::fs::write(&profile, invalid.to_string()).unwrap();
        assert!(
            !model_run(&fixture, &suite, &profile, &[]).status.success(),
            "{field}"
        );
    }
    std::fs::write(&profile, vec![b'x'; 16 * 1024 + 1]).unwrap();
    assert!(!model_run(&fixture, &suite, &profile, &[]).status.success());
    std::fs::write(&profile, original.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let link = fixture.workspace.join("profile-link.json");
        symlink(&profile, &link).unwrap();
        assert!(!model_run(&fixture, &suite, &link, &[]).status.success());
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!model_run(&fixture, &suite, &profile, &[]).status.success());
    }
    assert!(server.captured().is_empty());
    assert_eq!(std::fs::read_dir(&fixture.artifacts).unwrap().count(), 0);
}
