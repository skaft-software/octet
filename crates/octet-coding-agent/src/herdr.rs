//! Best-effort Herdr pane lifecycle reporting.
//!
//! octet is a Rust rewrite of Pi, so this module implements the same contract
//! Herdr's official Pi integration uses (`herdr integration install pi`,
//! `src/integration/assets/pi/herdr-agent-state.ts`, integration version 9) and
//! that Herdr documents for custom agents
//! (<https://herdr.dev/docs/agent-automation/>):
//!
//! - `pane.report_agent` with semantic `idle` / `working` / `blocked` state;
//! - `pane.report_agent_session` for native session identity (with
//!   `session_start_source` when the reason is known);
//! - a strictly increasing `seq` per source, so Herdr can drop stale reports;
//! - identical consecutive states are not re-sent, and a session-identity
//!   change always re-publishes;
//! - `pane.release_agent` on exit (the documented custom-integration practice;
//!   Pi's file relies on Herdr observing the agent process exit instead).
//!
//! House rules, in order of severity:
//!
//! 1. **No-op outside Herdr.** Reporting activates only when `HERDR_ENV=1` and
//!    a transport (`HERDR_SOCKET_PATH`, else `HERDR_BIN_PATH`) and
//!    `HERDR_PANE_ID` are present. Nothing here reads, writes, or launches
//!    anything else.
//! 2. **Best effort.** Every delivery uses the same 500 ms → 1500 ms
//!    attempts the Pi integration uses, and every failure is swallowed. A slow
//!    server may delay a report boundary, but cannot fail a prompt or exit.
//! 3. **Session identity only, never secrets.** The reported session reference
//!    is octet's opaque, path-free session id (the same value
//!    `octet --resume <id>` accepts); transcript paths are deliberately not
//!    reported, unlike Pi's `agent_session_path`. No credential or transcript
//!    text leaves the process. Blocked messages are the short confirmation
//!    prompts the user already sees on screen, bounded and control-free.
//! 4. **Sequence-authoritative.** Herdr ignores stale sequence numbers per
//!    source, so ordering is enforced by `seq` rather than by cross-connection
//!    socket ordering. After a release, no further reports are sent: a late
//!    report would reclaim a pane whose agent has exited.
//! 5. **Semantic state only.** Display-only presentation (pane titles, tokens,
//!    custom state labels) is left to the user through Herdr metadata, exactly
//!    as the official integrations do.
//! 6. **TUI only.** The reporter is owned by the interactive terminal frontend,
//!    so `--print`, `--rpc`, plain/headless, and Serve runs never report — the
//!    same gate Pi applies with `ctx.mode !== "tui"`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use octet_agent::AgentEvent;
use octet_ai::ToolCallId;
use serde_json::{json, Value};

pub(crate) mod plugin;
pub(crate) mod restore;

/// `octet herdr <command>`: the integration's own CLI surface.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum HerdrCommand {
    /// Resume recorded octet sessions in restored Herdr panes.
    ///
    /// This is the command the octet Herdr plugin runs from its startup hook,
    /// after Herdr has restored the session and its API socket is ready.
    Restore,
    /// Install the Herdr plugin that runs the restore hook after a restart.
    InstallPlugin,
    /// Remove the octet Herdr plugin.
    UninstallPlugin,
    /// Show the octet plugin and recorded-pane state.
    Status,
}

/// Dispatch `octet herdr <command>`.
pub(crate) fn run_command(command: HerdrCommand) -> anyhow::Result<()> {
    match command {
        HerdrCommand::Restore => restore::run_restore(),
        HerdrCommand::InstallPlugin => {
            let executable = std::env::current_exe().map_err(|error| {
                anyhow::anyhow!("could not resolve the octet executable: {error}")
            })?;
            plugin::install(&executable)
        }
        HerdrCommand::UninstallPlugin => plugin::uninstall(),
        HerdrCommand::Status => plugin::status(),
    }
}

/// Stable integration source id. Herdr binds lifecycle authority, sequence
/// numbers, and stored session references per source.
const SOURCE: &str = "custom:octet";
/// Agent label shown by the Herdr sidebar and used as a control target.
const AGENT_LABEL: &str = "octet";
/// Upper bound for one blocked-state message. Herdr normalizes presentation
/// text itself; this keeps our wire payload bounded independently.
const MESSAGE_MAX_CHARS: usize = 200;
/// Delivery attempts, matching the official Pi integration: one short
/// attempt, then one longer retry only when the first was not delivered.
const FIRST_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(500);
const RETRY_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Bounded wait for the CLI transport used where raw socket IPC is not
/// available (Windows named pipes). The CLI wrapper talks to the same server.
const CLI_WAIT_TIMEOUT: Duration = Duration::from_millis(1000);

/// Semantic agent states accepted by `pane.report_agent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HerdrState {
    Idle,
    Working,
    Blocked,
}

impl HerdrState {
    fn as_str(self) -> &'static str {
        match self {
            HerdrState::Idle => "idle",
            HerdrState::Working => "working",
            HerdrState::Blocked => "blocked",
        }
    }
}

