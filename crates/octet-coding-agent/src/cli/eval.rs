#![allow(missing_docs)]

//! Isolated, fixture-backed evaluation harness (parity row 5.11).
//!
//! The harness is a *bounded measurement rig*, not a second agent runner:
//!
//! * **Own provider injection.** Every case runs against a loopback HTTP
//!   fixture the harness starts itself inside its own temporary `HOME`. The
//!   octet child process is spawned with `env_clear()`, so no ambient
//!   credential, proxy or provider endpoint is visible to it, and the suite
//!   schema rejects any field that could name a remote endpoint. A live or paid
//!   provider call is therefore impossible by construction, not by policy.
//! * **Own artifact directory.** Each run writes a private run directory with
//!   `report.json`, incrementally appended `runs.jsonl` and `report.txt`.
//! * **Pass / latency / cost recording.** Per case: pass or fail against the
//!   declared expectation and budgets, wall-clock latency, provider usage, the
//!   session's cost total and whether that cost is exact or uncertain. A run can
//!   be compared with an earlier report (`--baseline`) to record
//!   candidate-minus-baseline deltas.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Subcommand;
use serde::{Deserialize, Serialize};

/// The suite schema the harness accepts.
pub const EVAL_SUITE_SCHEMA: &str = "octet-eval-suite-1";
/// The report schema the harness writes.
pub const EVAL_RUN_SCHEMA: &str = "octet-eval-run-1";
/// The injected model id; the harness owns the endpoint behind it.
pub const EVAL_FIXTURE_MODEL: &str = "eval-fixture";
/// The text a case sees when its suite does not script a response.
pub const EVAL_FIXTURE_DEFAULT_RESPONSE: &str = "octet eval fixture response";

#[derive(Clone, Debug, Subcommand)]
pub enum EvalCommand {
    /// Run one isolated suite and write its artifact report.
    Run {
        /// Suite document (`octet-eval-suite-1`).
        suite: PathBuf,
        /// Artifact directory; a private run directory is created inside it.
        #[arg(long, value_name = "DIR")]
        artifact_dir: Option<PathBuf>,
        /// Compare against an earlier `report.json` and record the deltas.
        #[arg(long, value_name = "REPORT.json")]
        baseline: Option<PathBuf>,
    },
}

pub fn run(command: EvalCommand, cwd: &Path) -> anyhow::Result<()> {
    match command {
        EvalCommand::Run {
            suite,
            artifact_dir,
            baseline,
        } => run_suite(&suite, artifact_dir.as_deref(), baseline.as_deref(), cwd),
    }
}

/// One scripted fixture reply. The harness never talks to a real provider, so a
/// case can only choose *what its own loopback fixture answers*.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureReply {
    /// Assistant text the fixture streams.
    #[serde(default = "default_fixture_response")]
    pub response: String,
    /// Prompt tokens the fixture reports.
    #[serde(default = "default_input_tokens")]
    pub input_tokens: u64,
    /// Completion tokens the fixture reports.
    #[serde(default = "default_output_tokens")]
    pub output_tokens: u64,
}

impl Default for FixtureReply {
    fn default() -> Self {
        Self {
            response: default_fixture_response(),
            input_tokens: default_input_tokens(),
            output_tokens: default_output_tokens(),
        }
    }
}

fn default_fixture_response() -> String {
    EVAL_FIXTURE_DEFAULT_RESPONSE.to_owned()
}

fn default_input_tokens() -> u64 {
    12
}

fn default_output_tokens() -> u64 {
    6
}

/// What a case asserts about the assistant's final answer.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    /// The answer must contain this substring.
    pub contains: Option<String>,
    /// The answer must equal this string exactly (trimmed).
    pub equals: Option<String>,
    /// The answer must not contain this substring.
    pub not_contains: Option<String>,
}

impl Expectation {
    fn evaluate(&self, output: &str) -> Option<String> {
        let trimmed = output.trim();
        if let Some(expected) = &self.equals {
            if trimmed != expected.trim() {
                return Some(format!("expected {expected:?}, got {trimmed:?}"));
            }
        }
        if let Some(needle) = &self.contains {
            if !output.contains(needle) {
                return Some(format!("expected the answer to contain {needle:?}"));
            }
        }
        if let Some(needle) = &self.not_contains {
            if output.contains(needle) {
                return Some(format!("expected the answer not to contain {needle:?}"));
            }
        }
        None
    }
}

