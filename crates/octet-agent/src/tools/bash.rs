//! Bash-compatible command execution with timeout, bounded capture, and child
//! process-tree cleanup.
//!
//! Like Pi, octet always gives the complete command string to one selected shell
//! with `-c`. On Unix the default selection order is `/bin/bash`, `bash` on
//! `PATH`, then `sh`; an explicit host-configured shell path takes precedence.
//! On Windows, only an explicit path or a discovered Git for Windows
//! `bash.exe` is accepted; `cmd.exe`, PowerShell, `$SHELL`, and `COMSPEC` are
//! never implicit fallbacks.

#[cfg(any(unix, windows))]
#[path = "bash_spill.rs"]
mod spill;

#[cfg(any(unix, windows))]
use std::collections::VecDeque;
#[cfg(any(unix, windows))]
use std::path::PathBuf;
#[cfg(any(unix, windows))]
use std::process::Stdio;
#[cfg(any(unix, windows))]
use std::sync::{Arc, Mutex};
#[cfg(any(unix, windows))]
use std::time::{Duration, Instant};

#[cfg(any(unix, windows))]
const POST_KILL_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
/// Leave room for exit/capture metadata inside the per-tool result cap.
#[cfg(any(unix, windows))]
const CAPTURE_ENVELOPE_RESERVE: usize = 256;
use bytes::Bytes;
use octet_ai::ToolDef;
use serde::Deserialize;
#[cfg(any(unix, windows))]
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::effect::{ToolEffect, ToolPolicyDenialCode};
#[cfg(windows)]
use crate::extension_process::WindowsProcessLaunch;
#[cfg(unix)]
use crate::extension_process::{wait_for_bash_process, BashProcessLaunch};
#[cfg(unix)]
use crate::sandbox::resolve_shell;
use crate::tool::{
    OutputStream, PartialOutputCheckpointSink, Tool, ToolContext, ToolError, ToolOutput,
    ToolProgressSink,
};
#[cfg(any(unix, windows))]
use crate::tools::parse_args;
use crate::tools::validate_effect_path;

const MAX_BASH_COMMAND_BYTES: usize = 128 * 1024;
/// Spill storage is independently bounded from the model-visible head/tail.
#[cfg(any(unix, windows))]
const MAX_BASH_SPILL_BYTES: usize = 16 * 1024 * 1024;

/// One Bash-compatible shell request.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashArgs {
    /// The complete command string passed to the selected shell with `-c`.
    command: String,
    /// Optional working directory. Relative paths use the workspace;
    /// trusted-local hosts also accept absolute and `~/` paths.
    cwd: Option<String>,
    /// Optional timeout in milliseconds.
    timeout_ms: Option<u64>,
}

/// The built-in `bash` tool.
///
/// Executes the complete command through a Bash-compatible shell with bounded
/// stdout/stderr capture and a timeout.
/// The child's entire process tree is killed on timeout or cancellation. On
/// Windows, the child is assigned to a private Job Object before it resumes.
/// Spill paths are temporary: this tool owns at most 64 MiB / 32 files,
/// including active captures, with a 16 MiB prefix limit per stream. Oldest
/// retained files are evicted first; retiring the resource owner removes the remainder.
#[derive(Default)]
pub struct BashTool;

impl BashTool {
    /// Release private spill files when the host resource owner is retired.
    /// Cleanup runs off the caller thread; in-flight captures cannot retain
    /// further output in the retired owner's store.
    pub fn release_owner(resource_owner: &str) {
        #[cfg(any(unix, windows))]
        spill::release_owner(resource_owner);
        #[cfg(not(any(unix, windows)))]
        let _ = resource_owner;
    }
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "bash".to_string(),
            description: "Run a command through the configured Bash-compatible shell. \
                          Omit cwd to run at the workspace root. Output reports the exit \
                          status and bounded stdout/stderr. Complete streams end with \
                          complete_<stream>=true; truncated_<stream>=... means bytes \
                          were omitted. Truncated output may include a private full_output_path, \
                          or partial_output_path containing only a prefix if the 16 MiB per-stream \
                          spill limit was reached or storage failed. Spills are limited to 64 MiB / \
                          32 files per resource owner and expire oldest-first or at owner shutdown."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command line to execute."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory relative to the workspace (default: workspace root)."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional timeout in milliseconds."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(
        &self,
        arguments: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        if !ctx.sandbox.allow_process {
            return Err(ToolError::policy_denied(
                ToolPolicyDenialCode::ProcessDisabled,
                "error not_permitted\ncommand execution is disabled by sandbox policy; \
                 arbitrary process execution has shell-equivalent authority and requires \
                 both allow_process=true and allow_shell=true",
            ));
        }
        if !ctx.sandbox.allow_shell {
            return Err(ToolError::policy_denied(
                ToolPolicyDenialCode::ShellDisabled,
                "error not_permitted\ncommand execution is disabled by sandbox policy; \
                 arbitrary process execution has shell-equivalent authority and requires \
                 both allow_process=true and allow_shell=true",
            ));
        }
        let object = arguments
            .as_object()
            .ok_or_else(|| ToolError::new("invalid arguments: expected an object"))?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "command" | "cwd" | "timeout_ms"))
        {
            return Err(ToolError::new("invalid arguments: unknown field"));
        }
        let command = object
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("invalid arguments: `command` must be a string"))?;
        if command.is_empty() {
            return Err(ToolError::new(
                "invalid arguments: command must be non-empty",
            ));
        }
        if command.len() > MAX_BASH_COMMAND_BYTES {
            return Err(ToolError::new(format!(
                "invalid arguments: command is {} bytes (limit {MAX_BASH_COMMAND_BYTES})",
                command.len()
            )));
        }
        if command.contains('\0') {
            return Err(ToolError::new(
                "invalid arguments: command must not contain NUL",
            ));
        }
        if let Some(cwd) = object.get("cwd") {
            let cwd = cwd
                .as_str()
                .ok_or_else(|| ToolError::new("invalid arguments: `cwd` must be a string"))?;
            validate_effect_path(cwd, ctx.sandbox.allow_external_paths)?;
        }
        if object
            .get("timeout_ms")
            .is_some_and(|value| value.as_u64().is_none_or(|timeout| timeout == 0))
        {
            return Err(ToolError::new(
                "invalid arguments: `timeout_ms` must be a positive integer",
            ));
        }
        Ok(ToolEffect::HostProcess)
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("Execute bash commands (prefer rg/ripgrep for file and content search)")
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        #[cfg(windows)]
        {
            self.execute_windows(
                args,
                ctx,
                false,
                &super::ShellSessionEnvironment::default(),
                None,
            )
            .await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (args, ctx);
            Err(ToolError::new(
                "error unsupported_platform\nbash is unavailable on this platform",
            ))
        }
        #[cfg(unix)]
        {
            self.execute_unix(args, ctx, &super::ShellSessionEnvironment::default(), None)
                .await
        }
    }
}