/// How one report reaches the local Herdr server.
enum Transport {
    /// Direct socket IPC (`pane.report_agent` and friends), the same transport
    /// the official integrations use. Unix only.
    #[cfg(unix)]
    Socket { path: PathBuf },
    /// The documented portable CLI wrapper (`HERDR_BIN_PATH`). Used on
    /// platforms without a std socket client and as a fallback when no socket
    /// path is present.
    Cli { bin: PathBuf },
}

/// One report to encode. Keeping the report structured lets both transports
/// render it without any string-splitting or shell involvement.
enum Request<'a> {
    ReportAgent {
        state: HerdrState,
        message: Option<&'a str>,
        session_id: Option<&'a str>,
        seq: u64,
    },
    ReportAgentSession {
        session_id: &'a str,
        start_source: Option<&'a str>,
        seq: u64,
    },
    ReleaseAgent {
        seq: u64,
    },
}

impl Request<'_> {
    /// The `pane.report_*` method name this request maps to.
    fn method(&self) -> &'static str {
        match self {
            Request::ReportAgent { .. } => "pane.report_agent",
            Request::ReportAgentSession { .. } => "pane.report_agent_session",
            Request::ReleaseAgent { .. } => "pane.release_agent",
        }
    }

    fn seq(&self) -> u64 {
        match self {
            Request::ReportAgent { seq, .. }
            | Request::ReportAgentSession { seq, .. }
            | Request::ReleaseAgent { seq } => *seq,
        }
    }

    /// Canonical newline-delimited socket request.
    fn json(&self, pane_id: &str) -> Value {
        let mut params = json!({
            "pane_id": pane_id,
            "source": SOURCE,
            "agent": AGENT_LABEL,
            "seq": self.seq(),
        });
        match self {
            Request::ReportAgent {
                state,
                message,
                session_id,
                ..
            } => {
                params["state"] = json!(state.as_str());
                if let Some(message) = message {
                    params["message"] = json!(message);
                }
                if let Some(session_id) = session_id {
                    params["agent_session_id"] = json!(session_id);
                }
            }
            Request::ReportAgentSession {
                session_id,
                start_source,
                ..
            } => {
                params["agent_session_id"] = json!(session_id);
                if let Some(start_source) = start_source {
                    params["session_start_source"] = json!(start_source);
                }
            }
            Request::ReleaseAgent { .. } => {}
        }
        json!({
            "id": format!("{SOURCE}:{}", self.seq()),
            "method": self.method(),
            "params": params,
        })
    }

    /// The same report as an argv list for the CLI transport. Every value is a
    /// separate argument; no shell is ever involved, so a message can never
    /// widen the command.
    fn argv(&self, pane_id: &str) -> Vec<String> {
        let mut argv = match self {
            Request::ReportAgent { .. } => vec!["pane".into(), "report-agent".into()],
            Request::ReportAgentSession { .. } => {
                vec!["pane".into(), "report-agent-session".into()]
            }
            Request::ReleaseAgent { .. } => vec!["pane".into(), "release-agent".into()],
        };
        argv.push(pane_id.to_owned());
        argv.push("--source".into());
        argv.push(SOURCE.into());
        argv.push("--agent".into());
        argv.push(AGENT_LABEL.into());
        match self {
            Request::ReportAgent {
                state,
                message,
                session_id,
                seq,
            } => {
                argv.push("--state".into());
                argv.push(state.as_str().into());
                if let Some(message) = message {
                    argv.push("--message".into());
                    argv.push((*message).to_owned());
                }
                if let Some(session_id) = session_id {
                    argv.push("--agent-session-id".into());
                    argv.push((*session_id).to_owned());
                }
                argv.push("--seq".into());
                argv.push(seq.to_string());
            }
            Request::ReportAgentSession {
                session_id,
                start_source,
                seq,
            } => {
                argv.push("--agent-session-id".into());
                argv.push((*session_id).to_owned());
                if let Some(start_source) = start_source {
                    argv.push("--session-start-source".into());
                    argv.push((*start_source).to_owned());
                }
                argv.push("--seq".into());
                argv.push(seq.to_string());
            }
            Request::ReleaseAgent { seq } => {
                argv.push("--seq".into());
                argv.push(seq.to_string());
            }
        }
        argv
    }
}

/// Lifecycle mapping plus strictly increasing report sequence.
struct Lifecycle {
    /// A prompt has been accepted and its run has not finished yet.
    active: bool,
    /// Tool ids with an open confirmation or input request, mapped to the
    /// short user-facing prompt describing the block.
    blocked: HashMap<ToolCallId, String>,
    /// Monotonic per-source sequence, seeded from wall-clock milliseconds so a
    /// restarted octet process in the same pane never re-enters the stale
    /// sequence range of the previous process.
    seq: u64,
    /// Whether any report was actually sent for this pane.
    reported: bool,
    /// Set by [`PaneReporter::release`]; a reporter must go silent after the
    /// pane's agent exited so a late report cannot reclaim the pane.
    released: bool,
    /// Last published (state, message) pair; identical successors are not
    /// re-sent.
    last: Option<(HerdrState, Option<String>)>,
    /// Opaque, path-free octet session id (the durable JSONL file stem).
    session_id: Option<String>,
    /// Lookup scope to restore alongside the id after a server restart.
    launch_scope: Option<restore::LaunchScope>,
}