/// Optional per-case ceilings. A case that exceeds one fails, with the
/// measured value recorded in its artifact record.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    /// Maximum wall-clock latency in milliseconds.
    pub max_latency_ms: Option<u64>,
    /// Maximum recorded cost in microdollars.
    pub max_cost_microdollars: Option<u64>,
}

/// `deny_unknown_fields` is the isolation boundary: a suite cannot declare a
/// provider, a base URL, an api key or any other way to reach a live endpoint.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    /// Stable case identity, unique within the suite.
    pub id: String,
    /// The prompt the isolated agent run receives.
    pub prompt: String,
    /// What the case asserts.
    #[serde(default)]
    pub expect: Expectation,
    /// What the case's own loopback fixture answers.
    #[serde(default)]
    pub fixture: FixtureReply,
    /// Optional pass ceilings.
    #[serde(default)]
    pub budgets: Budgets,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalSuite {
    /// Must be [`EVAL_SUITE_SCHEMA`].
    pub schema: String,
    /// Optional suite label recorded in the report.
    #[serde(default)]
    pub name: Option<String>,
    /// At least one case.
    pub cases: Vec<EvalCase>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalCaseRecord {
    pub id: String,
    pub passed: bool,
    /// Wall-clock latency of the isolated run, in milliseconds.
    pub latency_ms: u64,
    /// The isolated session's cost total, in microdollars.
    pub cost_microdollars: u64,
    /// Whether that cost is a known subtotal (unknown provider usage).
    pub usage_uncertain: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Provider requests the loopback fixture answered for this case.
    pub fixture_requests: u64,
    /// Why the case failed, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EvalTotals {
    pub cases: u64,
    pub passed: u64,
    pub failed: u64,
    /// Pass rate in percentage points (0..=100).
    pub pass_rate_pp: f64,
    pub latency_ms_total: u64,
    pub latency_ms_mean: f64,
    pub cost_microdollars_total: u64,
    pub input_tokens_total: u64,
    pub output_tokens_total: u64,
}

/// What the run was isolated from, recorded so an artifact is self-describing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalIsolation {
    /// Always `injected-fixture`: the harness supplies the only credentials.
    pub credentials: String,
    /// Always `loopback-only`.
    pub network: String,
    /// The loopback base URL every case ran against.
    pub fixture_base_url: String,
    /// Always `stripped`: the child sees no ambient environment.
    pub ambient_environment: String,
    /// Always `true`.
    pub offline: bool,
}

/// Candidate-minus-baseline deltas, recorded when `--baseline` is supplied.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalDeltas {
    pub baseline_cases: u64,
    pub baseline_pass_rate_pp: f64,
    pub pass_rate_pp_delta: f64,
    pub baseline_latency_ms_mean: f64,
    pub latency_ms_mean_delta: f64,
    pub baseline_cost_microdollars_total: u64,
    pub cost_microdollars_total_delta: i64,
    pub baseline_input_tokens_total: u64,
    pub input_tokens_total_delta: i64,
    pub baseline_output_tokens_total: u64,
    pub output_tokens_total_delta: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalRunReport {
    pub schema: String,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suite: Option<String>,
    pub isolation: EvalIsolation,
    pub cases: Vec<EvalCaseRecord>,
    pub totals: EvalTotals,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deltas: Option<EvalDeltas>,
}

