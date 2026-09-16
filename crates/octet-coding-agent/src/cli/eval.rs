#![allow(missing_docs)]

//! Isolated evaluation with a safe scripted default and explicit local-model opt-in.
//!
//! `--model-profile` admits one bounded owner-private operator profile, never
//! suite or ambient routing. Only literal loopback Chat endpoints are supported.
//! The ordinary octet provider runtime performs inference; no response wrapper
//! sits between it and the selected server. `--offline` disables discovery, not
//! inference. Each case owns a fresh HOME/workspace/session, has no tools, and
//! is bounded by wall time and captured output. Private reports preserve known
//! usage even on failure and label unknown prices/settlement as uncertain. A
//! selected local server is operator-authorized: its own downstream routing or
//! billing is outside this harness's control. No OAuth or persistent store policy
//! is changed.

use std::io::{Read as _, Seek as _, Write as _};
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
        /// Explicit owner-private `octet-eval-model-1` JSON profile for a local
        /// OpenAI Chat model. Omit to use only the harness's scripted fixture.
        #[arg(long, value_name = "FILE")]
        model_profile: Option<PathBuf>,
        /// Hard per-case child deadline (1..=120000 ms), including startup.
        #[arg(long, default_value_t = 60000, value_parser = clap::value_parser!(u64).range(1..=120000))]
        case_timeout_ms: u64,
        /// Maximum captured bytes per child output stream (1..=1048576).
        #[arg(long, default_value_t = 262144, value_parser = clap::value_parser!(u64).range(1..=1048576))]
        max_output_bytes: u64,
    },
}

pub fn run(command: EvalCommand, cwd: &Path) -> anyhow::Result<()> {
    match command {
        EvalCommand::Run {
            suite,
            artifact_dir,
            baseline,
            model_profile,
            case_timeout_ms,
            max_output_bytes,
        } => run_suite(
            &suite,
            artifact_dir.as_deref(),
            baseline.as_deref(),
            cwd,
            model_profile.as_deref(),
            CaseLimits {
                timeout_ms: case_timeout_ms,
                output_bytes: max_output_bytes as usize,
            },
        ),
    }
}

const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
struct CaseLimits {
    timeout_ms: u64,
    output_bytes: usize,
}

/// Invocation-only profile. Deliberately not the ambient credential-store schema.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelProfile {
    schema: String,
    base_url: String,
    model: String,
    /// Empty explicitly selects unauthenticated local inference.
    api_key: String,
    context_window: u64,
    max_output_tokens: u64,
    /// Omission means unknown, NOT the custom-provider runtime's free default.
    pricing: Option<EvalPricing>,
}

/// Operator-declared microdollars per million tokens. All rates are required;
/// explicit zero prices declare a free server rather than infer one.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EvalPricing {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write_5m: u64,
}

impl ModelProfile {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let bytes = octet_agent::secure_fs::read_private_file_bounded(path, 16 * 1024)?;
        // Do not echo parser errors: unknown field names/values can be secrets.
        let profile: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid octet-eval-model-1 profile"))?;
        let url = url::Url::parse(&profile.base_url).map_err(|_| {
            anyhow::anyhow!("eval profile requires a literal HTTP loopback endpoint")
        })?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if profile.schema != "octet-eval-model-1"
            || url.scheme() != "http"
            || !loopback
            || url.port().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/v1/"
        {
            anyhow::bail!("eval profile requires octet-eval-model-1 and http://<loopback-IP>:<port>/v1/ without userinfo, query or fragment");
        }
        if profile.model.is_empty()
            || profile.model.len() > 256
            || profile.model.trim() != profile.model
            || profile.model.chars().any(char::is_control)
            || profile.api_key.len() > 4096
            || profile
                .api_key
                .chars()
                .any(|c| !c.is_ascii() || c.is_control())
            || !(1024..=1_048_576).contains(&profile.context_window)
            || !(1..=16_384).contains(&profile.max_output_tokens)
            || profile.max_output_tokens >= profile.context_window
        {
            anyhow::bail!("eval profile has invalid model, key or token limits");
        }
        Ok(profile)
    }

    fn write_credential(&self, path: &Path) -> anyhow::Result<()> {
        let record = serde_json::json!({
            "version": 1,
            "providers": {"eval": {
                "base_url": self.base_url, "api_key": self.api_key,
                "auto_discover": false, "headers": [],
                "models": [{"api_name": self.model, "context_window": self.context_window,
                    "max_output_tokens": self.max_output_tokens, "tools": false,
                    "reasoning": false, "pricing": self.pricing}]
            }}
        });
        octet_agent::secure_fs::write_private_atomic(
            path,
            format!("{record}\n").as_bytes(),
            32 * 1024,
        )?;
        Ok(())
    }
}

