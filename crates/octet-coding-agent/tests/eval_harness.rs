//! Behavioral coverage for the isolated, fixture-backed eval harness (parity
//! row 5.11).
//!
//! Every test runs the real `octet eval` command in a process boundary with an
//! isolated HOME/workspace/session root and asserts from the artifact it wrote.
//! The harness injects its own loopback provider, so no test touches a live or
//! paid endpoint.

use std::path::PathBuf;
use std::process::{Command, Output};

const PASSING_TEXT: &str = "hello from the eval fixture";

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

fn number<'a>(value: &'a serde_json::Value, key: &str) -> f64 {
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
    let baseline_run = fixture.run(&["eval", "run", baseline_suite.to_str().unwrap(), "--artifact-dir", &artifacts]);
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
    assert!(cases[0]["cost_microdollars"].as_u64().is_some(), "{cases:?}");
    assert_eq!(cases[1]["passed"], false);
    assert!(
        cases[1]["failure"].as_str().unwrap_or_default().contains("definitely-not-present"),
        "{cases:?}"
    );

    // The run directory holds the incremental records and the text report too.
    let report_dir = fixture
        .latest_run_dir()
        .expect("the newest artifact run directory");
    let runs = std::fs::read_to_string(report_dir.join("runs.jsonl")).unwrap();
    assert_eq!(runs.lines().count(), 2, "{runs}");
    assert!(runs.lines().all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()));
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
        .args(["eval", "run", suite.to_str().unwrap(), "--artifact-dir", &artifacts])
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
            stderr.contains("only runs scripted loopback fixtures"),
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
    assert!(stderr_of(&output).contains("octet-eval-suite-1"), "{}", stderr_of(&output));

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