impl Lifecycle {
    fn desired(&self) -> (HerdrState, Option<String>) {
        if let Some(message) = self.blocked.values().next() {
            return (HerdrState::Blocked, Some(message.clone()));
        }
        if self.active {
            return (HerdrState::Working, None);
        }
        (HerdrState::Idle, None)
    }
}

/// Best-effort Herdr lifecycle reporter for one interactive frontend.
pub(crate) struct PaneReporter {
    pane_id: String,
    transport: Option<Transport>,
    /// Durable record directory. `None` uses the real user state directory;
    /// tests inject a temporary directory so they never write there.
    records_dir: Option<PathBuf>,
    inner: Mutex<Lifecycle>,
}

impl PaneReporter {
    /// Detect the Herdr pane environment. Returns a reporter that is a
    /// complete no-op when any documented variable is missing.
    pub fn detect() -> Self {
        let enabled = std::env::var("HERDR_ENV").ok().as_deref() == Some("1");
        let pane_id = non_empty_env("HERDR_PANE_ID").unwrap_or_default();
        let transport = if enabled { transport_from_env() } else { None };
        Self::with_transport(pane_id, transport)
    }

    /// A reporter that never touches Herdr. Used by renderer tests so a test
    /// run inside a real Herdr pane can never report pane state.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn disabled() -> Self {
        Self::with_transport(String::new(), None)
    }

    fn with_transport(pane_id: String, transport: Option<Transport>) -> Self {
        Self {
            pane_id,
            transport,
            records_dir: None,
            inner: Mutex::new(Lifecycle {
                active: false,
                blocked: HashMap::new(),
                seq: initial_seq(),
                reported: false,
                released: false,
                last: None,
                session_id: None,
                launch_scope: None,
            }),
        }
    }

    /// Install the session identity and report the startup-ready state, like
    /// Pi's `session_start` handler.
    pub fn attach(
        &self,
        session_id: Option<String>,
        start_source: Option<&str>,
        launch_scope: restore::LaunchScope,
    ) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.released {
            return;
        }
        inner.session_id = session_id;
        inner.launch_scope = Some(launch_scope);
        self.record_pane_locked(&inner);
        self.report_session_locked(&mut inner, start_source);
        let (state, message) = inner.desired();
        self.publish_locked(&mut inner, state, message, true);
    }

    /// A prompt was accepted: refresh the session reference (it may have
    /// changed through `/resume`, `/fork`, `/new`, or a reload) and report the
    /// working state, like Pi's `agent_start`.
    pub fn run_started(&self, session_id: Option<String>) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.released {
            return;
        }
        if let Some(session_id) = session_id {
            if inner.session_id.as_deref() != Some(session_id.as_str()) {
                inner.session_id = Some(session_id);
                self.record_pane_locked(&inner);
                // Pi re-reports its session reference on every `agent_start`;
                // octet reports it only when it actually changed, which keeps
                // the same guarantee without a redundant request per turn.
                self.report_session_locked(&mut inner, None);
            }
        }
        inner.active = true;
        let (state, message) = inner.desired();
        self.publish_locked(&mut inner, state, message, false);
    }

    /// The active durable session changed (`/resume`, `/fork`, `/new`, or an
    /// extension lifecycle reload). Refresh the reference and re-publish so
    /// Herdr stores the successor session rather than a stale one.
    pub fn session_changed(
        &self,
        session_id: Option<String>,
        start_source: Option<&str>,
        launch_scope: restore::LaunchScope,
    ) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.released {
            return;
        }
        inner.session_id = session_id;
        inner.launch_scope = Some(launch_scope);
        self.record_pane_locked(&inner);
        self.report_session_locked(&mut inner, start_source);
        let (state, message) = inner.desired();
        self.publish_locked(&mut inner, state, message, true);
    }

    /// Keep the durable "octet is running in this pane" record current.
    ///
    /// The record is what lets octet hand a restored pane back to itself after
    /// a Herdr server restart; [`Self::finish`] removes it on a deliberate exit.
    fn record_pane_locked(&self, inner: &Lifecycle) {
        if self.pane_id.is_empty() || self.transport.is_none() {
            return;
        }
        let (Some(session_id), Some(launch_scope)) =
            (inner.session_id.as_deref(), inner.launch_scope.as_ref())
        else {
            return;
        };
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        restore::write_record_in(
            &self.records_path(),
            &self.pane_id,
            session_id,
            &cwd,
            launch_scope,
        );
    }

    /// The directory holding this pane's durable record.
    fn records_path(&self) -> PathBuf {
        self.records_dir
            .clone()
            .unwrap_or_else(restore::records_dir)
    }

    /// Feed one run-stream event into the lifecycle mapping. Only events that
    /// change the semantic state send a report.
    pub fn on_run_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolProgress { id, progress } => match progress {
                octet_agent::ToolProgress::Confirmation(request) => {
                    self.note_blocked(id.clone(), &request.prompt);
                }
                octet_agent::ToolProgress::Input(request) => {
                    self.note_blocked(id.clone(), &request.prompt);
                }
                _ => {}
            },
            AgentEvent::ToolFinished { id, .. } => self.note_unblocked(id),
            AgentEvent::RunFinished { .. } => self.note_run_finished(),
            _ => {}
        }
    }

    /// A tool is waiting on a human confirmation or input answer — Pi's
    /// `herdr:blocked` event with `active: true`.
    fn note_blocked(&self, id: ToolCallId, prompt: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.released {
            return;
        }
        inner.blocked.insert(id, bound_message(prompt));
        let (state, message) = inner.desired();
        self.publish_locked(&mut inner, state, message, false);
    }

    /// The blocked tool produced its final result — Pi's `herdr:blocked` event
    /// with `active: false`.
    fn note_unblocked(&self, id: &ToolCallId) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.released {
            return;
        }
        if inner.blocked.remove(id).is_none() {
            return;
        }
        let (state, message) = inner.desired();
        self.publish_locked(&mut inner, state, message, false);
    }

    /// The run settled (`completed`, `aborted`, `failed`, or `max_turns`) —
    /// Pi's `agent_settled`. The state returns to `idle` in every case; the
    /// outcome itself stays in the transcript, exactly as it does for Pi.
    fn note_run_finished(&self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.released {
            return;
        }
        inner.active = false;
        inner.blocked.clear();
        let (state, message) = inner.desired();
        self.publish_locked(&mut inner, state, message, false);
    }

    /// Finish this pane's reporting as the frontend leaves the terminal.
    ///
    /// `keep_for_restore` is true only when the terminal itself went away — a
    /// Herdr server stop sends `SIGHUP` to the pane's foreground process
    /// (measured against Herdr 0.9.0) — because that is exactly the case where
    /// the recorded pane should be handed back to octet once Herdr restores the
    /// workspace. Every other exit is deliberate, so the record is dropped and
    /// a later restore will not resurrect a session the user closed.
    pub fn finish(&self, keep_for_restore: bool) {
        if !keep_for_restore && !self.pane_id.is_empty() {
            restore::remove_record_in(&self.records_path(), &self.pane_id);
        }
        self.release();
    }

    /// Release lifecycle authority for this pane's source. Called once when
    /// the frontend leaves the terminal. Idempotent, and silent if this pane
    /// never reported.
    pub fn release(&self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.released {
            return;
        }
        inner.released = true;
        if !inner.reported {
            return;
        }
        inner.last = None;
        inner.seq += 1;
        let request = Request::ReleaseAgent { seq: inner.seq };
        self.send(&request);
    }

    /// Send one `pane.report_agent_session` request when a session reference
    /// exists. The caller holds the lifecycle lock.
    fn report_session_locked(&self, inner: &mut Lifecycle, start_source: Option<&str>) {
        if inner.released || self.transport.is_none() {
            return;
        }
        let Some(session_id) = inner.session_id.clone() else {
            return;
        };
        inner.seq += 1;
        inner.reported = true;
        let request = Request::ReportAgentSession {
            session_id: &session_id,
            start_source,
            seq: inner.seq,
        };
        self.send(&request);
    }

    /// Compute, deduplicate, sequence, and send one state report. The caller
    /// holds the lifecycle lock for its whole transition, so reports are
    /// emitted in the same order as the state changes that produced them.
    fn publish_locked(
        &self,
        inner: &mut Lifecycle,
        state: HerdrState,
        message: Option<String>,
        force: bool,
    ) {
        if inner.released || self.transport.is_none() {
            return;
        }
        if !force && inner.last.as_ref() == Some(&(state, message.clone())) {
            return;
        }
        inner.last = Some((state, message.clone()));
        inner.seq += 1;
        inner.reported = true;
        let request = Request::ReportAgent {
            state,
            message: message.as_deref(),
            session_id: inner.session_id.as_deref(),
            seq: inner.seq,
        };
        self.send(&request);
    }

    /// Deliver one request over the configured transport. Failures are
    /// deliberately silent: Herdr is an observer, never a dependency.
    fn send(&self, request: &Request<'_>) {
        let Some(transport) = &self.transport else {
            return;
        };
        match transport {
            #[cfg(unix)]
            Transport::Socket { path } => {
                let payload = request.json(&self.pane_id);
                if !socket_attempt(path, &payload, FIRST_ATTEMPT_TIMEOUT) {
                    socket_attempt(path, &payload, RETRY_ATTEMPT_TIMEOUT);
                }
            }
            Transport::Cli { bin } => {
                let _ = cli_send(bin, &request.argv(&self.pane_id));
            }
        }
    }
}