fn run_suite(
    suite_path: &Path,
    artifact_dir: Option<&Path>,
    baseline: Option<&Path>,
    cwd: &Path,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(suite_path)
        .map_err(|error| anyhow::anyhow!("could not read eval suite {}: {error}", suite_path.display()))?;
    let suite: EvalSuite = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "eval suite {} is not a valid {EVAL_SUITE_SCHEMA} document ({error}); the harness only runs scripted loopback fixtures, so a suite cannot name a provider, endpoint or key",
            suite_path.display()
        )
    })?;
    if suite.schema != EVAL_SUITE_SCHEMA {
        anyhow::bail!(
            "eval suite {} declares schema {:?}, expected {EVAL_SUITE_SCHEMA}",
            suite_path.display(),
            suite.schema
        );
    }
    if suite.cases.is_empty() {
        anyhow::bail!("eval suite {} has no cases", suite_path.display());
    }
    let mut seen = std::collections::HashSet::new();
    for case in &suite.cases {
        if !seen.insert(case.id.clone()) {
            anyhow::bail!("eval suite {} repeats case id {:?}", suite_path.display(), case.id);
        }
    }

    let artifact_root = artifact_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.join(".eval"));
    std::fs::create_dir_all(&artifact_root)?;
    let run_id = format!("{}-{}", timestamp(), std::process::id());
    let run_dir = artifact_root.join(&run_id);
    octet_agent::secure_fs::create_private_directory_all(&run_dir)?;

    // The harness owns the child's entire environment: one private HOME with one
    // injected loopback credential, no inherited variables at all.
    let home = tempfile::Builder::new()
        .prefix("octet-eval-home-")
        .tempdir()?;
    let session_root = tempfile::Builder::new()
        .prefix("octet-eval-sessions-")
        .tempdir()?;
    let workspace = tempfile::Builder::new()
        .prefix("octet-eval-workspace-")
        .tempdir()?;
    let credentials = home.path().join(".octet/credentials");
    octet_agent::secure_fs::create_private_directory_all(&credentials)?;

    let executable = std::env::current_exe()
        .map_err(|error| anyhow::anyhow!("could not locate the running octet binary: {error}"))?;

    let mut runs = std::fs::File::create(run_dir.join("runs.jsonl"))?;
    let mut records = Vec::new();
    let mut fixture_base_url = String::new();
    for case in &suite.cases {
        let fixture = LoopbackFixture::start(&case.fixture.response, &case.fixture)?;
        fixture_base_url = fixture.base_url.clone();
        write_injected_credential(&credentials.join("custom.json"), &fixture.base_url)?;
        let started = Instant::now();
        let outcome = run_case(
            &executable,
            home.path(),
            session_root.path(),
            workspace.path(),
            case,
        );
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (usage, failure) = match outcome {
            Ok(usage) => (usage, None),
            Err(error) => (CaseUsage::default(), Some(format!("{error:#}"))),
        };
        let mut failure = failure.or_else(|| case.expect.evaluate(&usage.output));
        if let Some(limit) = case.budgets.max_latency_ms {
            if failure.is_none() && latency_ms > limit {
                failure = Some(format!("latency {latency_ms}ms exceeds the {limit}ms budget"));
            }
        }
        if let Some(limit) = case.budgets.max_cost_microdollars {
            if failure.is_none() && usage.cost_microdollars > limit {
                failure = Some(format!(
                    "cost {}µ$ exceeds the {limit}µ$ budget",
                    usage.cost_microdollars
                ));
            }
        }
        let record = EvalCaseRecord {
            id: case.id.clone(),
            passed: failure.is_none(),
            latency_ms,
            cost_microdollars: usage.cost_microdollars,
            usage_uncertain: usage.usage_uncertain,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            fixture_requests: fixture.requests(),
            failure,
        };
        // Write continuously: a killed run still leaves its evidence behind.
        serde_json::to_writer(&mut runs, &record)?;
        runs.write_all(b"\n")?;
        runs.flush()?;
        records.push(record);
    }
    drop(runs);

    let totals = totals_of(&records);
    let deltas = match baseline {
        Some(path) => Some(deltas_against(path, &totals)?),
        None => None,
    };
    let report = EvalRunReport {
        schema: EVAL_RUN_SCHEMA.to_owned(),
        run_id: run_id.clone(),
        suite: suite.name.clone(),
        isolation: EvalIsolation {
            credentials: "injected-fixture".to_owned(),
            network: "loopback-only".to_owned(),
            fixture_base_url,
            ambient_environment: "stripped".to_owned(),
            offline: true,
        },
        cases: records,
        totals,
        deltas,
    };
    std::fs::write(
        run_dir.join("report.json"),
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    std::fs::write(run_dir.join("report.txt"), render_report(&report))?;

    crate::output::stdout_line(render_report(&report));
    crate::output::stdout_line(format!("Artifacts: {}", run_dir.display()));
    // The harness is a measurement tool: a failing *case* is an observation, so
    // the command succeeds and the failure lives in the artifact. Only
    // infrastructure failures return an error.
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct CaseUsage {
    output: String,
    cost_microdollars: u64,
    usage_uncertain: bool,
    input_tokens: u64,
    output_tokens: u64,
}

fn run_case(
    executable: &Path,
    home: &Path,
    session_root: &Path,
    workspace: &Path,
    case: &EvalCase,
) -> anyhow::Result<CaseUsage> {
    let output = Command::new(executable)
        .current_dir(workspace)
        // The child sees nothing ambient: no keys, no proxies, no HOME state.
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("PWD", workspace)
        .env("TERM", "dumb")
        .env("LANG", "C.UTF-8")
        .args(["--offline", "--no-context-files", "--no-tools"])
        .arg("--workspace")
        .arg(workspace)
        .arg("--session-dir")
        .arg(session_root)
        .args(["--color", "never", "--model"])
        .arg(format!("custom/{EVAL_FIXTURE_MODEL}"))
        .arg("--print")
        .arg(&case.prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| anyhow::anyhow!("could not start the isolated octet run: {error}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "the isolated run exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut usage = CaseUsage {
        output: String::from_utf8_lossy(&output.stdout).into_owned(),
        ..CaseUsage::default()
    };
    if let Some(transcript) = newest_transcript(session_root) {
        // The harness reads its own child's session for accounting, then the
        // temporary session root disappears with the run.
        if let Ok(session) = octet_agent::Session::open_read_only(transcript) {
            usage.cost_microdollars = session.total_cost_microdollars();
            usage.usage_uncertain = session.has_uncertain_usage();
            for record in session.usage_records() {
                usage.input_tokens = usage.input_tokens.saturating_add(record.usage.input_tokens);
                usage.output_tokens = usage.output_tokens.saturating_add(record.usage.output_tokens);
            }
        }
    }
    Ok(usage)
}

fn newest_transcript(root: &Path) -> Option<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
                continue;
            };
            if newest.as_ref().is_none_or(|(current, _)| modified >= *current) {
                newest = Some((modified, path));
            }
        }
    }
    newest.map(|(_, path)| path)
}