/// A scripted fixture reply, used only when no operator profile was selected.
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
                return Some(format!("expected answer to equal {expected:?}"));
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

/// Per-case ceilings. Latency terminates the child; cost is enforced by the
/// agent's pre-request reservation as well as checked against settled usage.
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
    pub fixture: Option<FixtureReply>,
    /// Optional hard ceilings.
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
    /// Durable usage records, not an inferred count of accepted generations.
    #[serde(default)]
    pub usage_records: u64,
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
    #[serde(default)]
    pub usage_uncertain_cases: u64,
}

/// What the run was isolated from, recorded so an artifact is self-describing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalIsolation {
    /// `injected-fixture` or `operator-profile`; no ambient credential is read.
    pub credentials: String,
    /// Always `loopback-only`.
    pub network: String,
    /// Last scripted-fixture URL; empty for operator-profile runs (no route secrets).
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
    #[serde(default)]
    pub usage_uncertain_cases_delta: i64,
    /// False means numeric cost deltas compare known subtotals only.
    #[serde(default)]
    pub cost_delta_exact: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalRunReport {
    pub schema: String,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suite: Option<String>,
    pub isolation: EvalIsolation,
    #[serde(default)]
    pub backend: String,
    /// Exact selected runtime identity, not an assertion about server internals.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub pricing: String,
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
    model_profile: Option<&Path>,
    limits: CaseLimits,
) -> anyhow::Result<()> {
    let profile = model_profile
        .map(|path| ModelProfile::read(&cwd.join(path)))
        .transpose()?;
    let suite_path = cwd.join(suite_path);
    let bytes = octet_agent::secure_fs::read_regular_file_bounded(&suite_path, MAX_DOCUMENT_BYTES)
        .map_err(|error| {
            anyhow::anyhow!(
                "could not read eval suite {}: {error}",
                suite_path.display()
            )
        })?;
    let suite: EvalSuite = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "eval suite {} is not a valid {EVAL_SUITE_SCHEMA} document ({error}); a suite cannot name a provider, endpoint or key; use an explicit operator --model-profile for local inference",
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
    if suite.cases.is_empty() || suite.cases.len() > 64 {
        anyhow::bail!("eval suite requires 1..=64 cases");
    }
    if suite
        .name
        .as_ref()
        .is_some_and(|name| name.len() > 128 || name.chars().any(char::is_control))
    {
        anyhow::bail!("eval suite name must be at most 128 bytes without controls");
    }
    let mut seen = std::collections::HashSet::new();
    for case in &suite.cases {
        if case.id.is_empty()
            || case.id.len() > 128
            || case.id.chars().any(char::is_control)
            || case.prompt.trim().is_empty()
            || case.prompt.len() > 32 * 1024
        {
            anyhow::bail!(
                "eval case requires a bounded id (128 bytes) and prompt (1..=32768 bytes)"
            );
        }
        if [
            &case.expect.equals,
            &case.expect.contains,
            &case.expect.not_contains,
        ]
        .into_iter()
        .flatten()
        .any(|text| text.len() > 4096)
        {
            anyhow::bail!("eval expectations are bounded to 4096 bytes");
        }
        if profile.is_some() && case.fixture.is_some() {
            anyhow::bail!(
                "model-backed eval refuses scripted fixture replies; remove fixture from the suite"
            );
        }
        if let Some(reply) = &case.fixture {
            if reply.response.len() > 256 * 1024
                || reply.input_tokens > 1_000_000
                || reply.output_tokens > 1_000_000
            {
                anyhow::bail!("scripted fixture response or tokens exceed eval bounds");
            }
        }
        if !seen.insert(case.id.clone()) {
            anyhow::bail!(
                "eval suite {} repeats case id {:?}",
                suite_path.display(),
                case.id
            );
        }
    }

    // Validate every local input before initiating even explicitly authorized inference.
    let baseline = baseline
        .map(|path| read_baseline(&cwd.join(path)))
        .transpose()?;
    let artifact_root = artifact_dir
        .map(|path| cwd.join(path))
        .unwrap_or_else(|| cwd.join(".eval"));
    std::fs::create_dir_all(&artifact_root)?;
    let run_id = format!("{}-{}", timestamp(), std::process::id());
    let run_dir = artifact_root.join(&run_id);
    octet_agent::secure_fs::create_private_directory_all(&run_dir)?;

    let executable = std::env::current_exe()
        .map_err(|error| anyhow::anyhow!("could not locate the running octet binary: {error}"))?;

    let runs_path = run_dir.join("runs.jsonl");
    octet_agent::secure_fs::write_private_atomic(&runs_path, b"", MAX_DOCUMENT_BYTES)?;
    let mut runs = octet_agent::secure_fs::open_regular_file_for_append(&runs_path)?;
    let mut records = Vec::new();
    let mut fixture_base_url = String::new();
    let model = profile
        .as_ref()
        .map(|p| format!("custom/eval/{}", p.model))
        .unwrap_or_else(|| format!("custom/{EVAL_FIXTURE_MODEL}"));
    let pricing_known = profile.as_ref().is_none_or(|p| p.pricing.is_some());
    for case in &suite.cases {
        // Fresh per-case roots prevent accidental prior-case session accounting,
        // config, history or prompt leakage. All disappear even after a refusal.
        let isolated = tempfile::Builder::new()
            .prefix("octet-eval-case-")
            .tempdir()?;
        let root = isolated.path().canonicalize()?;
        let home = root.join("home");
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        let credentials = home.join(".octet/credentials");
        for directory in [&credentials, &workspace, &session_root] {
            octet_agent::secure_fs::create_private_directory_all(directory)?;
        }
        let fixture = if let Some(profile) = &profile {
            profile.write_credential(&credentials.join("custom.json"))?;
            None
        } else {
            let reply = case.fixture.clone().unwrap_or_default();
            let fixture = LoopbackFixture::start(&reply.response, &reply)?;
            fixture_base_url = fixture.base_url.clone();
            write_injected_credential(&credentials.join("custom.json"), &fixture.base_url)?;
            Some(fixture)
        };
        let started = Instant::now();
        let (usage, failure) = if !pricing_known && case.budgets.max_cost_microdollars.is_some() {
            // The ordinary custom registry assumes free when prices are absent.
            // Eval must not turn unknown spend into a zero-cost hard guarantee.
            (
                CaseUsage::default(),
                Some(
                    "hard cost budget refused: profile pricing is unknown (no request sent)"
                        .to_owned(),
                ),
            )
        } else {
            match run_case(
                &executable,
                &home,
                &session_root,
                &workspace,
                case,
                &model,
                limits,
                pricing_known,
            ) {
                Ok(outcome) => outcome,
                Err(_) => (
                    CaseUsage {
                        usage_uncertain: true,
                        ..CaseUsage::default()
                    },
                    Some("could not supervise the isolated run".to_owned()),
                ),
            }
        };
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut failure = failure.or_else(|| case.expect.evaluate(&usage.output));
        if let Some(limit) = case.budgets.max_latency_ms {
            if failure.is_none() && latency_ms > limit {
                failure = Some(format!(
                    "latency {latency_ms}ms exceeds the {limit}ms budget"
                ));
            }
        }
        if let Some(limit) = case.budgets.max_cost_microdollars {
            if failure.is_none() && usage.usage_uncertain {
                failure = Some(
                    "hard cost budget cannot be verified: usage or pricing is uncertain".to_owned(),
                );
            }
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
            fixture_requests: fixture.as_ref().map_or(0, LoopbackFixture::requests),
            usage_records: usage.usage_records,
            failure: failure.map(|mut message| {
                if let Some(profile) = &profile {
                    if !profile.api_key.is_empty() {
                        message = message.replace(&profile.api_key, "[REDACTED]");
                    }
                }
                message
            }),
        };
        // Write continuously: a killed run still leaves its evidence behind.
        serde_json::to_writer(&mut runs, &record)?;
        runs.write_all(b"\n")?;
        runs.flush()?;
        records.push(record);
    }
    drop(runs);

    let totals = totals_of(&records);
    let deltas = baseline.as_ref().map(|base| deltas_against(base, &totals));
    let report = EvalRunReport {
        schema: EVAL_RUN_SCHEMA.to_owned(),
        run_id: run_id.clone(),
        suite: suite.name.clone(),
        isolation: EvalIsolation {
            credentials: if profile.is_some() {
                "operator-profile"
            } else {
                "injected-fixture"
            }
            .to_owned(),
            network: "loopback-only".to_owned(),
            fixture_base_url,
            ambient_environment: "stripped".to_owned(),
            offline: true,
        },
        backend: if profile.is_some() {
            "model"
        } else {
            "scripted-fixture"
        }
        .to_owned(),
        model,
        pricing: if profile.is_none() {
            "fixture-free"
        } else if pricing_known {
            "operator-declared"
        } else {
            "unknown"
        }
        .to_owned(),
        cases: records,
        totals,
        deltas,
    };
    octet_agent::secure_fs::write_private_atomic(
        &run_dir.join("report.json"),
        format!("{}\n", serde_json::to_string_pretty(&report)?).as_bytes(),
        MAX_DOCUMENT_BYTES,
    )?;
    octet_agent::secure_fs::write_private_atomic(
        &run_dir.join("report.txt"),
        render_report(&report).as_bytes(),
        MAX_DOCUMENT_BYTES,
    )?;

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
    usage_records: u64,
}