#[cfg(unix)]
impl BashTool {
    pub(super) async fn execute_unix(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
        environment: &super::ShellSessionEnvironment,
        checkpoints: Option<&BashCheckpoints>,
    ) -> Result<ToolOutput, ToolError> {
        self.effect(&args, ctx)?;
        let args: BashArgs = parse_args(args)?;

        let shell = resolve_shell(ctx.sandbox.shell_path.as_deref()).path;
        let mut command = tokio::process::Command::new(&shell);

        // Honour the per-call timeout when present, bounded by sandbox max.
        let effective_timeout = match args.timeout_ms {
            Some(ms) => Duration::from_millis(ms).min(ctx.sandbox.bash_timeout),
            None => ctx.sandbox.bash_timeout,
        };

        let workdir: PathBuf = match args.cwd.as_ref() {
            None => ctx.workspace.to_path_buf(),
            Some(rel) => {
                let display_path = ctx.display_path(rel);
                let dir = ctx.resolve_existing(rel)?;
                if !dir.is_dir() {
                    return Err(ToolError::new(format!(
                        "error invalid_cwd\n{display_path}: not a directory"
                    )));
                }
                dir
            }
        };

        command
            .env_clear()
            .envs(crate::extension_process::sanitized_subprocess_environment())
            .current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        environment.apply(&mut command)?;
        // Put the child in its own process group so cancellation and timeouts
        // can terminate the whole tree, not just the direct child.
        #[cfg(unix)]
        command.process_group(0);
        let launch = BashProcessLaunch::prepare(&mut command, &args.command).map_err(|error| {
            ToolError::new(format!(
                "error spawn\nfailed to prepare shell {} for lifecycle supervision: {error}",
                shell.display()
            ))
        })?;
        command.arg("-c").arg(launch.source());

        let start = Instant::now();
        let mut child = command.spawn().map_err(|e| {
            ToolError::new(format!(
                "error spawn\nfailed to start shell {}: {e}",
                shell.display()
            ))
        })?;
        let (guard, handoff) = match launch.register(child.id()).await {
            Ok(registered) => registered,
            Err(error) => {
                let _ = child.wait().await;
                return Err(ToolError::new(format!(
                    "error spawn\nfailed to register shell {} for lifecycle supervision: {error}",
                    shell.display()
                )));
            }
        };

        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        // Capture each stream up to the complete shared allowance first. Once
        // both byte counts are known, rebalance the retained bytes so an empty
        // or short peer cannot strand half of the advertised result budget.
        let capture_budget = ctx
            .sandbox
            .max_output_bytes
            .saturating_sub(CAPTURE_ENVELOPE_RESERVE);
        let stdout_progress = ctx.progress.clone();
        let stderr_progress = ctx.progress.clone();

        let work = async {
            let (mut out, mut err, status) = tokio::join!(
                read_bounded_with_progress(
                    &mut stdout_pipe,
                    capture_budget,
                    &stdout_progress,
                    OutputStream::Stdout,
                    checkpoints,
                    (ctx.resource_owner, ctx.execution_scope),
                ),
                read_bounded_with_progress(
                    &mut stderr_pipe,
                    capture_budget,
                    &stderr_progress,
                    OutputStream::Stderr,
                    checkpoints,
                    (ctx.resource_owner, ctx.execution_scope),
                ),
                wait_for_bash_process(&mut child, handoff),
            );
            // EOF-only spills share the readers' deadline and drop cancellation.
            rebalance_captures(&mut out, &mut err, capture_budget).await;
            (out, err, status)
        };
        tokio::pin!(work);

        match tokio::time::timeout(effective_timeout, &mut work).await {
            Err(_elapsed) => {
                guard.terminate_now();
                // Preserve final output when ordinary descendants close the
                // pipes promptly, but never let an escaped descendant retain a
                // capture descriptor and defeat the execution deadline. When
                // this bounded drain expires, returning drops `work`, the pipe
                // readers, and the kill-on-drop child handle immediately.
                let drained = tokio::time::timeout(POST_KILL_DRAIN_TIMEOUT, &mut work).await;
                let mut message = format!(
                    "error timeout\ncommand exceeded the {:.0}s execution limit and was killed",
                    effective_timeout.as_secs_f64()
                );
                match drained {
                    Ok((out, err, status)) => {
                        guard.disarm();
                        if out.total_bytes > 0 {
                            message.push('\n');
                            message.push_str(&out.render("stdout"));
                        }
                        if err.total_bytes > 0 {
                            message.push('\n');
                            message.push_str(&err.render("stderr"));
                        }
                        if let Ok(status) = status {
                            use std::os::unix::process::ExitStatusExt;
                            let exit = match (status.code(), status.signal()) {
                                (Some(code), _) => format!("exit={code}"),
                                (None, Some(sig)) => format!("exit=signal:{sig}"),
                                (None, None) => "exit=unknown".to_string(),
                            };
                            message.push_str(&format!("\n{exit}"));
                        }
                    }
                    Err(_) => message.push_str(
                        "\noutput drain abandoned after escaped descendants kept capture pipes open",
                    ),
                }
                Err(ToolError::new(message))
            }
            Ok((out, err, status)) => {
                let status = status.map_err(|e| {
                    ToolError::new(format!("error io\nfailed to wait for command: {e}"))
                })?;
                // The direct child and capture pipes are finished, but a
                // background descendant may have deliberately redirected both
                // streams and remained in the same process group. Transfer the
                // group to the centralized reaper for the rest of this tool's
                // original deadline instead of silently unregistering it.
                guard.supervise_bash_descendants(
                    effective_timeout.saturating_sub(start.elapsed()),
                    ctx.cancellation.clone(),
                );
                let duration = start.elapsed();

                let exit = {
                    use std::os::unix::process::ExitStatusExt;
                    match (status.code(), status.signal()) {
                        (Some(code), _) => format!("exit={code}"),
                        (None, Some(sig)) => format!("exit=signal:{sig}"),
                        (None, None) => "exit=unknown".to_string(),
                    }
                };

                let mut text = format!("{exit} duration={:.2}s", duration.as_secs_f64());
                if out.total_bytes == 0 && err.total_bytes == 0 {
                    text.push_str("\n(no output)");
                } else {
                    if out.total_bytes > 0 {
                        text.push('\n');
                        text.push_str(&out.render("stdout"));
                    }
                    if err.total_bytes > 0 {
                        text.push('\n');
                        text.push_str(&err.render("stderr"));
                    }
                }
                if status.success() {
                    Ok(ToolOutput::new(text))
                } else {
                    Err(ToolError::new(format!("error nonzero_exit\n{text}")))
                }
            }
        }
    }
}

/// Pi's bash checkpoint cadence: at most one durable partial-output checkpoint
/// every two seconds, regardless of how much output the command produces.
#[cfg(any(unix, windows))]
pub const BASH_CHECKPOINT_INTERVAL: Duration = Duration::from_millis(2_000);

/// Smallest accepted checkpoint interval. A host may pace checkpoints faster
/// than Pi's two seconds, but an interval floor keeps storage write rate bounded
/// independently of output volume.
#[cfg(any(unix, windows))]
pub const MIN_BASH_CHECKPOINT_INTERVAL: Duration = Duration::from_millis(10);

/// Hard cap for one durable partial-output snapshot. Pi's bash snapshot is "the
/// last 2,000 lines or 50 KiB"; the two stream sections share this cap, so
/// replacing one invocation's value can never grow with the command's output.
#[cfg(any(unix, windows))]
pub const BASH_CHECKPOINT_MAX_BYTES: usize = 50 * 1024;

/// Marker prepended when a checkpoint snapshot had to drop earlier bytes.
#[cfg(any(unix, windows))]
const CHECKPOINT_ELISION_MARKER: &str = "[earlier output elided]";

/// Bounded interval publisher for durable partial-output checkpoints.
///
/// The tool owns cadence, snapshot bounding, and duplicate suppression; the sink
/// owns storage. `observe` therefore returns a snapshot only when the interval
/// elapsed *and* the complete bounded snapshot differs from the last requested
/// one, exactly like Pi's bash policy (`BASH_CHECKPOINT_INTERVAL_MS = 2_000`,
/// duplicate suppression against the last requested checkpoint).
#[cfg(any(unix, windows))]
#[derive(Clone, Debug)]
pub struct BashCheckpointPublisher {
    interval: Duration,
    last_requested_at: Option<Instant>,
    last_snapshot: Option<String>,
    requested: u64,
    suppressed: u64,
    before_interval: u64,
    failures: u64,
}