/// The one credential the child can see: this run's own loopback fixture.
fn write_injected_credential(path: &Path, base_url: &str) -> anyhow::Result<()> {
    let record = serde_json::json!({
        "base_url": base_url,
        "api_key": "",
        "api_name": EVAL_FIXTURE_MODEL,
        "headers": [],
        "auto_discover": false,
        "models": [{"api_name": EVAL_FIXTURE_MODEL}],
    });
    octet_agent::secure_fs::write_private_atomic(
        path,
        format!("{record}\n").as_bytes(),
        1024 * 1024,
    )?;
    Ok(())
}

fn totals_of(records: &[EvalCaseRecord]) -> EvalTotals {
    let cases = records.len() as u64;
    let passed = records.iter().filter(|record| record.passed).count() as u64;
    let latency_ms_total = records.iter().map(|record| record.latency_ms).sum::<u64>();
    let cost_microdollars_total = records
        .iter()
        .map(|record| record.cost_microdollars)
        .sum::<u64>();
    EvalTotals {
        cases,
        passed,
        failed: cases - passed,
        pass_rate_pp: if cases == 0 {
            0.0
        } else {
            (passed as f64) * 100.0 / (cases as f64)
        },
        latency_ms_total,
        latency_ms_mean: if cases == 0 {
            0.0
        } else {
            latency_ms_total as f64 / cases as f64
        },
        cost_microdollars_total,
        input_tokens_total: records.iter().map(|record| record.input_tokens).sum(),
        output_tokens_total: records.iter().map(|record| record.output_tokens).sum(),
    }
}

fn deltas_against(path: &Path, totals: &EvalTotals) -> anyhow::Result<EvalDeltas> {
    let bytes = std::fs::read(path)
        .map_err(|error| anyhow::anyhow!("could not read baseline {}: {error}", path.display()))?;
    let baseline: EvalRunReport = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "baseline {} is not an {EVAL_RUN_SCHEMA} artifact: {error}",
            path.display()
        )
    })?;
    let base = &baseline.totals;
    Ok(EvalDeltas {
        baseline_cases: base.cases,
        baseline_pass_rate_pp: base.pass_rate_pp,
        pass_rate_pp_delta: totals.pass_rate_pp - base.pass_rate_pp,
        baseline_latency_ms_mean: base.latency_ms_mean,
        latency_ms_mean_delta: totals.latency_ms_mean - base.latency_ms_mean,
        baseline_cost_microdollars_total: base.cost_microdollars_total,
        cost_microdollars_total_delta: i64::try_from(totals.cost_microdollars_total)
            .unwrap_or(i64::MAX)
            - i64::try_from(base.cost_microdollars_total).unwrap_or(i64::MAX),
        baseline_input_tokens_total: base.input_tokens_total,
        input_tokens_total_delta: i64::try_from(totals.input_tokens_total).unwrap_or(i64::MAX)
            - i64::try_from(base.input_tokens_total).unwrap_or(i64::MAX),
        baseline_output_tokens_total: base.output_tokens_total,
        output_tokens_total_delta: i64::try_from(totals.output_tokens_total).unwrap_or(i64::MAX)
            - i64::try_from(base.output_tokens_total).unwrap_or(i64::MAX),
    })
}