fn run_case(
    executable: &Path,
    home: &Path,
    session_root: &Path,
    workspace: &Path,
    case: &EvalCase,
    model: &str,
    limits: CaseLimits,
    pricing_known: bool,
) -> anyhow::Result<(CaseUsage, Option<String>)> {
    // Stdin carries literal prompt text, so @file/--flag syntax in a suite
    // cannot become CLI options or cause local file expansion.
    let mut input = tempfile::tempfile()?;
    input.write_all(case.prompt.as_bytes())?;
    input.rewind()?;
    let mut command = Command::new(executable);
    command
        .current_dir(workspace)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("PWD", workspace)
        .env("TERM", "dumb")
        .env("LANG", "C.UTF-8")
        .env("OCTET_COMPACTION_MODE", "disabled")
        .env(
            "OCTET_SYSTEM_PROMPT",
            "Answer the user's evaluation prompt. No tools are available.",
        )
        .args([
            "--offline",
            "--safe-mode",
            "--no-context-files",
            "--no-tools",
            "--max-turns",
            "1",
        ])
        .arg("--workspace")
        .arg(workspace)
        .arg("--session-dir")
        .arg(session_root)
        .args(["--color", "never", "--model"])
        .arg(model)
        .arg("--print")
        .stdin(Stdio::from(input))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(limit) = case.budgets.max_cost_microdollars {
        // Not a post-hoc threshold: the agent reserves worst-case request cost
        // before sending, and refuses unknown/unsettled usage on later requests.
        command.env("OCTET_MAX_COST_MICRODOLLARS", limit.to_string());
    }
    let mut child = command.spawn()?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!("isolated child output pipes unavailable");
    };
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = capture_bounded(stdout, limits.output_bytes, Arc::clone(&overflow));
    let stderr = capture_bounded(stderr, limits.output_bytes, Arc::clone(&overflow));
    let timeout = Duration::from_millis(
        case.budgets
            .max_latency_ms
            .unwrap_or(limits.timeout_ms)
            .min(limits.timeout_ms),
    );
    let started = Instant::now();
    let (status, mut failure) = loop {
        let reason = if overflow.load(Ordering::SeqCst) {
            Some("isolated run exceeded the output capture bound".to_owned())
        } else if started.elapsed() >= timeout {
            Some("isolated run exceeded the hard wall-time budget".to_owned())
        } else {
            None
        };
        if let Some(reason) = reason {
            let _ = child.kill();
            break (child.wait().ok(), Some(reason));
        }
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), None),
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                let _ = child.kill();
                break (
                    child.wait().ok(),
                    Some("could not wait for isolated run".to_owned()),
                );
            }
        }
    };
    let output = stdout.join().unwrap_or_default();
    let stderr_bytes = stderr.join().unwrap_or_default(); // Never persisted into report artifacts.
    if overflow.load(Ordering::SeqCst) {
        failure = Some("isolated run exceeded the output capture bound".to_owned());
    }
    let interrupted = failure.is_some();
    if failure.is_none() && !status.is_some_and(|status| status.success()) {
        // Diagnostics stay out of the persisted report by design; surface a
        // bounded tail on the test/log stream only, so an intermittent isolated
        // failure is diagnosable without leaking provider text into artifacts.
        let diagnostics = String::from_utf8_lossy(&stderr_bytes);
        eprintln!(
            "isolated run failed status={status:?}: {}",
            diagnostics.chars().take(2_000).collect::<String>()
        );
        failure = Some("isolated run failed (provider diagnostics omitted)".to_owned());
    }
    let mut usage = CaseUsage {
        output: String::from_utf8_lossy(&output).into_owned(),
        usage_uncertain: true,
        ..CaseUsage::default()
    };
    // Read accounting even after nonzero exit/timeout. An absent or torn usage
    // settlement never becomes a fictitious zero-cost successful observation.
    if let Some(transcript) = newest_transcript(session_root) {
        if let Ok(session) = octet_agent::Session::open_read_only(transcript) {
            usage.cost_microdollars = session.total_cost_microdollars();
            usage.usage_records = session.usage_records().len() as u64;
            usage.usage_uncertain = !pricing_known
                || interrupted
                || session.has_uncertain_usage()
                || usage.usage_records == 0;
            for record in session.usage_records() {
                usage.input_tokens = usage.input_tokens.saturating_add(record.usage.input_tokens);
                usage.output_tokens = usage
                    .output_tokens
                    .saturating_add(record.usage.output_tokens);
                usage.usage_uncertain |=
                    record.cost.is_none() || record.usage == octet_ai::Usage::default();
            }
        }
    }
    Ok((usage, failure))
}