/// Observable checkpoint bookkeeping for one bash invocation.
#[cfg(any(unix, windows))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BashCheckpointStats {
    /// Checkpoints handed to the sink.
    pub requested: u64,
    /// Intervals skipped because the bounded snapshot was unchanged.
    pub suppressed: u64,
    /// Observations that arrived before the next interval boundary.
    pub before_interval: u64,
    /// Checkpoint writes the sink refused (storage faults).
    pub failures: u64,
}

#[cfg(any(unix, windows))]
impl BashCheckpointPublisher {
    /// Creates a publisher with `interval` clamped to at least
    /// [`MIN_BASH_CHECKPOINT_INTERVAL`].
    pub fn new(interval: Duration) -> Self {
        Self {
            interval: interval.max(MIN_BASH_CHECKPOINT_INTERVAL),
            last_requested_at: None,
            last_snapshot: None,
            requested: 0,
            suppressed: 0,
            before_interval: 0,
            failures: 0,
        }
    }

    /// Effective cadence.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether a checkpoint may be requested at `now`.
    pub fn is_due(&self, now: Instant) -> bool {
        match self.last_requested_at {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= self.interval,
        }
    }

    /// Records the latest complete bounded snapshot and decides what to persist.
    ///
    /// Returns the snapshot to hand to the sink, or `None` when this observation
    /// is before the interval boundary or repeats the last requested checkpoint.
    /// A suppressed observation still restarts the interval, so output volume can
    /// never increase checkpoint frequency.
    pub fn observe(&mut self, snapshot: &str, now: Instant) -> Option<String> {
        if !self.is_due(now) {
            self.before_interval = self.before_interval.saturating_add(1);
            return None;
        }
        self.last_requested_at = Some(now);
        if self.last_snapshot.as_deref() == Some(snapshot) {
            self.suppressed = self.suppressed.saturating_add(1);
            return None;
        }
        self.last_snapshot = Some(snapshot.to_owned());
        self.requested = self.requested.saturating_add(1);
        Some(snapshot.to_owned())
    }

    /// Records an observation that arrived before the next interval boundary.
    pub fn note_before_interval(&mut self) {
        self.before_interval = self.before_interval.saturating_add(1);
    }

    /// Records that the sink refused the last checkpoint.
    pub fn note_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    /// Bounds one snapshot to [`BASH_CHECKPOINT_MAX_BYTES`].
    ///
    /// Public so a host or tool that builds its own checkpoint snapshot applies
    /// exactly the bound this publisher relies on.
    pub fn bound_snapshot(snapshot: &str) -> String {
        bound_snapshot(snapshot, BASH_CHECKPOINT_MAX_BYTES)
    }

    /// Current bookkeeping.
    pub fn stats(&self) -> BashCheckpointStats {
        BashCheckpointStats {
            requested: self.requested,
            suppressed: self.suppressed,
            before_interval: self.before_interval,
            failures: self.failures,
        }
    }
}

/// Bounds one checkpoint snapshot to `max_bytes` without splitting a UTF-8 code
/// point. Over-long snapshots keep their most recent bytes, because that is what
/// a recovery consumer needs, and are marked as elided.
#[cfg(any(unix, windows))]
fn bound_snapshot(snapshot: &str, max_bytes: usize) -> String {
    if snapshot.len() <= max_bytes {
        return snapshot.to_owned();
    }
    let reserve = CHECKPOINT_ELISION_MARKER.len() + 1;
    let budget = max_bytes.saturating_sub(reserve);
    let mut start = snapshot.len().saturating_sub(budget);
    while start < snapshot.len() && !snapshot.is_char_boundary(start) {
        start += 1;
    }
    format!("{CHECKPOINT_ELISION_MARKER}\n{}", &snapshot[start..])
}

/// Renders one bounded checkpoint section for a stream.
///
/// Deliberately shares no wording with a final result: it never emits
/// `complete_<stream>=true`, so a consumer cannot mistake a checkpoint for proof
/// that the command finished.
#[cfg(any(unix, windows))]
fn render_checkpoint_section(name: &str, capture: &Capture) -> String {
    let total = capture.total_bytes;
    if total == 0 {
        return format!("{name}: 0 bytes seen");
    }
    let head = String::from_utf8_lossy(&capture.head);
    if capture.tail.is_empty() {
        return format!(
            "{name}: {total} bytes seen\n{}",
            head.trim_end_matches('\n')
        );
    }
    let tail_bytes: Vec<u8> = capture.tail.iter().copied().collect();
    let tail = String::from_utf8_lossy(&tail_bytes);
    let head = head.rsplit_once('\n').map(|(kept, _)| kept).unwrap_or("");
    let tail = tail.split_once('\n').map(|(_, kept)| kept).unwrap_or("");
    format!(
        "{name}: {total} bytes seen, showing first and last lines\n{head}\n...\n{}",
        tail.trim_end_matches('\n')
    )
}

/// Host-supplied checkpoint sink plus the bounded pacing state for one bash run.
#[cfg(any(unix, windows))]
pub(super) struct BashCheckpoints {
    sink: Arc<dyn PartialOutputCheckpointSink>,
    state: Mutex<CheckpointState>,
}

#[cfg(any(unix, windows))]
pub(super) struct CheckpointState {
    publisher: BashCheckpointPublisher,
    /// Latest bounded snapshot per stream (`stdout`, `stderr`).
    sections: [Option<String>; 2],
}