/// Pick the transport from the documented environment. Socket IPC is
/// preferred wherever it exists; the CLI wrapper is the portable fallback.
fn transport_from_env() -> Option<Transport> {
    #[cfg(unix)]
    {
        if let Some(path) = non_empty_env("HERDR_SOCKET_PATH") {
            return Some(Transport::Socket {
                path: PathBuf::from(path),
            });
        }
    }
    non_empty_env("HERDR_BIN_PATH").map(|bin| Transport::Cli {
        bin: PathBuf::from(bin),
    })
}

/// One bounded socket attempt. `true` means the server answered, which is what
/// the official Pi integration treats as delivery.
///
/// A Unix-domain connect has no network handshake, so the connect itself is
/// effectively immediate; the bounded read of the reply is what caps a
/// wedged-server attempt.
#[cfg(unix)]
fn socket_attempt(socket_path: &std::path::Path, request: &Value, timeout: Duration) -> bool {
    use std::io::{Read, Write};
    use std::os::unix::net::{SocketAddr, UnixStream};

    let Ok(address) = SocketAddr::from_pathname(socket_path) else {
        return false;
    };
    let Ok(mut stream) = UnixStream::connect_addr(&address) else {
        return false;
    };
    if stream.set_write_timeout(Some(timeout)).is_err()
        || stream.set_read_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    let mut payload = request.to_string().into_bytes();
    payload.push(b'\n');
    if stream.write_all(&payload).is_err() {
        return false;
    }
    let mut acknowledgement = [0u8; 1];
    matches!(stream.read(&mut acknowledgement), Ok(1))
}