fn capture_bounded(
    mut stream: impl std::io::Read + Send + 'static,
    limit: usize,
    overflow: Arc<AtomicBool>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return output,
                Ok(read) => {
                    let keep = read.min(limit.saturating_sub(output.len()));
                    output.extend_from_slice(&chunk[..keep]);
                    if keep < read {
                        overflow.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    })
}

fn newest_transcript(root: &Path) -> Option<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
        {
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
            if newest
                .as_ref()
                .is_none_or(|(current, _)| modified >= *current)
            {
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
    let latency_ms_total = records
        .iter()
        .fold(0u64, |sum, record| sum.saturating_add(record.latency_ms));
    let cost_microdollars_total = records.iter().fold(0u64, |sum, record| {
        sum.saturating_add(record.cost_microdollars)
    });
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
        input_tokens_total: records
            .iter()
            .fold(0u64, |sum, record| sum.saturating_add(record.input_tokens)),
        output_tokens_total: records
            .iter()
            .fold(0u64, |sum, record| sum.saturating_add(record.output_tokens)),
        usage_uncertain_cases: records
            .iter()
            .filter(|record| record.usage_uncertain)
            .count() as u64,
    }
}

fn read_baseline(path: &Path) -> anyhow::Result<EvalTotals> {
    let bytes = octet_agent::secure_fs::read_regular_file_bounded(path, MAX_DOCUMENT_BYTES)
        .map_err(|error| anyhow::anyhow!("could not read baseline {}: {error}", path.display()))?;
    let baseline: EvalRunReport = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "baseline {} is not an {EVAL_RUN_SCHEMA} artifact: {error}",
            path.display()
        )
    })?;
    if baseline.schema != EVAL_RUN_SCHEMA {
        anyhow::bail!("baseline schema must be {EVAL_RUN_SCHEMA}");
    }
    // Recompute from cases; old artifacts lacked an aggregate uncertainty flag.
    Ok(totals_of(&baseline.cases))
}