#[cfg(any(unix, windows))]
impl BashCheckpoints {
    fn new(sink: Arc<dyn PartialOutputCheckpointSink>, interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            sink,
            state: Mutex::new(CheckpointState {
                publisher: BashCheckpointPublisher::new(interval),
                sections: [None, None],
            }),
        })
    }

    /// Observes the current bounded capture state for one stream.
    ///
    /// Building the snapshot and calling the sink happen only when the interval
    /// elapsed, so an arbitrary amount of output costs at most one bounded
    /// snapshot per interval. The sink call happens outside the pacing lock so a
    /// slow durable write cannot stall the other stream's reader.
    fn observe(&self, stream: OutputStream, capture: &Capture) {
        let now = Instant::now();
        let pending = {
            let mut state = self.lock();
            if !state.publisher.is_due(now) {
                state.publisher.note_before_interval();
                return;
            }
            let (index, name) = match stream {
                OutputStream::Stdout => (0, "stdout"),
                OutputStream::Stderr => (1, "stderr"),
            };
            let section = bound_snapshot(
                &render_checkpoint_section(name, capture),
                BASH_CHECKPOINT_MAX_BYTES / 2,
            );
            state.sections[index] = Some(section);
            let combined = format!(
                "{}\n{}",
                state.sections[0]
                    .as_deref()
                    .unwrap_or("stdout: 0 bytes seen"),
                state.sections[1]
                    .as_deref()
                    .unwrap_or("stderr: 0 bytes seen")
            );
            let combined = bound_snapshot(&combined, BASH_CHECKPOINT_MAX_BYTES);
            state.publisher.observe(&combined, now)
        };
        if let Some(snapshot) = pending {
            if self.sink.checkpoint_partial_output(&snapshot).is_err() {
                // A storage fault is the host's to observe and report; it never
                // changes the command's result and never fabricates settlement.
                self.lock().publisher.note_failure();
            }
        }
    }

    fn stats(&self) -> BashCheckpointStats {
        self.lock().publisher.stats()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CheckpointState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Bash tool whose host supplies durable partial-output checkpoints.
///
/// Row 4.7's delivery mechanism: `ToolContext` carries no durable handle, so a
/// host that wants interval checkpoints injects a sink and a cadence here. An
/// unwrapped [`BashTool`] never checkpoints, so the default behavior is
/// unchanged and no other consumer pays for this.
#[cfg(any(unix, windows))]
pub struct CheckpointedBashTool {
    bash: BashTool,
    checkpoints: Arc<BashCheckpoints>,
}

#[cfg(any(unix, windows))]
impl CheckpointedBashTool {
    /// Wraps `BashTool` with durable checkpoints at `interval` (clamped to at
    /// least [`MIN_BASH_CHECKPOINT_INTERVAL`]).
    pub fn with_checkpoints(
        sink: Arc<dyn PartialOutputCheckpointSink>,
        interval: Duration,
    ) -> Self {
        Self {
            bash: BashTool,
            checkpoints: BashCheckpoints::new(sink, interval),
        }
    }

    /// Wraps `BashTool` with Pi's two-second checkpoint cadence.
    pub fn with_default_checkpoints(sink: Arc<dyn PartialOutputCheckpointSink>) -> Self {
        Self::with_checkpoints(sink, BASH_CHECKPOINT_INTERVAL)
    }

    /// Effective cadence.
    pub fn interval(&self) -> Duration {
        self.checkpoints.lock().publisher.interval()
    }

    /// Checkpoint bookkeeping for the run(s) this tool has executed.
    pub fn checkpoint_stats(&self) -> BashCheckpointStats {
        self.checkpoints.stats()
    }
}

#[cfg(any(unix, windows))]
#[async_trait::async_trait]
impl Tool for CheckpointedBashTool {
    fn definition(&self) -> ToolDef {
        self.bash.definition()
    }

    fn effect(
        &self,
        args: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        self.bash.effect(args, ctx)
    }

    fn replay_safety(&self) -> crate::tool::ReplaySafety {
        self.bash.replay_safety()
    }

    fn concurrency(&self) -> crate::tool::ToolConcurrency {
        self.bash.concurrency()
    }

    fn prompt_snippet(&self) -> Option<&str> {
        self.bash.prompt_snippet()
    }

    fn prompt_guidelines(&self) -> &[&str] {
        self.bash.prompt_guidelines()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        #[cfg(windows)]
        {
            self.bash
                .execute_windows(
                    args,
                    ctx,
                    false,
                    &super::ShellSessionEnvironment::default(),
                    Some(&self.checkpoints),
                )
                .await
        }
        #[cfg(unix)]
        {
            self.bash
                .execute_unix(
                    args,
                    ctx,
                    &super::ShellSessionEnvironment::default(),
                    Some(&self.checkpoints),
                )
                .await
        }
    }
}

#[cfg(windows)]
fn resolve_windows_shell(configured: Option<&std::path::Path>) -> Result<PathBuf, ToolError> {
    if let Some(configured) = configured {
        // An explicit host path is an intentional contract: it must provide
        // Bash-compatible `-c` semantics, but octet does not reinterpret it as
        // cmd.exe or PowerShell.
        return Ok(configured.to_path_buf());
    }

    let mut candidates = Vec::new();
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(variable) {
            candidates.push(PathBuf::from(root).join("Git/bin/bash.exe"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            candidates.push(directory.join("bash.exe"));
            candidates.push(directory.join("bash"));
        }
    }

    candidates
        .into_iter()
        .find(|candidate| candidate.is_file() && !is_legacy_wsl_bash_path(candidate))
        .ok_or_else(|| {
            ToolError::new(
                "error unsupported_platform\nWindows bash execution requires an explicit \
                 Bash-compatible shell_path or Git for Windows bash.exe; cmd.exe, PowerShell, \
                 and legacy WSL bash.exe are not implicit fallbacks",
            )
        })
}

#[cfg(windows)]
fn is_legacy_wsl_bash_path(path: &std::path::Path) -> bool {
    let normalized = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    normalized.ends_with("\\windows\\system32\\bash.exe")
        || normalized.ends_with("\\windows\\sysnative\\bash.exe")
}

#[cfg(windows)]
impl BashTool {
    pub(super) async fn execute_windows(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
        powershell: bool,
        environment: &super::ShellSessionEnvironment,
        checkpoints: Option<&BashCheckpoints>,
    ) -> Result<ToolOutput, ToolError> {
        self.effect(&args, ctx)?;
        let args: BashArgs = parse_args(args)?;
        let shell = if powershell {
            super::powershell::resolve_shell()?
        } else {
            resolve_windows_shell(ctx.sandbox.shell_path.as_deref())?
        };
        let mut command = tokio::process::Command::new(&shell);

        // Honour the per-call timeout when present, bounded by sandbox max.
        let effective_timeout = match args.timeout_ms {
            Some(ms) => Duration::from_millis(ms).min(ctx.sandbox.bash_timeout),
            None => ctx.sandbox.bash_timeout,
        };

        let workdir: PathBuf = match args.cwd.as_ref() {
            None => ctx.workspace.to_path_buf(),
            Some(rel) => {
                let display_path = ctx.display_path(rel);
                let dir = ctx.resolve_existing(rel)?;
                if !dir.is_dir() {
                    return Err(ToolError::new(format!(
                        "error invalid_cwd\n{display_path}: not a directory"
                    )));
                }
                dir
            }
        };

        command
            .env_clear()
            .envs(crate::extension_process::sanitized_subprocess_environment())
            .current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        environment.apply(&mut command)?;
        let launch = WindowsProcessLaunch::bash(&mut command).map_err(|error| {
            ToolError::new(format!(
                "error spawn\nfailed to prepare shell {} for Job Object supervision: {error}",
                shell.display()
            ))
        })?;
        if powershell {
            super::powershell::configure_command(&mut command, &args.command);
        } else {
            command.arg("-c").arg(&args.command);
        }

        let start = Instant::now();
        let mut child = command.spawn().map_err(|error| {
            ToolError::new(format!(
                "error spawn\nfailed to start shell {}: {error}",
                shell.display()
            ))
        })?;
        let guard = match launch.register(&child) {
            Ok(guard) => guard,
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(ToolError::new(format!(
                    "error spawn\nfailed to register shell {} for Job Object supervision: {error}",
                    shell.display()
                )));
            }
        };

        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let capture_budget = ctx
            .sandbox
            .max_output_bytes
            .saturating_sub(CAPTURE_ENVELOPE_RESERVE);
        let stdout_progress = ctx.progress.clone();
        let stderr_progress = ctx.progress.clone();

        let work = async {
            let (mut out, mut err, status) = tokio::join!(
                read_bounded_with_progress(
                    &mut stdout_pipe,
                    capture_budget,
                    &stdout_progress,
                    OutputStream::Stdout,
                    checkpoints,
                    (ctx.resource_owner, ctx.execution_scope),
                ),
                read_bounded_with_progress(
                    &mut stderr_pipe,
                    capture_budget,
                    &stderr_progress,
                    OutputStream::Stderr,
                    checkpoints,
                    (ctx.resource_owner, ctx.execution_scope),
                ),
                child.wait(),
            );
            // EOF-only spills share the readers' deadline and drop cancellation.
            rebalance_captures(&mut out, &mut err, capture_budget).await;
            (out, err, status)
        };
        tokio::pin!(work);

        match tokio::time::timeout(effective_timeout, &mut work).await {
            Err(_elapsed) => {
                guard.terminate_now();
                let drained = tokio::time::timeout(POST_KILL_DRAIN_TIMEOUT, &mut work).await;
                let mut message = format!(
                    "error timeout\ncommand exceeded the {:.0}s execution limit and was killed",
                    effective_timeout.as_secs_f64()
                );
                match drained {
                    Ok((out, err, status)) => {
                        guard.disarm();
                        if out.total_bytes > 0 {
                            message.push('\n');
                            message.push_str(&out.render("stdout"));
                        }
                        if err.total_bytes > 0 {
                            message.push('\n');
                            message.push_str(&err.render("stderr"));
                        }
                        if let Ok(status) = status {
                            let exit = status.code().map_or_else(
                                || "exit=unknown".to_owned(),
                                |code| format!("exit={code}"),
                            );
                            message.push_str(&format!("\n{exit}"));
                        }
                    }
                    Err(_) => message.push_str(
                        "\noutput drain abandoned after a descendant kept a capture pipe open",
                    ),
                }
                Err(ToolError::new(message))
            }
            Ok((out, err, status)) => {
                let status = status.map_err(|error| {
                    ToolError::new(format!("error io\nfailed to wait for command: {error}"))
                })?;
                guard.supervise_bash_descendants(
                    effective_timeout.saturating_sub(start.elapsed()),
                    ctx.cancellation.clone(),
                );
                let duration = start.elapsed();
                let exit = status
                    .code()
                    .map_or_else(|| "exit=unknown".to_owned(), |code| format!("exit={code}"));
                let mut text = format!("{exit} duration={:.2}s", duration.as_secs_f64());
                if out.total_bytes == 0 && err.total_bytes == 0 {
                    text.push_str("\n(no output)");
                } else {
                    if out.total_bytes > 0 {
                        text.push('\n');
                        text.push_str(&out.render("stdout"));
                    }
                    if err.total_bytes > 0 {
                        text.push('\n');
                        text.push_str(&err.render("stderr"));
                    }
                }
                if status.success() {
                    Ok(ToolOutput::new(text))
                } else {
                    Err(ToolError::new(format!("error nonzero_exit\n{text}")))
                }
            }
        }
    }
}

/// Pin the owner's retirement fence from capture admission, even if the writer
/// is only needed after both streams reach EOF.
#[cfg(any(unix, windows))]
struct PendingSpill {
    owner: Arc<spill::OwnerState>,
    scope: String,
    limit: usize,
}

/// Byte-bounded stream capture keeping the head and tail halves of the budget.
#[cfg(any(unix, windows))]
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: usize,
    truncated: bool,
    // Head + tail are the exact provisional stream until the first eviction.
    // No separate prefix buffer or spill worker is needed while output fits.
    pending_spill: Option<PendingSpill>,
    spill: Option<spill::Spill>,
    spill_bytes: usize,
    spill_truncated: bool,
    spill_error: bool,
}