/// One bounded CLI attempt through `HERDR_BIN_PATH`. The wrapper talks to the
/// same server over the platform-native local socket, so this is the portable
/// path for platforms without a std socket client. Failures are silent.
fn cli_send(bin: &std::path::Path, argv: &[String]) -> bool {
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new(bin)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + CLI_WAIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                // Bounded: never let an unresponsive CLI outlive the report.
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Wall-clock-millisecond seed scaled so a same-pane process restart starts
/// above every sequence number the previous process could have used. Matches
/// the official Pi integration's `Date.now() * 1000` seed.
fn initial_seq() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| {
            since
                .as_millis()
                .saturating_mul(1000)
                .min(u128::from(u64::MAX)) as u64
        })
        .unwrap_or(0)
}

/// Keep a blocked message short, single-line, and free of control bytes. The
/// text is display-only; Herdr additionally normalizes what it stores.
fn bound_message(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= MESSAGE_MAX_CHARS {
        return trimmed.to_owned();
    }
    let mut bounded: String = trimmed.chars().take(MESSAGE_MAX_CHARS).collect();
    bounded.push('…');
    bounded
}

#[cfg(test)]
fn test_launch_scope() -> restore::LaunchScope {
    restore::LaunchScope::new(
        std::path::Path::new("/tmp/octet-sessions"),
        std::path::Path::new("/tmp/octet-workspace"),
        std::path::Path::new("/"),
    )
}