fn deltas_against(base: &EvalTotals, totals: &EvalTotals) -> EvalDeltas {
    EvalDeltas {
        usage_uncertain_cases_delta: totals.usage_uncertain_cases as i64
            - base.usage_uncertain_cases as i64,
        cost_delta_exact: totals.usage_uncertain_cases == 0 && base.usage_uncertain_cases == 0,
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
    }
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
    out.push_str(&format!(
        "  backend {}, model {}, pricing {}, uncertain cases {}\n",
        report.backend, report.model, report.pricing, report.totals.usage_uncertain_cases
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
        "  latency mean {:.1}ms, known cost subtotal {}µ$ ({} injected, {}, discovery offline)\n",
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
        out.push_str(&format!(
            "  uncertainty delta {:+}; cost delta {}\n",
            deltas.usage_uncertain_cases_delta,
            if deltas.cost_delta_exact {
                "exact under declared prices"
            } else {
                "known subtotals only"
            }
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
                let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = socket.set_write_timeout(Some(Duration::from_secs(1)));
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
    if length > 512 * 1024 {
        return String::new();
    }
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
        assert_eq!(honest.cases[0].fixture.as_ref().unwrap().input_tokens, 3);
        assert_eq!(honest.cases[0].fixture.as_ref().unwrap().output_tokens, 4);
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
                usage_records: 1,
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
                usage_records: 1,
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