#[cfg(any(unix, windows))]
impl Capture {
    fn empty() -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            total_bytes: 0,
            truncated: false,
            pending_spill: None,
            spill: None,
            spill_bytes: 0,
            spill_truncated: false,
            spill_error: false,
        }
    }

    /// Flush the exact provisional bytes before any head/tail bytes are lost.
    /// The writer splits these borrowed slices into bounded queue messages.
    async fn promote_spill(&mut self) -> Option<spill::Writer> {
        let pending = self.pending_spill.take()?;
        let mut writer = spill::Writer::start(pending.owner, pending.scope, pending.limit);
        writer.chunk(&self.head).await;
        let (first, second) = self.tail.as_slices();
        writer.chunk(first).await;
        writer.chunk(second).await;
        Some(writer)
    }

    fn record_spill(&mut self, outcome: spill::Outcome) {
        self.spill = outcome.spill;
        self.spill_bytes = outcome.bytes;
        self.spill_truncated = outcome.truncated;
        self.spill_error |= outcome.error;
    }

    /// A stream may fit its provisional allowance but not the final shared one.
    /// Materialize it before shrinking, and report whether budgeting must be
    /// repeated to account for the newly known private path length.
    async fn spill_if_truncated(&mut self, budget: usize) -> bool {
        if self.total_bytes > budget {
            if let Some(writer) = self.promote_spill().await {
                self.record_spill(writer.finish().await);
                return true;
            }
        }
        false
    }

    /// Finalize a capture recorded with an equal-or-larger provisional
    /// allowance. If every byte fits, restore the original stream exactly;
    /// otherwise retain balanced head/tail evidence within `budget`.
    fn fit_to_budget(&mut self, budget: usize) {
        if self.total_bytes <= budget {
            self.head.extend(self.tail.drain(..));
            self.truncated = false;
            self.spill = None;
            return;
        }

        let head_cap = budget / 2;
        let tail_cap = budget.saturating_sub(head_cap);
        if self.tail.len() < tail_cap {
            // Shared-budget/path overhead can cut inside the provisional head.
            // Its suffix still belongs in the final tail, not in omitted bytes.
            let needed = tail_cap - self.tail.len();
            for &byte in self.head[self.head.len() - needed..].iter().rev() {
                self.tail.push_front(byte);
            }
        }
        self.head.truncate(head_cap);
        if self.tail.len() > tail_cap {
            self.tail.drain(..self.tail.len() - tail_cap);
        }
        self.truncated = true;
        if let Some(spill) = self.spill.as_mut() {
            spill.retain();
        }
    }

    /// Renders one output section:
    ///
    /// ```text
    /// stdout: 12 lines
    /// <lines>
    /// complete_stdout=true
    /// ```
    ///
    /// or, when the byte budget was exceeded:
    ///
    /// ```text
    /// stdout: 5210240 bytes, showing first N and last M lines
    /// <head lines>
    /// ...
    /// <tail lines>
    /// truncated_stdout=head:N tail:M omitted_bytes:K
    /// ```
    fn render(&self, name: &str) -> String {
        let mut text = self.render_capture(name);
        if self.truncated {
            if let Some(spill) = &self.spill {
                if spill.expired() {
                    text.push_str("\nspill_expired=true (owner retention evicted this output)");
                } else {
                    text.push_str(&format!(
                        "\n{}_output_path={}",
                        if self.spill_error || self.spill_truncated {
                            "partial"
                        } else {
                            "full"
                        },
                        spill.path.display()
                    ));
                }
            }
            if self.spill_truncated {
                text.push_str("\nspill_truncated=true (per-stream byte limit reached)");
            }
            if self.spill_error {
                text.push_str("\nspill_error=true (full output could not be retained)");
            }
        }
        text
    }

    fn render_capture(&self, name: &str) -> String {
        if !self.truncated {
            let text = String::from_utf8_lossy(&self.head);
            let text = text.strip_suffix('\n').unwrap_or(&text);
            let lines = if text.is_empty() {
                0
            } else {
                text.lines().count()
            };
            format!("{name}: {lines} lines\n{text}\ncomplete_{name}=true")
        } else {
            let head = String::from_utf8_lossy(&self.head);
            let tail_bytes = self.tail.iter().copied().collect::<Vec<_>>();
            let tail = String::from_utf8_lossy(&tail_bytes);
            // Drop the partial line at each cut so the output stays line-oriented.
            let head = head.rsplit_once('\n').map(|(kept, _)| kept).unwrap_or("");
            let tail = tail.split_once('\n').map(|(_, kept)| kept).unwrap_or("");
            let tail = tail.strip_suffix('\n').unwrap_or(tail);
            let head_lines = if head.is_empty() {
                0
            } else {
                head.lines().count()
            };
            let tail_lines = if tail.is_empty() {
                0
            } else {
                tail.lines().count()
            };
            let omitted = self.total_bytes - self.head.len() - self.tail.len();
            format!(
                "{name}: {} bytes, showing first {head_lines} and last {tail_lines} lines\n\
                 {head}\n...\n{tail}\n\
                 truncated_{name}=head:{head_lines} tail:{tail_lines} omitted_bytes:{omitted}",
                self.total_bytes
            )
        }
    }
}