fn render_report(report: &EvalRunReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Eval {} — {} case(s), {} passed, {} failed ({:.1}%)\n",
        report.suite.as_deref().unwrap_or(&report.run_id),
        report.totals.cases,
        report.totals.passed,
        report.totals.failed,
        report.totals.pass_rate_pp
    ));
    for record in &report.cases {
        out.push_str(&format!(
            "  {} {} — {}ms, {}µ$, {} in / {} out{}",
            if record.passed { "PASS" } else { "FAIL" },
            record.id,
            record.latency_ms,
            record.cost_microdollars,
            record.input_tokens,
            record.output_tokens,
            record
                .failure
                .as_ref()
                .map(|failure| format!(" ({failure})"))
                .unwrap_or_default()
        ));
        if record.usage_uncertain {
            out.push_str(" [usage uncertain]");
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "  latency mean {:.1}ms, cost total {}µ$ ({} injected, {}, offline)\n",
        report.totals.latency_ms_mean,
        report.totals.cost_microdollars_total,
        report.isolation.credentials,
        report.isolation.network
    ));
    if let Some(deltas) = &report.deltas {
        out.push_str(&format!(
            "  vs baseline ({} case(s)): pass {}{:+.1}pp, latency mean {:+.1}ms, cost total {:+}µ$, tokens {:+}/{:+}\n",
            deltas.baseline_cases,
            deltas.baseline_pass_rate_pp,
            deltas.pass_rate_pp_delta,
            deltas.latency_ms_mean_delta,
            deltas.cost_microdollars_total_delta,
            deltas.input_tokens_total_delta,
            deltas.output_tokens_total_delta
        ));
    }
    out
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{:06}", now.as_secs(), now.subsec_micros())
}