#[cfg(test)]
fn tool_id(value: &str) -> ToolCallId {
    ToolCallId(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reporter() -> PaneReporter {
        PaneReporter::with_transport(String::new(), None)
    }

    #[test]
    fn blocked_messages_are_bounded_and_single_line() {
        assert_eq!(bound_message("  run cargo test? \n"), "run cargo test?");
        let bounded = bound_message(&"x".repeat(300));
        assert_eq!(bounded.chars().count(), MESSAGE_MAX_CHARS + 1);
        assert!(bounded.ends_with('…'));
        assert_eq!(bound_message("a\u{7}b"), "ab");
    }

    #[test]
    fn lifecycle_maps_runs_blocks_and_completions() {
        let reporter = reporter();
        reporter.attach(
            Some("session-1".to_owned()),
            Some("startup"),
            test_launch_scope(),
        );
        {
            let inner = reporter.inner.lock().unwrap();
            assert_eq!(inner.desired(), (HerdrState::Idle, None));
            assert_eq!(inner.session_id.as_deref(), Some("session-1"));
        }
        reporter.run_started(Some("session-1".to_owned()));
        assert_eq!(
            reporter.inner.lock().unwrap().desired(),
            (HerdrState::Working, None)
        );
        // Non-blocking progress never changes the semantic state.
        reporter.on_run_event(&AgentEvent::ToolProgress {
            id: tool_id("t1"),
            progress: octet_agent::ToolProgress::Status("running".to_owned()),
        });
        assert_eq!(
            reporter.inner.lock().unwrap().desired(),
            (HerdrState::Working, None)
        );
        reporter.note_blocked(tool_id("t1"), "run the tests?");
        assert_eq!(
            reporter.inner.lock().unwrap().desired(),
            (HerdrState::Blocked, Some("run the tests?".to_owned()))
        );
        reporter.on_run_event(&AgentEvent::ToolFinished {
            id: tool_id("t1"),
            result: Ok(octet_agent::ToolOutput::new("ok")),
            duration: Duration::from_millis(1),
        });
        assert_eq!(
            reporter.inner.lock().unwrap().desired(),
            (HerdrState::Working, None)
        );
        reporter.on_run_event(&AgentEvent::RunFinished {
            head: octet_agent::EntryId("entry-1".to_owned()),
            reason: octet_agent::FinishReason::Completed,
        });
        let inner = reporter.inner.lock().unwrap();
        assert_eq!(inner.desired(), (HerdrState::Idle, None));
        assert!(inner.blocked.is_empty());
        assert!(!inner.active);
    }

    #[test]
    fn a_settled_run_clears_a_still_open_block() {
        let reporter = reporter();
        reporter.run_started(None);
        reporter.note_blocked(tool_id("t1"), "approve?");
        reporter.on_run_event(&AgentEvent::RunFinished {
            head: octet_agent::EntryId("entry-1".to_owned()),
            reason: octet_agent::FinishReason::Aborted,
        });
        let inner = reporter.inner.lock().unwrap();
        assert_eq!(inner.desired(), (HerdrState::Idle, None));
        assert!(!inner.active);
    }

    #[test]
    fn a_failed_run_settles_to_idle_like_the_pi_integration() {
        let reporter = reporter();
        reporter.run_started(None);
        reporter.on_run_event(&AgentEvent::RunFinished {
            head: octet_agent::EntryId("entry-1".to_owned()),
            reason: octet_agent::FinishReason::MaxTurns,
        });
        assert_eq!(
            reporter.inner.lock().unwrap().desired(),
            (HerdrState::Idle, None)
        );
    }

    #[test]
    fn session_changes_and_release_are_tracked_without_a_transport() {
        let reporter = reporter();
        reporter.attach(Some("s".to_owned()), None, test_launch_scope());
        {
            let inner = reporter.inner.lock().unwrap();
            // Without a transport nothing is sent and nothing is claimed.
            assert!(!inner.reported);
            assert_eq!(inner.desired(), (HerdrState::Idle, None));
        }
        reporter.session_changed(Some("s2".to_owned()), Some("resume"), test_launch_scope());
        assert_eq!(
            reporter.inner.lock().unwrap().session_id.as_deref(),
            Some("s2")
        );
        reporter.release();
        {
            let inner = reporter.inner.lock().unwrap();
            assert!(inner.released);
            assert!(inner.last.is_none());
        }
        // A late report after release must not reclaim the pane.
        reporter.run_started(None);
        reporter.note_blocked(tool_id("t9"), "still there?");
        let inner = reporter.inner.lock().unwrap();
        assert!(!inner.active);
        assert!(inner.blocked.is_empty());
    }

    #[test]
    fn release_without_a_report_sends_nothing() {
        let reporter = reporter();
        reporter.release();
        let inner = reporter.inner.lock().unwrap();
        assert!(inner.released);
        assert!(!inner.reported);
    }

    #[test]
    fn initial_sequence_is_wall_clock_scaled() {
        assert!(initial_seq() > 1_000_000_000_000);
    }

    #[test]
    fn socket_requests_are_canonical_json() {
        let report = Request::ReportAgent {
            state: HerdrState::Blocked,
            message: Some("run the tests?"),
            session_id: Some("session-7"),
            seq: 12,
        };
        let value = report.json("w1:p1");
        assert_eq!(value["method"], "pane.report_agent");
        assert_eq!(value["params"]["pane_id"], "w1:p1");
        assert_eq!(value["params"]["source"], SOURCE);
        assert_eq!(value["params"]["agent"], AGENT_LABEL);
        assert_eq!(value["params"]["state"], "blocked");
        assert_eq!(value["params"]["message"], "run the tests?");
        assert_eq!(value["params"]["agent_session_id"], "session-7");
        assert_eq!(value["params"]["seq"], 12);

        let session = Request::ReportAgentSession {
            session_id: "session-7",
            start_source: Some("resume"),
            seq: 13,
        };
        let value = session.json("w1:p1");
        assert_eq!(value["method"], "pane.report_agent_session");
        assert_eq!(value["params"]["agent_session_id"], "session-7");
        assert_eq!(value["params"]["session_start_source"], "resume");

        let release = Request::ReleaseAgent { seq: 14 };
        assert_eq!(release.json("w1:p1")["method"], "pane.release_agent");
    }

    #[test]
    fn cli_arguments_are_separate_elements() {
        let report = Request::ReportAgent {
            state: HerdrState::Working,
            message: None,
            session_id: None,
            seq: 3,
        };
        assert_eq!(
            report.argv("w1:p1"),
            vec![
                "pane",
                "report-agent",
                "w1:p1",
                "--source",
                SOURCE,
                "--agent",
                AGENT_LABEL,
                "--state",
                "working",
                "--seq",
                "3",
            ]
        );
        // A message with shell metacharacters stays exactly one argv element.
        let hostile = Request::ReportAgent {
            state: HerdrState::Blocked,
            message: Some("approve $(rm -rf /) && echo hi"),
            session_id: None,
            seq: 4,
        };
        let argv = hostile.argv("w1:p1");
        assert!(argv.contains(&"approve $(rm -rf /) && echo hi".to_owned()));
        assert_eq!(
            argv.iter()
                .filter(|value| value.as_str() == "--message")
                .count(),
            1
        );
    }
}

/// Wire-level tests. They bind a private temporary socket or a stub CLI
/// executable and never touch a live Herdr server.
#[cfg(all(test, unix))]
mod transport_tests {
    use super::*;
    use std::io::{Read, Write};

    struct TemporarySocket {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TemporarySocket {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000;
            let directory =
                std::env::temp_dir().join(format!("octet-herdr-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("herdr.sock");
            Self { directory, path }
        }
    }

    impl Drop for TemporarySocket {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.directory).ok();
        }
    }

    /// Accept connections until the socket goes idle for a moment, replying to
    /// each so the reporter sees a delivered request, and return every
    /// newline-delimited line. An idle-timeout loop keeps the test independent
    /// of an exact request count while still failing loudly on a hang.
    fn serve(listener: std::os::unix::net::UnixListener) -> std::thread::JoinHandle<Vec<String>> {
        std::thread::spawn(move || {
            let mut received = Vec::new();
            listener.set_nonblocking(true).unwrap();
            let mut idle_since = std::time::Instant::now();
            while idle_since.elapsed() < Duration::from_millis(400) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        idle_since = std::time::Instant::now();
                        // macOS may hand back a stream that inherits the
                        // listener's non-blocking mode; restore blocking reads
                        // so the bounded read timeout (not a WouldBlock race)
                        // decides when a request is complete.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_millis(500)))
                            .unwrap();
                        let mut line = Vec::new();
                        let mut byte = [0u8; 1];
                        while !line.ends_with(b"\n") {
                            match stream.read(&mut byte) {
                                Ok(1) => line.push(byte[0]),
                                _ => break,
                            }
                        }
                        let _ = stream.write_all(b"{\"ok\":true}\n");
                        received.push(String::from_utf8(line).unwrap());
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            received
        })
    }

    fn socket_reporter(socket: &TemporarySocket) -> PaneReporter {
        socket_reporter_with_records(socket, "w1:p1")
    }

    /// A socket reporter whose durable records live in `TemporarySocket`'s own
    /// directory, so tests can never write into the real user state directory.
    fn socket_reporter_with_records(socket: &TemporarySocket, pane_id: &str) -> PaneReporter {
        let mut reporter = PaneReporter::with_transport(
            pane_id.to_owned(),
            Some(Transport::Socket {
                path: socket.path.clone(),
            }),
        );
        reporter.records_dir = Some(socket.directory.join("records"));
        reporter
    }

    fn parsed(lines: &[String]) -> Vec<Value> {
        lines
            .iter()
            .map(|line| serde_json::from_str(line.trim_end()).unwrap())
            .collect()
    }

    #[test]
    fn a_full_lifecycle_is_reported_in_order() {
        let socket = TemporarySocket::new();
        let listener = std::os::unix::net::UnixListener::bind(&socket.path).unwrap();
        let server = serve(listener);

        let reporter = socket_reporter(&socket);
        reporter.attach(
            Some("session-42".to_owned()),
            Some("startup"),
            test_launch_scope(),
        );
        reporter.run_started(None);
        reporter.note_blocked(tool_id("t1"), "run the tests?");
        reporter.on_run_event(&AgentEvent::ToolFinished {
            id: tool_id("t1"),
            result: Ok(octet_agent::ToolOutput::new("ok")),
            duration: Duration::from_millis(1),
        });
        reporter.on_run_event(&AgentEvent::RunFinished {
            head: octet_agent::EntryId("entry-1".to_owned()),
            reason: octet_agent::FinishReason::Completed,
        });
        reporter.release();

        let received = server.join().unwrap();
        let requests = parsed(&received);
        let methods: Vec<&str> = requests
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            vec![
                "pane.report_agent_session",
                "pane.report_agent",
                "pane.report_agent",
                "pane.report_agent",
                "pane.report_agent",
                "pane.report_agent",
                "pane.release_agent",
            ],
            "session identity, idle, working, blocked, working, idle, release"
        );
        let states: Vec<&str> = requests[1..6]
            .iter()
            .map(|request| request["params"]["state"].as_str().unwrap())
            .collect();
        assert_eq!(
            states,
            vec!["idle", "working", "blocked", "working", "idle"]
        );
        assert_eq!(requests[0]["params"]["session_start_source"], "startup");
        assert_eq!(requests[0]["params"]["agent_session_id"], "session-42");
        // idle, working, blocked, working, idle
        assert_eq!(requests[3]["params"]["message"], "run the tests?");
        assert_eq!(requests[0]["params"]["pane_id"], "w1:p1");
        assert_eq!(requests[0]["params"]["source"], SOURCE);
        assert_eq!(requests[0]["params"]["agent"], AGENT_LABEL);
        // Sequence numbers strictly increase across every request.
        let sequences: Vec<u64> = requests
            .iter()
            .map(|request| request["params"]["seq"].as_u64().unwrap())
            .collect();
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn an_identical_state_is_not_resent() {
        let socket = TemporarySocket::new();
        let listener = std::os::unix::net::UnixListener::bind(&socket.path).unwrap();
        let server = serve(listener);

        let reporter = socket_reporter(&socket);
        reporter.attach(Some("s".to_owned()), None, test_launch_scope());
        // Repeating the same state must not produce another request.
        reporter.run_started(None);
        reporter.run_started(None);
        reporter.on_run_event(&AgentEvent::RunFinished {
            head: octet_agent::EntryId("entry-1".to_owned()),
            reason: octet_agent::FinishReason::Completed,
        });
        reporter.release();

        let received = server.join().unwrap();
        let requests = parsed(&received);
        let methods: Vec<String> = requests
            .iter()
            .map(|request| request["method"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            methods,
            vec![
                "pane.report_agent_session",
                "pane.report_agent",
                "pane.report_agent",
                "pane.report_agent",
                "pane.release_agent",
            ],
            "session, idle, working, idle, release — the duplicate working is dropped"
        );
        let states: Vec<&str> = requests[1..4]
            .iter()
            .map(|request| request["params"]["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, vec!["idle", "working", "idle"]);
    }

    #[test]
    fn a_deliberate_exit_drops_the_record_and_a_teardown_keeps_it() {
        let socket = TemporarySocket::new();
        let listener = std::os::unix::net::UnixListener::bind(&socket.path).unwrap();
        let server = serve(listener);

        // Deliberate exit: the record is removed, so a later restore does not
        // resurrect a session the user closed.
        let deliberate = socket_reporter_with_records(&socket, "w1:p1");
        deliberate.attach(
            Some("session-quit".to_owned()),
            Some("startup"),
            test_launch_scope(),
        );
        let records = deliberate.records_path();
        assert_eq!(restore::read_records(&records).len(), 1);
        deliberate.finish(false);
        assert!(
            restore::read_records(&records).is_empty(),
            "a deliberate exit must drop the pane record"
        );

        // Workspace teardown (SIGHUP): the record survives for the restore pass.
        let teardown = socket_reporter_with_records(&socket, "w1:p2");
        teardown.attach(
            Some("session-hup".to_owned()),
            Some("startup"),
            test_launch_scope(),
        );
        teardown.finish(true);
        let kept = restore::read_records(&records);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].pane_id, "w1:p2");
        assert_eq!(kept[0].session_id, "session-hup");
        server.join().unwrap();
    }

    #[test]
    fn a_session_change_refreshes_the_record() {
        let socket = TemporarySocket::new();
        let listener = std::os::unix::net::UnixListener::bind(&socket.path).unwrap();
        let server = serve(listener);

        let reporter = socket_reporter_with_records(&socket, "w1:p1");
        reporter.attach(Some("session-first".to_owned()), None, test_launch_scope());
        reporter.session_changed(
            Some("session-second".to_owned()),
            Some("resume"),
            test_launch_scope(),
        );
        let records = restore::read_records(&reporter.records_path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, "session-second");
        // The record is scoped to the owning Herdr session and carries no
        // transcript path, credential, or prompt text.
        assert!(records[0].cwd.starts_with('/'));
        server.join().unwrap();
    }

    #[test]
    fn a_disabled_reporter_never_writes_a_record() {
        let socket = TemporarySocket::new();
        let mut reporter = PaneReporter::disabled();
        reporter.records_dir = Some(socket.directory.join("records"));
        reporter.attach(
            Some("session-1".to_owned()),
            Some("startup"),
            test_launch_scope(),
        );
        reporter.run_started(Some("session-1".to_owned()));
        reporter.finish(false);
        assert!(
            !reporter.records_path().exists(),
            "a reporter without a pane id must not create records"
        );
    }

    #[test]
    fn an_unreachable_socket_never_fails_the_caller() {
        let socket = TemporarySocket::new();
        let reporter = socket_reporter(&socket);
        reporter.attach(Some("s".to_owned()), None, test_launch_scope());
        reporter.run_started(None);
        reporter.note_blocked(tool_id("t1"), "approve?");
        reporter.release();
        assert!(reporter.inner.lock().unwrap().released);
    }

    #[test]
    fn a_stub_cli_transport_receives_the_documented_argv() {
        let socket = TemporarySocket::new();
        let log = socket.directory.join("argv.log");
        let stub = socket.directory.join("herdr-stub.sh");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n",
                log.to_string_lossy()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&stub).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&stub, permissions).unwrap();

        let reporter = PaneReporter::with_transport(
            "w1:p1".to_owned(),
            Some(Transport::Cli { bin: stub.clone() }),
        );
        let mut reporter = reporter;
        reporter.records_dir = Some(socket.directory.join("records"));
        reporter.attach(
            Some("session-9".to_owned()),
            Some("startup"),
            test_launch_scope(),
        );
        reporter.run_started(None);
        reporter.release();

        let logged = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = logged.lines().collect();
        assert_eq!(lines.len(), 4, "session, idle, working, release");
        assert!(lines[0].starts_with("pane report-agent-session w1:p1 --source custom:octet --agent octet --agent-session-id session-9 --session-start-source startup --seq "));
        assert!(lines[1]
            .contains("pane report-agent w1:p1 --source custom:octet --agent octet --state idle"));
        assert!(lines[2].contains("--state working"));
        assert!(lines[3]
            .starts_with("pane release-agent w1:p1 --source custom:octet --agent octet --seq "));
    }
}