/// Reads a pipe to EOF keeping at most `budget` bytes: the first half of the
/// budget verbatim plus a rolling tail of the second half. Forwards every
/// chunk to `progress` so the consumer sees live output.
#[cfg(any(unix, windows))]
async fn read_bounded_with_progress<R: AsyncRead + Unpin>(
    reader: &mut Option<R>,
    budget: usize,
    progress: &ToolProgressSink,
    stream: OutputStream,
    checkpoints: Option<&BashCheckpoints>,
    ownership: (&str, &str),
) -> Capture {
    read_bounded_with_spill_limit(
        reader,
        budget,
        progress,
        stream,
        checkpoints,
        MAX_BASH_SPILL_BYTES,
        ownership,
    )
    .await
}

#[cfg(any(unix, windows))]
async fn read_bounded_with_spill_limit<R: AsyncRead + Unpin>(
    reader: &mut Option<R>,
    budget: usize,
    progress: &ToolProgressSink,
    stream: OutputStream,
    checkpoints: Option<&BashCheckpoints>,
    spill_limit: usize,
    ownership: (&str, &str),
) -> Capture {
    let Some(reader) = reader.as_mut() else {
        return Capture::empty();
    };
    let head_cap = budget / 2;
    let tail_cap = budget.saturating_sub(head_cap);

    let mut capture = Capture::empty();
    capture.pending_spill = Some(PendingSpill {
        owner: spill::owner(ownership.0),
        scope: ownership.1.to_owned(),
        limit: spill_limit,
    });
    let mut writer = None;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Err(_) => {
                capture.spill_error = true;
                break;
            }
            Ok(n) => {
                if writer.is_none() && capture.total_bytes.saturating_add(n) > budget {
                    // Promote before this chunk can evict the first raw byte.
                    writer = capture.promote_spill().await;
                }
                if let Some(writer) = writer.as_mut() {
                    writer.chunk(&buf[..n]).await;
                }
                progress.output(stream, Bytes::copy_from_slice(&buf[..n]));
                capture.total_bytes += n;
                let mut chunk = &buf[..n];
                if capture.head.len() < head_cap {
                    let take = chunk.len().min(head_cap - capture.head.len());
                    capture.head.extend_from_slice(&chunk[..take]);
                    chunk = &chunk[take..];
                }
                if !chunk.is_empty() && tail_cap > 0 {
                    capture.tail.extend(chunk.iter().copied());
                    if capture.tail.len() > tail_cap {
                        let excess = capture.tail.len() - tail_cap;
                        capture.tail.drain(..excess);
                    }
                }
                // Interval-bounded durable checkpoint of the complete bounded
                // snapshot the tool already streams live. Output volume never
                // accelerates checkpoint frequency, and a checkpoint never
                // becomes part of the result.
                if let Some(checkpoints) = checkpoints {
                    checkpoints.observe(stream, &capture);
                }
            }
        }
    }
    if let Some(writer) = writer {
        capture.record_spill(writer.finish().await);
    }
    capture
}

#[cfg(any(unix, windows))]
fn shared_capture_budgets(
    stdout_bytes: usize,
    stderr_bytes: usize,
    budget: usize,
) -> (usize, usize) {
    let stdout_floor = budget / 2;
    let stderr_floor = budget.saturating_sub(stdout_floor);
    let mut stdout_budget = stdout_bytes.min(stdout_floor);
    let mut stderr_budget = stderr_bytes.min(stderr_floor);
    let mut remaining = budget.saturating_sub(stdout_budget.saturating_add(stderr_budget));

    let stdout_extra = stdout_bytes.saturating_sub(stdout_budget).min(remaining);
    stdout_budget = stdout_budget.saturating_add(stdout_extra);
    remaining -= stdout_extra;

    let stderr_extra = stderr_bytes.saturating_sub(stderr_budget).min(remaining);
    stderr_budget = stderr_budget.saturating_add(stderr_extra);
    (stdout_budget, stderr_budget)
}