/// The harness's own provider: a loopback OpenAI-compatible endpoint that answers
/// one scripted streaming completion. Nothing here can reach a remote host.
struct LoopbackFixture {
    base_url: String,
    requests: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LoopbackFixture {
    fn start(text: &str, reply: &FixtureReply) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| anyhow::anyhow!("could not bind the eval fixture: {error}"))?;
        listener.set_nonblocking(true)?;
        let base_url = format!("http://{}/v1/", listener.local_addr()?);
        let body = fixture_sse_body(text, reply);
        let requests = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counted = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                let _ = socket.set_nonblocking(false);
                let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
                let request = read_request(&mut socket);
                let (content_type, body) = if request.starts_with("GET /v1/models ") {
                    (
                        "application/json",
                        format!("{{\"data\":[{{\"id\":\"{EVAL_FIXTURE_MODEL}\"}}]}}"),
                    )
                } else {
                    counted.fetch_add(1, Ordering::SeqCst);
                    ("text/event-stream", body.clone())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                if socket.write_all(response.as_bytes()).is_ok() {
                    let _ = socket.flush();
                }
            }
        });
        Ok(Self {
            base_url,
            requests,
            stop,
            worker: Some(worker),
        })
    }

    fn requests(&self) -> u64 {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for LoopbackFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_request(socket: &mut std::net::TcpStream) -> String {
    let mut request = Vec::new();
    let header_end = loop {
        let mut bytes = [0u8; 1024];
        match socket.read(&mut bytes) {
            Ok(0) | Err(_) => break 0,
            Ok(read) => request.extend_from_slice(&bytes[..read]),
        }
        if request.len() > 512 * 1024 {
            return String::new();
        }
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    if header_end == 0 {
        return String::new();
    }
    let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while request.len() < header_end + length {
        let mut bytes = [0u8; 1024];
        match socket.read(&mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&bytes[..read]),
        }
    }
    headers
}

fn fixture_sse_body(text: &str, reply: &FixtureReply) -> String {
    let text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned());
    format!(
        concat!(
            "data: {{\"id\":\"eval-fixture\",\"model\":\"{model}\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":{text}}},\"finish_reason\":null}}]}}\n\n",
            "data: {{\"id\":\"eval-fixture\",\"model\":\"{model}\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":{input},\"completion_tokens\":{output},\"total_tokens\":{total}}}}}\n\n",
            "data: [DONE]\n\n"
        ),
        model = EVAL_FIXTURE_MODEL,
        text = text,
        input = reply.input_tokens,
        output = reply.output_tokens,
        total = reply.input_tokens + reply.output_tokens,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite(json: &str) -> anyhow::Result<EvalSuite> {
        Ok(serde_json::from_str(json)?)
    }

    #[test]
    fn a_suite_cannot_name_a_provider_endpoint_or_key() {
        // The isolation boundary is the schema: the only endpoint a case may
        // influence is its own scripted fixture reply.
        for forbidden in [
            r#"{"schema":"octet-eval-suite-1","provider":{"base_url":"https://api.openai.com/v1"},"cases":[]}"#,
            r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"p","api_key":"sk-live"}]}"#,
            r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"p","base_url":"http://example.com"}]}"#,
        ] {
            assert!(suite(forbidden).is_err(), "{forbidden}");
        }
        let honest = suite(
            r#"{"schema":"octet-eval-suite-1","cases":[{"id":"a","prompt":"p","fixture":{"response":"ok","input_tokens":3,"output_tokens":4}}]}"#,
        )
        .unwrap();
        assert_eq!(honest.cases[0].fixture.input_tokens, 3);
        assert_eq!(honest.cases[0].fixture.output_tokens, 4);
    }

    #[test]
    fn expectations_and_budgets_decide_pass_and_record_the_reason() {
        let contains = Expectation {
            contains: Some("expected".to_owned()),
            ..Expectation::default()
        };
        assert_eq!(contains.evaluate("that is expected"), None);
        assert!(contains.evaluate("nope").is_some());

        let exact = Expectation {
            equals: Some("exact".to_owned()),
            ..Expectation::default()
        };
        assert_eq!(exact.evaluate("  exact\n"), None);
        assert!(exact.evaluate("exactly").is_some());

        let absent = Expectation {
            not_contains: Some("secret".to_owned()),
            ..Expectation::default()
        };
        assert_eq!(absent.evaluate("public"), None);
        assert!(absent.evaluate("secret").is_some());
    }

    #[test]
    fn totals_and_deltas_are_recorded_from_the_case_records() {
        let records = vec![
            EvalCaseRecord {
                id: "passes".to_owned(),
                passed: true,
                latency_ms: 10,
                cost_microdollars: 5,
                usage_uncertain: false,
                input_tokens: 3,
                output_tokens: 4,
                fixture_requests: 1,
                failure: None,
            },
            EvalCaseRecord {
                id: "fails".to_owned(),
                passed: false,
                latency_ms: 30,
                cost_microdollars: 15,
                usage_uncertain: true,
                input_tokens: 7,
                output_tokens: 8,
                fixture_requests: 1,
                failure: Some("expected the answer to contain \"nope\"".to_owned()),
            },
        ];
        let totals = totals_of(&records);
        assert_eq!(totals.cases, 2);
        assert_eq!(totals.passed, 1);
        assert_eq!(totals.failed, 1);
        assert_eq!(totals.pass_rate_pp, 50.0);
        assert_eq!(totals.latency_ms_mean, 20.0);
        assert_eq!(totals.cost_microdollars_total, 20);
        assert_eq!(totals.output_tokens_total, 12);
    }

    #[test]
    fn the_fixture_streams_the_scripted_text_and_usage() {
        let body = fixture_sse_body(
            "hello \"world\"",
            &FixtureReply {
                response: String::new(),
                input_tokens: 11,
                output_tokens: 22,
            },
        );
        assert!(body.contains("hello \\\"world\\\""), "{body}");
        assert!(body.contains("\"prompt_tokens\":11"), "{body}");
        assert!(body.contains("\"completion_tokens\":22"), "{body}");
        assert!(body.ends_with("data: [DONE]\n\n"), "{body}");
    }
}