#[cfg(any(unix, windows))]
async fn rebalance_captures(stdout: &mut Capture, stderr: &mut Capture, budget: usize) {
    loop {
        // Complete inline output needs no spill-path envelope. When truncating,
        // reserve actual private path lengths rather than assuming /tmp is short.
        let inline_budget = if stdout.total_bytes.saturating_add(stderr.total_bytes) > budget {
            let paths = [stdout.spill.as_ref(), stderr.spill.as_ref()]
                .into_iter()
                .flatten()
                .map(|spill| spill.path.as_os_str().len())
                .sum::<usize>();
            let second_section = if stdout.total_bytes > 0 && stderr.total_bytes > 0 {
                128
            } else {
                0
            };
            budget.saturating_sub(paths + second_section)
        } else {
            budget
        };
        let (stdout_budget, stderr_budget) =
            shared_capture_budgets(stdout.total_bytes, stderr.total_bytes, inline_budget);
        let (out_promoted, err_promoted) = tokio::join!(
            stdout.spill_if_truncated(stdout_budget),
            stderr.spill_if_truncated(stderr_budget),
        );
        if !out_promoted && !err_promoted {
            stdout.fit_to_budget(stdout_budget);
            stderr.fit_to_budget(stderr_budget);
            return;
        }
        // A new spill path may itself truncate the peer. Each stream promotes
        // at most once, so at most two more budget passes are needed. Neither
        // capture may discard provisional bytes until this settles.
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::sandbox::SandboxConfig;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;

    struct Fixture {
        _dir: tempfile::TempDir,
        workspace: PathBuf,
        sandbox: SandboxConfig,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        let mut sandbox = SandboxConfig::new(&workspace);
        sandbox.allow_process = true;
        sandbox.allow_shell = true;
        sandbox.bash_timeout = Duration::from_secs(10);
        Fixture {
            _dir: dir,
            workspace,
            sandbox,
        }
    }

    impl Fixture {
        fn ctx(&self) -> ToolContext<'_> {
            ToolContext {
                workspace: &self.workspace,
                sandbox: &self.sandbox,
                execution_scope: "bash-test",
                resource_owner: "bash-test",
                active_skills: &[],
                registered_tools: &[],
                progress: ToolProgressSink::null(),
                cancellation: Default::default(),
            }
        }
    }

    fn process_is_alive(pid: i32) -> bool {
        crate::extension_process::process_is_live_for_test(pid)
    }

    async fn wait_for_process_exit(pid: i32, timeout: Duration) -> bool {
        let started = std::time::Instant::now();
        while process_is_alive(pid) && started.elapsed() < timeout {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        !process_is_alive(pid)
    }

    #[tokio::test]
    async fn spill_quota_keeps_a_prefix_and_drains_the_rest() {
        use tokio::io::AsyncWriteExt;
        let bytes = b"first\nsecond\nthird\nfourth\nfifth\nlast\n";
        let (mut writer, reader) = tokio::io::duplex(4);
        let mut reader = Some(reader);
        let progress = ToolProgressSink::null();
        let drain = read_bounded_with_spill_limit(
            &mut reader,
            12,
            &progress,
            OutputStream::Stdout,
            None,
            9,
            ("quota-test", "quota-call"),
        );
        let write = async {
            writer.write_all(bytes).await.unwrap();
            writer.shutdown().await.unwrap();
        };
        let (mut capture, ()) =
            tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(drain, write) })
                .await
                .expect("quota must not stop pipe draining");
        assert_eq!(capture.total_bytes, bytes.len());
        assert_eq!(capture.spill_bytes, 9);
        assert!(capture.spill_truncated);
        assert!(!capture.spill_error);
        capture.fit_to_budget(12);
        let path = &capture.spill.as_ref().unwrap().path;
        assert_eq!(std::fs::read(path).unwrap(), bytes[..9]);
        let rendered = capture.render("stdout");
        assert!(rendered.contains("partial_output_path="), "{rendered}");
        assert!(rendered.contains("spill_truncated=true"), "{rendered}");
        assert!(!rendered.contains("full_output_path="), "{rendered}");
        assert!(!rendered.contains("spill_error=true"), "{rendered}");
        assert!(
            rendered.contains("last"),
            "tail after quota was lost: {rendered}"
        );
        std::fs::remove_file(path).unwrap();

        // Exactly reaching the cap still preserves a complete spill; only
        // seeing an omitted byte can change the path label to partial.
        let mut reader = Some(std::io::Cursor::new(bytes));
        let mut capture = read_bounded_with_spill_limit(
            &mut reader,
            12,
            &progress,
            OutputStream::Stdout,
            None,
            bytes.len(),
            ("quota-test", "quota-call-2"),
        )
        .await;
        capture.fit_to_budget(12);
        assert!(capture.render("stdout").contains("full_output_path="));
        assert!(!capture.spill_truncated);
        BashTool::release_owner("quota-test");
    }

    #[test]
    fn effect_validates_capabilities_and_arguments_before_approval() {
        let f = fixture();
        assert_eq!(
            BashTool
                .effect(&json!({"command": "printf ok"}), &f.ctx())
                .unwrap(),
            ToolEffect::HostProcess
        );
        for arguments in [
            json!({"command": ""}),
            json!({"command": "echo ok", "cwd": "../outside"}),
            json!({"command": "echo ok", "cwd": "/tmp"}),
            json!({"command": "echo ok", "unknown": true}),
            json!({"command": "echo ok", "timeout_ms": 0}),
            json!({"command": "bad\0command"}),
        ] {
            assert!(
                BashTool.effect(&arguments, &f.ctx()).is_err(),
                "{arguments}"
            );
        }
        assert!(BashTool
            .effect(
                &json!({"command": "x".repeat(MAX_BASH_COMMAND_BYTES + 1)}),
                &f.ctx(),
            )
            .is_err());

        let mut disabled = fixture();
        disabled.sandbox.allow_shell = false;
        let error = BashTool
            .effect(&json!({"command": "printf ok"}), &disabled.ctx())
            .unwrap_err();
        assert!(error.message.contains("allow_shell=true"));
        assert_eq!(
            error.policy_denial_code(),
            Some(ToolPolicyDenialCode::ShellDisabled)
        );

        disabled.sandbox.allow_shell = true;
        disabled.sandbox.allow_process = false;
        let error = BashTool
            .effect(&json!({"command": "printf ok"}), &disabled.ctx())
            .unwrap_err();
        assert!(error.message.contains("allow_process=true"));
        assert_eq!(
            error.policy_denial_code(),
            Some(ToolPolicyDenialCode::ProcessDisabled)
        );
    }

    #[tokio::test]
    async fn every_command_uses_bash_semantics() {
        let f = fixture();
        let out = BashTool
            .execute(
                json!({"command": "printf '%s\\n' brace-{one,two} \"$BASH_VERSION\""}),
                &f.ctx(),
            )
            .await
            .unwrap();
        assert!(out.text.starts_with("exit=0"), "{}", out.text);
        assert!(out.text.contains("brace-one"), "{}", out.text);
        assert!(out.text.contains("brace-two"), "{}", out.text);
        assert!(
            out.text
                .lines()
                .any(|line| line.chars().next().is_some_and(|ch| ch.is_ascii_digit())),
            "BASH_VERSION was empty: {}",
            out.text
        );
    }

    #[tokio::test]
    async fn explicit_shell_path_takes_precedence() {
        use std::os::unix::fs::PermissionsExt;

        let mut f = fixture();
        let shell = f.workspace.join("custom-shell");
        std::fs::write(
            &shell,
            concat!(
                "#!/bin/sh\n",
                "printf 'custom-shell\\n'\n",
                "exec /bin/sh \"$@\"\n",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
        f.sandbox.shell_path = Some(shell);
        let out = BashTool
            .execute(json!({"command": "printf 'command-output\\n'"}), &f.ctx())
            .await
            .unwrap();
        assert!(out.text.contains("custom-shell"), "{}", out.text);
        assert!(out.text.contains("command-output"), "{}", out.text);
    }

    #[tokio::test]
    async fn successful_command_without_descendants_releases_its_registry_entry() {
        let f = fixture();
        BashTool
            .execute(json!({"command": "printf '%s' $$ > leader.pid"}), &f.ctx())
            .await
            .unwrap();
        let leader = std::fs::read_to_string(f.workspace.join("leader.pid"))
            .unwrap()
            .parse::<i32>()
            .unwrap();

        assert!(
            !crate::extension_process::process_group_registered_for_test(leader),
            "a completed process group without descendants remained registered"
        );
    }

    #[tokio::test]
    async fn detached_setsid_descendant_remains_supervised_after_leader_exit() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.bash_timeout = Duration::from_secs(2);
        let cancellation = crate::tool::CancellationToken::default();
        let ctx = ToolContext {
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: cancellation.clone(),
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-detached-descendant-test",
            resource_owner: "bash-detached-descendant-test",
        };

        let output = BashTool
            .execute(
                json!({
                    "command": r#"printf '%s' $$ > leader.pid
mkfifo descendant.ready
python3 -c 'import os,sys,time; os.setsid(); open(sys.argv[1], "w").write(str(os.getpid())); os.write(os.open(sys.argv[2], os.O_WRONLY), b"ready\n"); time.sleep(30)' descendant.pid descendant.ready </dev/null >/dev/null 2>&1 &
IFS= read -r _ < descendant.ready
rm descendant.ready"#
                }),
                &ctx,
            )
            .await
            .unwrap();
        let leader = std::fs::read_to_string(f.workspace.join("leader.pid"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let descendant = std::fs::read_to_string(f.workspace.join("descendant.pid"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let descendant_was_alive = process_is_alive(descendant);
        let group_was_registered =
            crate::extension_process::process_group_registered_for_test(leader);

        cancellation.cancel();
        let descendant_exited = wait_for_process_exit(descendant, Duration::from_secs(2)).await;
        if !descendant_exited {
            unsafe {
                let _ = libc::kill(descendant, libc::SIGKILL);
            }
        }

        assert!(output.text.starts_with("exit=0"), "{}", output.text);
        assert!(
            descendant_was_alive,
            "background descendant exited before supervision could be verified"
        );
        assert!(
            group_was_registered,
            "leader completion prematurely unregistered the descendant process group"
        );
        assert!(
            descendant_exited,
            "background descendant survived cancellation"
        );
        assert!(
            !crate::extension_process::process_group_registered_for_test(leader),
            "cancelled descendant process group remained registered"
        );
    }

    #[tokio::test]
    async fn all_commands_are_rejected_when_unified_shell_authority_is_false() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.allow_shell = false;
        let ctx = ToolContext {
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-permission-test",
            resource_owner: "bash-permission-test",
        };
        for command in [
            "true",
            "true | false",
            "/bin/sh -c 'printf bypass'",
            "python3 -c 'print(1)'",
            "env /bin/sh -c true",
        ] {
            let err = BashTool
                .execute(json!({"command": command}), &ctx)
                .await
                .unwrap_err();
            assert!(err.message.contains("shell-equivalent"), "{command}: {err}");
        }
    }

    #[tokio::test]
    async fn bash_rejected_when_allow_process_false() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.allow_process = false;
        let ctx = ToolContext {
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-permission-test",
            resource_owner: "bash-permission-test",
        };
        let err = BashTool
            .execute(json!({"command": "true"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.message.contains("allow_process"), "{err}");
    }

    #[tokio::test]
    async fn nonzero_exit_and_stderr_are_reported_as_an_error() {
        let f = fixture();
        let error = BashTool
            .execute(json!({"command": "echo oops >&2; exit 3"}), &f.ctx())
            .await
            .unwrap_err();
        assert!(error.message.contains("error nonzero_exit"), "{error}");
        assert!(error.message.contains("exit=3"), "{error}");
        assert!(error.message.contains("stderr: 1 lines\noops"), "{error}");
        assert!(error.message.contains("complete_stderr=true"), "{error}");
        assert!(!error.message.contains("truncated_stderr=false"), "{error}");
    }

    #[tokio::test]
    async fn stdout_uses_the_unused_stderr_capture_budget() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.max_output_bytes = 2048;
        let ctx = ToolContext {
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-shared-budget-test",
            resource_owner: "bash-shared-budget-test",
        };
        let out = BashTool
            .execute(
                json!({"command": "i=0; while [ $i -lt 150 ]; do printf 'abcdefghij\\n'; i=$((i+1)); done"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.text.contains("stdout: 150 lines"), "{}", out.text);
        assert!(out.text.contains("complete_stdout=true"), "{}", out.text);
        assert!(!out.text.contains("truncated_stdout"), "{}", out.text);
    }

    #[tokio::test]
    async fn cwd_is_workspace_bounded() {
        let f = fixture();
        std::fs::create_dir(f.workspace.join("sub")).unwrap();
        let out = BashTool
            .execute(json!({"command": "pwd", "cwd": "sub"}), &f.ctx())
            .await
            .unwrap();
        assert!(out.text.contains("/sub"), "{}", out.text);

        let err = BashTool
            .execute(json!({"command": "pwd", "cwd": "../"}), &f.ctx())
            .await
            .unwrap_err();
        assert!(err.message.contains(".."), "{err}");

        let err = BashTool
            .execute(json!({"command": "pwd", "cwd": "missing"}), &f.ctx())
            .await
            .unwrap_err();
        assert!(err.message.contains("missing"), "{err}");
    }

    #[tokio::test]
    async fn trusted_local_mode_accepts_an_absolute_cwd() {
        let f = fixture();
        let outside = tempfile::tempdir().unwrap();
        let mut sandbox = f.sandbox.clone();
        sandbox.allow_external_paths = true;
        let ctx = ToolContext {
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-external-path-test",
            resource_owner: "bash-external-path-test",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
        };

        let out = BashTool
            .execute(
                json!({"command": "pwd", "cwd": outside.path().to_string_lossy()}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            out.text
                .contains(&outside.path().canonicalize().unwrap().display().to_string()),
            "{}",
            out.text
        );
    }

    #[tokio::test]
    async fn output_is_bounded_with_head_tail_truncation() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.max_output_bytes = 2048;
        let ctx = ToolContext {
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-output-test",
            resource_owner: "bash-output-test",
        };
        let out = BashTool
            .execute(
                json!({"command": "i=0; while [ $i -lt 2000 ]; do echo \"line $i\"; i=$((i+1)); done"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            out.text.len() <= sandbox.max_output_bytes,
            "result exceeded the configured cap: {} bytes",
            out.text.len()
        );
        assert!(out.text.len() < 8192, "output must stay bounded");
        assert!(out.text.contains("truncated_stdout=head:"), "{}", out.text);
        assert!(out.text.contains("omitted_bytes:"), "{}", out.text);
        assert!(out.text.contains("line 0"), "head preserved: {}", out.text);
        assert!(
            out.text.contains("line 1999"),
            "tail preserved: {}",
            out.text
        );
    }

    #[tokio::test]
    async fn timeout_kills_the_child() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.bash_timeout = Duration::from_millis(200);
        let ctx = ToolContext {
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-timeout-test",
            resource_owner: "bash-timeout-test",
        };
        let started = std::time::Instant::now();
        let err = BashTool
            .execute(
                json!({"command": "printf 'partial-before-timeout\\n'; sleep 30"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("timeout"), "{err}");
        assert!(
            err.message.contains("partial-before-timeout"),
            "timeout diagnostics must retain partial output: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout must not wait for the child's natural exit"
        );
    }

    #[tokio::test]
    async fn per_call_timeout_overrides_sandbox() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.bash_timeout = Duration::from_secs(30);
        let ctx = ToolContext {
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-per-call-timeout-test",
            resource_owner: "bash-per-call-timeout-test",
        };
        let started = std::time::Instant::now();
        let err = BashTool
            .execute(json!({"command": "sleep 30", "timeout_ms": 200}), &ctx)
            .await
            .unwrap_err();
        assert!(err.message.contains("timeout"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "per-call timeout must fire before sandbox timeout"
        );
    }

    #[tokio::test]
    async fn timeout_drain_is_bounded_when_escaped_descendant_holds_pipes() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.bash_timeout = Duration::from_millis(100);
        let ctx = ToolContext {
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "bash-escaped-pipe-test",
            resource_owner: "bash-escaped-pipe-test",
        };
        let started = std::time::Instant::now();
        let error = BashTool
            .execute(
                json!({"command": "python3 -c 'import os,time; os.setsid(); open(\"escaped.pid\", \"w\").write(str(os.getpid())); time.sleep(30)' & sleep 30"}),
                &ctx,
            )
            .await
            .unwrap_err();

        if let Ok(pid) = std::fs::read_to_string(f.workspace.join("escaped.pid")) {
            if let Ok(pid) = pid.parse::<i32>() {
                unsafe {
                    let _ = libc::kill(pid, libc::SIGKILL);
                }
            }
        }
        // Either behaviour is acceptable: the escaped descendant may exit
        // quickly enough that the drain succeeds, or it may hold pipes open.
        assert!(
            error.message.contains("output drain abandoned") || error.message.contains("timeout"),
            "{error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "escaped pipe holder defeated the deadline"
        );
    }

    #[tokio::test]
    async fn cancellation_kills_the_child_process_tree() {
        let f = fixture();
        let marker = format!("986{}", std::process::id());
        let args = json!({
            "command": format!("sleep {marker} & wait")
        });

        {
            let ctx = f.ctx();
            let tool = BashTool;
            let bash = tool.execute(args, &ctx);
            tokio::pin!(bash);
            let _ = tokio::time::timeout(Duration::from_millis(500), &mut bash).await;
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
        let check = tokio::process::Command::new("pgrep")
            .args(["-f", &format!("sleep {marker}")])
            .output()
            .await
            .unwrap();
        assert!(
            check.stdout.is_empty(),
            "grandchild survived cancellation: {}",
            String::from_utf8_lossy(&check.stdout)
        );
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn legacy_wsl_bash_paths_are_not_implicit_candidates() {
        assert!(is_legacy_wsl_bash_path(Path::new(
            r"C:\Windows\System32\bash.exe"
        )));
        assert!(is_legacy_wsl_bash_path(Path::new(
            r"C:/Windows/Sysnative/bash.exe"
        )));
        assert!(!is_legacy_wsl_bash_path(Path::new(
            r"C:\Program Files\Git\bin\bash.exe"
        )));
    }

    #[test]
    fn explicit_windows_shell_path_is_not_rewritten() {
        let path = Path::new(r"C:\Program Files\Git\bin\bash.exe");
        let resolved = resolve_windows_shell(Some(path)).expect("explicit shell path");
        assert_eq!(resolved.as_path(), path);
    }
}
