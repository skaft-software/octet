//! Safe hot re-exec for `/reload`.
//!
//! `/reload` keeps its existing transactional resource reload. Only when the
//! executable on disk is no longer the image this process started from does the
//! interactive caller consult this module. The decision core is deliberately
//! self-contained: it never reaches into `App`, the TUI shell, the extension
//! manager, or the session store. Live facts arrive as [`LiveSafetyInputs`], and
//! the two operations this module must not own (a durable session head and a
//! bounded extension shutdown) arrive as [`ReexecHooks`].
//!
//! # Ordering
//!
//! 1. The caller completes its transactional resource reload, then calls
//!    [`ReexecController::observe_generation`] and hands the observed generation
//!    to [`ReexecController::reexec_if_changed`].
//! 2. An unchanged generation returns [`ReexecDecision::ResourcesOnly`] with the
//!    notice `resources reloaded · binary unchanged`.
//! 3. A changed generation is validated before anything is torn down. A
//!    **retargeted** image — one that no longer shares the startup image's file
//!    identity (a moved path or a replaced inode) — first returns
//!    [`ReexecDecision::ConfirmationRequired`] without spawning anything; only
//!    after the caller confirms ([`ReexecOptions::redirect_confirmed`]) is the
//!    candidate probed through `--internal-reexec-probe` in a subprocess with a
//!    hard timeout, the probed generation pinned, and live-state refusal checks
//!    run. Any failure returns `Blocked` or `Refused` and leaves the current
//!    process fully running.
//! 4. [`ReexecHooks::make_session_durable`] commits the session head through the
//!    existing session persistence path, then
//!    [`ReexecHooks::shutdown_extensions`] stops extension children inside the
//!    caller's bound.
//! 5. `ReexecDecision::Ready(plan)`: the caller presents
//!    [`ReexecPlan::notice`], **unwinds the TUI first** (for example
//!    `InteractiveShell::leave`), and only then calls [`ReexecPlan::exec`].
//!    This module never restores the terminal and never calls
//!    `std::process::exit`.
//! 6. `exec` never returns on success. If it returns,
//!    [`ReexecDecision::ExecFailed`] tells the caller to re-enter the TUI,
//!    rebuild extension processes, and leave the terminal usable.
//!
//! # Trust: only a confirmed image is ever executed
//!
//! A re-exec has two executions of the candidate image: the probe subprocess and
//! the `exec` itself. The probe runs first, so it is the candidate's first
//! execution, and it is treated as one: it is spawned only after the live-state
//! refusals passed and only for an image the caller is willing to enter.
//! `/reload` cannot reach this module at all (it is resources-only); the two
//! paths that can are an explicit `/reload --force`, which is itself the user's
//! confirmation, and the `reload_host = true` watcher, which confirms a
//! retargeted image through the interactive picker before the probe starts.
//!
//! # Which executable, and when
//!
//! An install replaces the running image in place, or it moves the *path* that
//! names it: Homebrew retargets a symlink, npm re-points a launcher, a
//! version-pinned directory is swapped for a newer one. [`ExecutableObservation`]
//! is therefore taken at reload time through [`ReexecController`]'s resolver
//! (`std::env::current_exe` in production, injected under test), not by re-stat'ing
//! the path captured at startup, and the *resolved* path travels into the probe,
//! the exec, and the notice. The startup path and its generation stay on the
//! controller so the old build can still be reported, and a path or an
//! inode/ctime change counts as a changed generation. An update that is still in
//! flight (the path no longer resolves, the file is missing or unreadable, the
//! file is not executable) is a distinct, clean `Blocked` notice, never a
//! half-executed image. On Linux an atomic replacement makes the kernel report
//! the captured startup path with ` (deleted)` appended. Only that exact marker
//! is mapped back to the captured path; its replacement still passes every
//! generation, confirmation, probe, and pre-exec check.
//!
//! # Descriptor hygiene
//!
//! `exec` keeps the PID, so an inherited descriptor is not a cosmetic leak.
//! `octet_agent::Session` holds the session file open for the whole run, the
//! append line takes an exclusive `fs2` lock on it for every append
//! (`octet-agent/src/session_writer.rs`), a read-only open holds a shared lock
//! for its lifetime (`session.rs`), and the open path itself takes an exclusive
//! lock while it replays and repairs the tail. An inherited lock or append-line
//! descriptor would therefore let the replacement image contend with — or block
//! forever on — the session it already owns, and an inherited append line could
//! interleave two images into one transcript. [`ReexecPlan::exec`] therefore
//! enumerates every descriptor this process owns above stdio (`/dev/fd` on
//! macOS/BSD, `/proc/self/fd` on Linux; a fixed numeric cap could miss
//! descriptors above it) and confirms `FD_CLOEXEC` on each one
//! ([`seal_process_descriptors`]) immediately before the image is replaced. A
//! listing that cannot be read fails the reload closed. The
//! only descriptors the replacement inherits on purpose are stdin, stdout, and
//! stderr.
//!
//! # Multi-process
//!
//! One reload owns exactly one process. This path takes **no** lock on the
//! executable, creates no lock file, writes no global state, and never touches
//! another process's descriptor or session: the candidate is probed as a
//! subprocess, the plan names one session, and the descriptor sweep only changes
//! this process's own descriptor flags. That is what lets several TUI panes and a
//! running `octet serve` reload independently: locking the executable would
//! serialize — and, for a pane that is itself executing from that path,
//! deadlock — exactly those independent reloaders. A reload in one pane is
//! therefore invisible to every other pane and to the server.
//!
//! # Probe payload
//!
//! [`probe_payload`] is the emitter and [`ProbePayload`] the parser for a
//! single JSON line that identifies octet, its version, and the session-format
//! and extension-API compatibility the emitting build can serve. The early CLI
//! branch that answers `--internal-reexec-probe` must print this line **before**
//! clap parsing, provider setup, extension activation, workspace access,
//! networking, or the agent loop, so the probe is side-effect free.
//!
//! # No probe → exec TOCTOU
//!
//! The generation actually probed is captured before the subprocess starts and
//! re-checked after the probe and again immediately before `exec`; any change
//! returns `Blocked` and the candidate is never executed into.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Arg, Command, CommandFactory};
use serde::{Deserialize, Serialize};

/// The internal, side-effect-free probe flag: `octet --internal-reexec-probe`.
///
/// It is intercepted before clap parsing. A build that does not know it fails
/// clap with an unknown-argument error, which exits nonzero and is therefore
/// reported as a validation failure instead of being exec'd into.
pub(crate) const PROBE_FLAG: &str = "--internal-reexec-probe";

/// The application name every probe payload must carry.
pub(crate) const APPLICATION: &str = "octet";

/// The probe payload schema. Bump when the meaning of a field changes.
pub(crate) const PROBE_SCHEMA: &str = "octet-reexec-probe-v1";

/// Hard deadline for one candidate probe subprocess.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval while waiting for the probe subprocess to exit.
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Upper bound on probe stdout retained for parsing. A candidate that writes
/// more fails the read and is reported as a validation failure.
const PROBE_MAX_OUTPUT_BYTES: u64 = 64 * 1024;

/// Session JSONL compatibility generation served by this build.
///
/// Sessions are append-only JSONL without an on-disk schema-version field. This
/// constant is the re-exec compatibility contract: a candidate probe must
/// report a `session_format` at least this high before this build hands it the
/// live session. Bump it whenever this build can write a session record that an
/// earlier re-exec-capable build could not replay.
pub(crate) const SESSION_FORMAT_VERSION: u32 = 1;

/// User-facing notice strings, in one place so they can be pinned by tests.
pub(crate) mod notice {
    use super::{BlockedReason, ExecutableRedirect, RefusalReason};

    /// The exact `/reload` notice when the running image is still on disk.
    pub(crate) const RESOURCES_ONLY: &str = "resources reloaded · binary unchanged";

    /// Prefix of the notice that asks the user to confirm a retargeted image.
    pub(crate) const CONFIRMATION_PREFIX: &str = "reload needs confirmation · ";

    /// Prefix of the notice presented just before the caller unwinds the TUI.
    pub(crate) const RELOADING_PREFIX: &str = "reloading into build ";

    /// Prefix of the warning presented when live workers are detached instead
    /// of refusing the reload.
    #[cfg(test)]
    pub(crate) const DETACHING_PREFIX: &str = "reload detaching workers · ";

    /// Prefix of every candidate-validation failure notice.
    pub(crate) const BLOCKED_PREFIX: &str = "reload blocked · ";

    /// Prefix of every unsafe-to-reload refusal notice.
    pub(crate) const REFUSED_PREFIX: &str = "reload refused · ";

    /// Prefix of the recovery notice after `exec` returned.
    pub(crate) const EXEC_FAILED_PREFIX: &str =
        "reload failed · the new binary could not take over: ";

    /// `reloading into build <version> at <resolved path>`; the version comes
    /// from the validated candidate probe payload and the path is the resolved
    /// executable that was just probed (and that `exec` will enter).
    pub(crate) fn reloading(version: &str, path: &std::path::Path) -> String {
        format!("{RELOADING_PREFIX}{version} at {}", path.display())
    }

    /// The exact warning for one detaching reload: what happens to the running
    /// workers, that they are reattachable, and how many there are.
    #[cfg(test)]
    pub(crate) fn detaching_workers(count: usize) -> String {
        if count == 1 {
            format!(
                "{DETACHING_PREFIX}1 background worker is being detached and will be reattachable in the new image"
            )
        } else {
            format!(
                "{DETACHING_PREFIX}{count} background workers are being detached and will be reattachable in the new image"
            )
        }
    }

    /// The exact notice for one blocked candidate validation.
    pub(crate) fn blocked(reason: &BlockedReason) -> String {
        format!("{BLOCKED_PREFIX}{}", blocked_detail(reason))
    }

    /// The exact notice for one unsafe live state.
    pub(crate) fn refused(reason: RefusalReason) -> String {
        format!("{REFUSED_PREFIX}{}", refused_detail(reason))
    }

    /// The exact notice for a retargeted image that needs confirmation before
    /// it may even be probed.
    pub(crate) fn confirmation(redirect: &ExecutableRedirect) -> String {
        match redirect {
            ExecutableRedirect::Moved { from, to } => format!(
                "{CONFIRMATION_PREFIX}the executable moved from {} to {}; confirm to probe and enter it",
                from.display(),
                to.display()
            ),
            ExecutableRedirect::Replaced { path } => format!(
                "{CONFIRMATION_PREFIX}the image at {} was replaced (new file identity); confirm to probe and enter it",
                path.display()
            ),
        }
    }

    /// The exact recovery notice after `exec` returned instead of replacing
    /// the image.
    pub(crate) fn exec_failed(error: &std::io::Error) -> String {
        format!("{EXEC_FAILED_PREFIX}{error}")
    }

    fn blocked_detail(reason: &BlockedReason) -> String {
        match reason {
            BlockedReason::ExecutablePathUnresolved(detail) => {
                format!("the running executable path no longer resolves: {detail}")
            }
            BlockedReason::ExecutableUnreadable { path, detail } => {
                format!(
                    "the updated executable at {} could not be read from disk: {detail}",
                    path.display()
                )
            }
            BlockedReason::ExecutableNotExecutable { path } => {
                format!(
                    "the updated executable at {} is not executable yet",
                    path.display()
                )
            }
            BlockedReason::ProbeSpawnFailed(detail) => {
                format!("the candidate binary could not be started: {detail}")
            }
            BlockedReason::ProbeExited { status } => {
                format!("the candidate binary did not answer the probe ({status})")
            }
            BlockedReason::ProbeTimedOut { timeout } => {
                format!("the candidate binary did not answer the probe within {timeout:?}")
            }
            BlockedReason::ProbeMalformed(detail) => {
                format!("the candidate binary returned an unreadable probe: {detail}")
            }
            BlockedReason::ProbeIncompatible(detail) => {
                format!("the candidate binary cannot serve this session: {detail}")
            }
            BlockedReason::CandidateChanged => {
                "the binary on disk changed while it was being verified; run /reload again"
                    .to_owned()
            }
            BlockedReason::DurableHeadFailed(detail) => {
                format!("the active session could not be made durable at its exact head: {detail}")
            }
            BlockedReason::ExtensionShutdownFailed(detail) => {
                format!("extension processes could not be stopped: {detail}")
            }
            BlockedReason::DescriptorHygieneFailed(detail) => {
                format!("octet-owned descriptors could not be made close-on-exec: {detail}")
            }
            BlockedReason::ResumeArgvInvariant(detail) => {
                format!("the resume command could not be rebuilt safely: {detail}")
            }
        }
    }

    fn refused_detail(reason: RefusalReason) -> String {
        match reason {
            RefusalReason::ModelTurnActive => {
                "a model turn is still streaming; wait for it to finish".to_owned()
            }
            RefusalReason::ToolCallActive => "a tool call is still running".to_owned(),
            RefusalReason::ShellChildActive => "a shell command is still running".to_owned(),
            RefusalReason::PendingApprovalOrEffect => "an effect is awaiting approval".to_owned(),
            RefusalReason::SessionPersistenceInFlight => {
                "session persistence is still in flight".to_owned()
            }
            RefusalReason::BackgroundWorkers(1) => {
                "1 background worker is active; stop it from /subagents, or reload with the worker-detach opt-in"
                    .to_owned()
            }
            RefusalReason::BackgroundWorkers(count) => {
                format!(
                    "{count} background workers are active; stop them from /subagents, or reload with the worker-detach opt-in"
                )
            }
            RefusalReason::NoSessionIdentity => {
                "this run has no durable session identity to resume in the new process".to_owned()
            }
        }
    }
}

/// One observed identity of the executable image on disk.
///
/// Cheap, but deliberately stronger than path + size + mtime: on Unix the
/// device, inode, and ctime (nanoseconds) are included, so replacing a file
/// with same-length content is detected even when a coarse filesystem clock
/// makes size and mtime identical. The non-Unix fallback is path + size + mtime
/// plus best-effort creation time; it is weaker because it cannot see an inode,
/// and it is documented as such rather than silently implied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BinaryGeneration {
    path: PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: UnixIdentity,
    #[cfg(not(unix))]
    created: Option<std::time::SystemTime>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UnixIdentity {
    device: u64,
    inode: u64,
    ctime_seconds: i64,
    ctime_nanoseconds: i64,
}

impl BinaryGeneration {
    /// Observe the executable image at `path`.
    ///
    /// Symlinks are followed, because `exec` follows them: the identity that
    /// matters is the target image, not the link that names it.
    pub(crate) fn capture(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self::from_metadata(path, &metadata))
    }

    /// Whether this generation names the same on-disk file identity as `other`.
    ///
    /// On Unix the device and inode are compared, so a file rewritten in place
    /// (same inode, new contents — a plain `cp` over the running binary) shares
    /// the identity while a swapped-in image or a retargeted symlink does not.
    /// Elsewhere no inode exists, so only the path is compared: every same-path
    /// rewrite then counts as the same identity. That fallback is weaker and is
    /// documented rather than implied; hot re-exec itself is Unix-only
    /// ([`platform_exec`]).
    fn shares_identity_with(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.identity.device == other.identity.device
                && self.identity.inode == other.identity.inode
        }
        #[cfg(not(unix))]
        {
            self.path == other.path
        }
    }

    fn from_metadata(path: &Path, metadata: &std::fs::Metadata) -> Self {
        Self {
            path: path.to_path_buf(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                UnixIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    ctime_seconds: metadata.ctime(),
                    ctime_nanoseconds: metadata.ctime_nsec(),
                }
            },
            #[cfg(not(unix))]
            created: metadata.created().ok(),
        }
    }
}

/// Why one path is not a runnable executable image right now.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ExecutableDefect {
    /// The file is missing, unreadable, or otherwise not a regular file.
    Unreadable(String),
    /// The file exists but cannot be executed: no execute bit, or not a regular
    /// file at all.
    NotExecutable,
}

impl ExecutableDefect {
    fn into_blocked(self, path: &Path) -> BlockedReason {
        match self {
            Self::Unreadable(detail) => BlockedReason::ExecutableUnreadable {
                path: path.to_path_buf(),
                detail,
            },
            Self::NotExecutable => BlockedReason::ExecutableNotExecutable {
                path: path.to_path_buf(),
            },
        }
    }
}

/// Capture one executable image, or classify why it cannot be entered.
///
/// This is [`BinaryGeneration::capture`] plus the two checks that turn "the file
/// exists" into "this process may `exec` it": it must be a regular file and it
/// must carry at least one execute bit. The execute-bit check is Unix-only
/// (there is no such bit elsewhere) and is documented rather than silently
/// implied.
fn capture_executable_image(path: &Path) -> Result<BinaryGeneration, ExecutableDefect> {
    let metadata =
        std::fs::metadata(path).map_err(|error| ExecutableDefect::Unreadable(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(ExecutableDefect::NotExecutable);
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_file() {
        return Err(ExecutableDefect::NotExecutable);
    }
    Ok(BinaryGeneration::from_metadata(path, &metadata))
}

/// How the image a re-exec would enter differs from the image this process
/// started from.
///
/// A *redirect* is an image whose file identity no longer matches the startup
/// capture: the resolved path moved (a retargeted `Homebrew` symlink, a swapped
/// versioned directory, an npm launcher re-point) or the same path now names a
/// different inode (a package replaced the file). A plain in-place rewrite of
/// the same inode is *not* a redirect: it is the same file the process is
/// already executing.
///
/// A redirect is never probed or entered without an explicit confirmation from
/// the caller ([`ReexecOptions::redirect_confirmed`]), because the probe is the
/// candidate image's first execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExecutableRedirect {
    /// Resolution now names a different path than the one this process started
    /// from.
    Moved {
        /// The startup path.
        from: PathBuf,
        /// The resolved path a re-exec would enter.
        to: PathBuf,
    },
    /// The path is unchanged, but the file behind it is a different inode.
    Replaced {
        /// The path whose identity changed.
        path: PathBuf,
    },
}

/// One resolved observation of the executable a re-exec would enter.
///
/// The observation is taken at reload time by
/// [`ReexecController::observe_generation`] and names the executable *now*, not
/// the path captured at startup: an update commonly retargets a symlink
/// (Homebrew), re-points a launcher (npm), or swaps a version-pinned directory,
/// and `/reload` must enter the new image rather than re-open the old one. The
/// resolved path travels into the probe, the exec, and the notice; an update
/// that is still in flight resolves to one of the partial variants, each of
/// which is a clean refusal with its own notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExecutableObservation {
    /// The executable resolved and is a runnable image.
    Resolved {
        /// Resolved executable path: exactly what must be probed and executed.
        path: PathBuf,
        /// Identity of the image at `path`.
        generation: BinaryGeneration,
    },
    /// The executable path could not be resolved at all (`current_exe` failed
    /// in production).
    PathUnresolved {
        /// Platform error text.
        detail: String,
    },
    /// The resolved path is missing or cannot be read.
    Unreadable {
        /// Resolved path that could not be read.
        path: PathBuf,
        /// Platform error text.
        detail: String,
    },
    /// The resolved path exists but is not an executable image (yet).
    NotExecutable {
        /// Resolved path that is not executable.
        path: PathBuf,
    },
}

impl ExecutableObservation {
    /// The path and generation about to be probed, or the clean refusal that
    /// replaces them.
    fn candidate(&self) -> Result<(&Path, &BinaryGeneration), BlockedReason> {
        match self {
            Self::Resolved { path, generation } => Ok((path.as_path(), generation)),
            Self::PathUnresolved { detail } => {
                Err(BlockedReason::ExecutablePathUnresolved(detail.clone()))
            }
            Self::Unreadable { path, detail } => Err(BlockedReason::ExecutableUnreadable {
                path: path.clone(),
                detail: detail.clone(),
            }),
            Self::NotExecutable { path } => {
                Err(BlockedReason::ExecutableNotExecutable { path: path.clone() })
            }
        }
    }

    /// Whether this is the exact image — the startup path with the startup file
    /// identity — the process is already running.
    fn is_startup_image(&self, startup_path: &Path, startup: &BinaryGeneration) -> bool {
        match self {
            Self::Resolved { path, generation } => path == startup_path && generation == startup,
            _ => false,
        }
    }
}

/// The classification seam behind [`ReexecController::observe_generation`].
///
/// It maps one resolution attempt — the process's own image in production, an
/// injected candidate under test — into the observation the decision consumes.
fn classify_executable(resolved: std::io::Result<PathBuf>) -> ExecutableObservation {
    let path = match resolved {
        Ok(path) => path,
        Err(error) => {
            return ExecutableObservation::PathUnresolved {
                detail: error.to_string(),
            }
        }
    };
    match capture_executable_image(&path) {
        Ok(generation) => ExecutableObservation::Resolved { path, generation },
        Err(ExecutableDefect::Unreadable(detail)) => {
            ExecutableObservation::Unreadable { path, detail }
        }
        Err(ExecutableDefect::NotExecutable) => ExecutableObservation::NotExecutable { path },
    }
}

/// One raw probe attempt, already classified by the runner.
///
/// Keeping the outcome semantic (rather than exposing `ExitStatus`) lets tests
/// inject every branch deterministically without starting a real octet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProbeOutput {
    /// The candidate exited successfully and wrote `stdout`.
    Completed { stdout: Vec<u8> },
    /// The candidate exited with a nonzero status or a signal; `status` is the
    /// platform's display form.
    FailedExit { status: String },
    /// The candidate exceeded the hard timeout and was killed.
    TimedOut,
    /// The candidate could not be started at all.
    SpawnFailed { detail: String },
}

/// Runs one candidate probe. The production implementation spawns the
/// candidate as a subprocess; tests inject deterministic outcomes.
pub(crate) trait ProbeRunner: Send + Sync {
    fn probe(&self, candidate: &Path, timeout: Duration) -> ProbeOutput;
}

/// The production probe runner: the candidate as a subprocess with a hard
/// deadline and bounded output.
struct SubprocessProbe;

impl ProbeRunner for SubprocessProbe {
    fn probe(&self, candidate: &Path, timeout: Duration) -> ProbeOutput {
        run_probe_process(candidate, timeout)
    }
}

fn run_probe_process(candidate: &Path, timeout: Duration) -> ProbeOutput {
    let mut command = std::process::Command::new(candidate);
    command
        .arg(PROBE_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProbeOutput::SpawnFailed {
                detail: error.to_string(),
            }
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return ProbeOutput::SpawnFailed {
            detail: "candidate probe stdout was unavailable".to_owned(),
        };
    };
    // Drain the pipe from one thread so a candidate that writes a lot cannot
    // deadlock the poll loop; the retained prefix is bounded.
    let reader = match std::thread::Builder::new()
        .name("octet-reexec-probe".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.take(PROBE_MAX_OUTPUT_BYTES).read_to_end(&mut bytes);
            bytes
        }) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return ProbeOutput::SpawnFailed {
                detail: error.to_string(),
            };
        }
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(PROBE_POLL_INTERVAL);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return ProbeOutput::SpawnFailed {
                    detail: error.to_string(),
                };
            }
        }
    };
    match status {
        Some(status) => {
            let stdout = reader.join().unwrap_or_default();
            if status.success() {
                ProbeOutput::Completed { stdout }
            } else {
                ProbeOutput::FailedExit {
                    status: status.to_string(),
                }
            }
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            ProbeOutput::TimedOut
        }
    }
}

/// The one-line probe payload: what a candidate binary can serve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProbePayload {
    /// Always [`APPLICATION`]; a same-shaped payload from another tool is
    /// refused.
    pub(crate) application: String,
    /// Always [`PROBE_SCHEMA`].
    pub(crate) schema: String,
    /// The emitting binary's own version.
    pub(crate) version: String,
    /// Newest session JSONL generation the emitting binary can open and resume.
    pub(crate) session_format: u32,
    /// Extension API protocol version the emitting binary serves.
    pub(crate) extension_api: String,
}

impl ProbePayload {
    /// The payload for this build.
    pub(crate) fn current() -> Self {
        Self {
            application: APPLICATION.to_owned(),
            schema: PROBE_SCHEMA.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            session_format: SESSION_FORMAT_VERSION,
            extension_api: octet_agent::EXTENSION_API_VERSION.to_owned(),
        }
    }

    /// Parse exactly one probe line. Unknown fields are ignored so a newer
    /// candidate can add fields; missing or mistyped fields are malformed.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes)
            .map_err(|error| format!("probe output is not UTF-8: {error}"))?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("probe output was empty".to_owned());
        }
        serde_json::from_str::<Self>(trimmed)
            .map_err(|error| format!("probe output is not a valid payload: {error}"))
    }

    /// Whether this build may hand the live session to the described binary.
    ///
    /// Compatibility is forward-tolerant for session format: a candidate that
    /// reads at least this build's format may resume the session. Extension
    /// children are never handed over (they are shut down first and re-created
    /// from disk), so the extension API version is carried for diagnostics and
    /// must merely be present.
    pub(crate) fn check_compatible(&self) -> Result<(), String> {
        if self.application != APPLICATION {
            return Err(format!(
                "probe application is {:?}, expected {APPLICATION:?}",
                self.application
            ));
        }
        if self.schema != PROBE_SCHEMA {
            return Err(format!(
                "probe schema is {:?}, expected {PROBE_SCHEMA:?}",
                self.schema
            ));
        }
        if self.version.trim().is_empty() {
            return Err("probe carries no version".to_owned());
        }
        if self.extension_api.trim().is_empty() {
            return Err("probe carries no extension API version".to_owned());
        }
        if self.session_format < SESSION_FORMAT_VERSION {
            return Err(format!(
                "it reads session format {} but this session is format {SESSION_FORMAT_VERSION}",
                self.session_format
            ));
        }
        Ok(())
    }
}

/// The emitter for the early CLI probe branch.
///
/// Serialization is infallible for this fixed shape; an unreachable failure
/// emits an empty line, which the caller parses as malformed and blocks on.
/// This function touches no workspace, provider, extension, or session state.
pub(crate) fn probe_payload() -> String {
    let mut line = serde_json::to_string(&ProbePayload::current()).unwrap_or_default();
    line.push('\n');
    line
}

/// Whether this invocation is the internal probe. Arguments after `--` are
/// positional prompts, never a probe request.
pub(crate) fn probe_requested(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|arg| arg.as_os_str() != OsStr::new("--"))
        .any(|arg| arg.as_os_str() == OsStr::new(PROBE_FLAG))
}

/// Live state that makes replacing the process unsafe.
///
/// The caller supplies these facts; this module never reaches into `App`, the
/// shell, extension processes, or the session writer to guess them. A nonempty
/// [`Self::background_workers`] is the count of live delegated workers, not a
/// capability flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LiveSafetyInputs {
    /// A model turn or streaming response owns the run.
    pub(crate) model_turn_active: bool,
    /// A tool call is executing.
    pub(crate) tool_call_active: bool,
    /// A shell child (bash escape or `bash` tool) is live.
    pub(crate) shell_child_active: bool,
    /// An effect is pending approval or awaiting settlement.
    pub(crate) pending_effect: bool,
    /// A session append/checkpoint is still in flight.
    pub(crate) session_persistence_in_flight: bool,
    /// Live background/delegated workers owned by extension children.
    pub(crate) background_workers: usize,
}

impl LiveSafetyInputs {
    /// The first active state, in a fixed order, or `None`.
    #[cfg(test)]
    pub(crate) fn refusal(&self) -> Option<RefusalReason> {
        self.refusal_with(ReexecOptions::default())
    }

    /// The first active state, in the same fixed order, under one set of caller
    /// opt-ins.
    ///
    /// Exactly one input is softened by [`ReexecOptions`]: live delegated
    /// workers become a warning that names their count instead of a refusal.
    /// Every run-owned input (`model_turn_active`, `tool_call_active`,
    /// `shell_child_active`, `pending_effect`,
    /// `session_persistence_in_flight`) keeps its place in the order and stays a
    /// hard refusal, so a detach opt-in can never override "a model turn is
    /// still streaming".
    pub(crate) fn refusal_with(&self, options: ReexecOptions) -> Option<RefusalReason> {
        if self.model_turn_active {
            return Some(RefusalReason::ModelTurnActive);
        }
        if self.tool_call_active {
            return Some(RefusalReason::ToolCallActive);
        }
        if self.shell_child_active {
            return Some(RefusalReason::ShellChildActive);
        }
        if self.pending_effect {
            return Some(RefusalReason::PendingApprovalOrEffect);
        }
        if self.session_persistence_in_flight {
            return Some(RefusalReason::SessionPersistenceInFlight);
        }
        if self.background_workers > 0 && !options.detach_background_workers {
            return Some(RefusalReason::BackgroundWorkers(self.background_workers));
        }
        None
    }
}

/// Caller opt-ins that widen what one `/reload` may do.
///
/// The default — [`Self::default`] — is the only policy that needs no explicit
/// user decision, and it refuses while any worker is live. Every other input
/// stays a hard refusal in the fixed order of
/// [`LiveSafetyInputs::refusal_with`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReexecOptions {
    /// Detach live delegated workers instead of refusing the reload.
    ///
    /// A caller may pass this only after an explicit user decision. The
    /// durable-head hook runs first and flushes every worker record, extension
    /// shutdown then detaches the workers, and the replacement image reattaches
    /// them from their durable records; the warning that names the count comes
    /// from [`notice::detaching_workers`].
    pub(crate) detach_background_workers: bool,
    /// The caller explicitly confirmed entering an image whose file identity no
    /// longer matches the startup image (a retargeted symlink, a swapped inode,
    /// or a different resolved path).
    ///
    /// Without this, [`ReexecController::reexec_if_changed`] returns
    /// [`ReexecDecision::ConfirmationRequired`] **before spawning the probe**,
    /// so an untrusted replacement is never executed as a subprocess just to be
    /// validated. The interactive caller sets it only after the user answered
    /// the confirmation; an explicit `/reload --force` is that user decision,
    /// while the automatic `reload_host = true` path asks first.
    pub(crate) redirect_confirmed: bool,
}

impl ReexecOptions {
    /// The default policy: a live worker refuses the reload, and a retargeted
    /// image needs an explicit confirmation before it is probed.
    pub(crate) fn refusing_workers() -> Self {
        Self::default()
    }

    /// The explicit opt-in: live workers are detached and named in a warning.
    #[cfg(test)]
    pub(crate) fn detaching_workers() -> Self {
        Self {
            detach_background_workers: true,
            ..Self::default()
        }
    }

    /// The explicit opt-in that confirms a retargeted image for this decision.
    pub(crate) fn confirming_redirect(self) -> Self {
        Self {
            redirect_confirmed: true,
            ..self
        }
    }
}

/// Why replacing the process is unsafe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusalReason {
    ModelTurnActive,
    ToolCallActive,
    ShellChildActive,
    PendingApprovalOrEffect,
    SessionPersistenceInFlight,
    BackgroundWorkers(usize),
    /// The controller has no usable session identity to resume, so the
    /// replacement could not re-enter the exact active session.
    NoSessionIdentity,
}

/// Why candidate validation or preparation failed. The current process stays
/// fully running for every one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BlockedReason {
    /// The running executable's path no longer resolves (`current_exe` failed):
    /// an update is mid-flight and no image may be entered.
    ExecutablePathUnresolved(String),
    /// The resolved executable is missing or unreadable: an update is mid-flight.
    ExecutableUnreadable {
        /// Resolved path that could not be read.
        path: PathBuf,
        /// Platform error text.
        detail: String,
    },
    /// The resolved executable carries no execute bit (or is not a regular
    /// file): an update is mid-flight.
    ExecutableNotExecutable {
        /// Resolved path that is not executable.
        path: PathBuf,
    },
    /// The candidate subprocess could not be started.
    ProbeSpawnFailed(String),
    /// The candidate exited nonzero or was killed by a signal.
    ProbeExited { status: String },
    /// The candidate did not answer within the hard timeout.
    ProbeTimedOut { timeout: Duration },
    /// The candidate wrote unparsable or incomplete probe output.
    ProbeMalformed(String),
    /// The candidate answered but cannot serve this session.
    ProbeIncompatible(String),
    /// The on-disk generation changed after it was probed.
    CandidateChanged,
    /// The durable-head hook failed; nothing was torn down.
    DurableHeadFailed(String),
    /// The extension-shutdown hook failed; nothing was exec'd into.
    ExtensionShutdownFailed(String),
    /// The final descriptor sweep could not prove that every octet-owned
    /// descriptor above stdio is close-on-exec; the image was not replaced.
    DescriptorHygieneFailed(String),
    /// The rebuilt resume command failed its own invariant check.
    ResumeArgvInvariant(String),
}

/// The caller-owned operations the decision core must not implement itself.
///
/// Both hooks run only after every refusal and validation check passed and
/// before `Ready` is returned. An error returns `Blocked` and the replacement
/// image is never started.
///
/// With [`ReexecOptions::detach_background_workers`] the pair is also the detach
/// boundary, and this order is the invariant, not a preference: the durable-head
/// hook runs **first** and flushes every worker record, so a worker that has no
/// durable record fails the reload instead of being detached, and only then does
/// the extension shutdown detach the workers that the replacement image will
/// reattach from those records.
// `?Send` because the interactive wiring's hooks borrow the live extension
// manager and the reference implementation is not required to be `Send`.
#[async_trait::async_trait(?Send)]
pub(crate) trait ReexecHooks {
    /// Make the active session durably recoverable at its exact current head.
    ///
    /// The implementation must reuse the existing session persistence path
    /// (`octet_agent::Session`); this module deliberately owns no second state
    /// store and knows nothing about session files. Every semantic session
    /// record is `sync_data`d before its append reports success, so a faithful
    /// implementation only has to observe that the durable head equals
    /// `Session::head_ref()` (for example by re-opening the file).
    ///
    /// In a detaching reload this hook is also responsible for every worker
    /// record: each worker that is about to be detached must already resolve in
    /// its durable roster before this hook returns, and the hook must fail
    /// closed when one does not.
    async fn make_session_durable(&mut self) -> anyhow::Result<()>;

    /// Stop extension children inside the caller's deadline.
    ///
    /// The caller owns both the processes and the bound (the interactive mode's
    /// `shutdown_for_exit` is the reference). The failure mode is fail-closed:
    /// an error or an expired bound returns `Blocked`, so no child is ever
    /// inherited by the replacement image. The hook must leave the caller able
    /// to rebuild extension children after a failed `exec`.
    async fn shutdown_extensions(&mut self) -> anyhow::Result<()>;
}

/// The outcome of the `/reload` decision core.
#[derive(Debug)]
#[must_use]
pub(crate) enum ReexecDecision {
    /// The caller's resource reload is the whole change; keep this process.
    ResourcesOnly,
    /// Candidate validation or preparation failed; the current process is
    /// still fully live.
    Blocked {
        #[cfg_attr(
            not(test),
            expect(
                dead_code,
                reason = "Retain the typed blocked cause alongside its user-safe notice for diagnostic consumers."
            )
        )]
        reason: BlockedReason,
        notice: String,
    },
    /// Live state makes replacing the process unsafe; the current process is
    /// still fully live.
    Refused {
        #[cfg_attr(
            not(test),
            expect(
                dead_code,
                reason = "Retain the typed refusal alongside its user-safe notice for diagnostic consumers."
            )
        )]
        reason: RefusalReason,
        notice: String,
    },
    /// The image on disk no longer shares the startup image's identity (a moved
    /// path or a replaced inode) and the caller did not confirm entering it.
    /// **Nothing was spawned**: the candidate was not probed, and the current
    /// process is fully live.
    ConfirmationRequired {
        /// How the image moved away from the startup image.
        redirect: ExecutableRedirect,
        notice: String,
    },
    /// Everything validated. The caller unwinds the TUI, then calls
    /// [`ReexecPlan::exec`].
    Ready(Box<ReexecPlan>),
    /// `exec` returned, which means the image was not replaced. The caller
    /// must re-enter the TUI, rebuild extension processes, and leave the
    /// terminal usable.
    ExecFailed {
        notice: String,
        #[cfg_attr(
            not(test),
            expect(
                dead_code,
                reason = "Retain the original exec failure for diagnostics; the UI shows its safe notice."
            )
        )]
        source: std::io::Error,
    },
}

impl ReexecDecision {
    pub(crate) fn blocked(reason: BlockedReason) -> Self {
        let notice = notice::blocked(&reason);
        Self::Blocked { reason, notice }
    }

    pub(crate) fn refused(reason: RefusalReason) -> Self {
        let notice = notice::refused(reason);
        Self::Refused { reason, notice }
    }

    pub(crate) fn confirmation(redirect: ExecutableRedirect) -> Self {
        let notice = notice::confirmation(&redirect);
        Self::ConfirmationRequired { redirect, notice }
    }

    pub(crate) fn exec_failed(source: std::io::Error) -> Self {
        let notice = notice::exec_failed(&source);
        Self::ExecFailed { notice, source }
    }

    /// The notice to present, or `None` for `Ready` (whose notice comes from
    /// [`ReexecPlan::notice`]).
    pub(crate) fn notice(&self) -> Option<&str> {
        match self {
            Self::ResourcesOnly => Some(notice::RESOURCES_ONLY),
            Self::Blocked { notice, .. } => Some(notice),
            Self::Refused { notice, .. } => Some(notice),
            Self::ConfirmationRequired { notice, .. } => Some(notice),
            Self::ExecFailed { notice, .. } => Some(notice),
            Self::Ready(_) => None,
        }
    }
}

/// A fully validated replacement: the exact image, argv, and probed
/// generation. The caller unwinds the TUI before calling [`Self::exec`].
#[derive(Debug)]
pub(crate) struct ReexecPlan {
    /// The resolved, probed executable. This is the only path `exec` enters.
    exe: PathBuf,
    /// The image this process started from, retained so the old build can still
    /// be reported after the new path took over.
    startup_exe: PathBuf,
    argv: Vec<OsString>,
    session_id: String,
    version: String,
    probed_generation: BinaryGeneration,
    /// Live workers this plan detaches, or `0` when nothing is detached.
    #[cfg(test)]
    detached_workers: usize,
}

impl ReexecPlan {
    /// The canonical restart argv, including `argv[0]`.
    #[cfg(test)]
    pub(crate) fn argv(&self) -> &[OsString] {
        &self.argv
    }

    /// The exact session this plan resumes.
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The resolved executable this plan enters: exactly the path that was just
    /// probed and re-pinned.
    #[cfg(test)]
    pub(crate) fn executable(&self) -> &Path {
        &self.exe
    }

    /// The executable image this process started from.
    ///
    /// It is kept for reporting the old build; it is never the image `exec`
    /// enters, because an update may have moved the path (an earlier symlink, a
    /// replaced versioned directory).
    pub(crate) fn startup_executable(&self) -> &Path {
        &self.startup_exe
    }

    /// How many live workers this plan detaches (`0` when none are).
    ///
    /// Used by [`Self::detach_notice`] and by the tests that pin the detach
    /// contract; the caller presents the notice, not the number.
    #[cfg(test)]
    fn detached_workers(&self) -> usize {
        self.detached_workers
    }

    /// The warning to present when this plan detaches live workers, or `None`.
    #[cfg(test)]
    pub(crate) fn detach_notice(&self) -> Option<String> {
        (self.detached_workers() > 0).then(|| notice::detaching_workers(self.detached_workers()))
    }

    /// The notice to present before the caller unwinds the TUI.
    ///
    /// When the update moved the executable (a retargeted symlink, a swapped
    /// version-pinned directory) both paths are named, so the reload can be
    /// attributed to the build the process started from and the image it is
    /// entering.
    pub(crate) fn notice(&self) -> String {
        let mut notice = notice::reloading(&self.version, &self.exe);
        if self.startup_exe != self.exe {
            notice.push_str(&format!(
                " (started from {})",
                self.startup_executable().display()
            ));
        }
        notice
    }

    /// Replace this process image. Never returns on success.
    ///
    /// On failure returns [`ReexecDecision::ExecFailed`] with the platform
    /// `io::Error`; the caller re-enters the TUI, rebuilds extension children,
    /// and leaves the terminal usable.
    pub(crate) fn exec(self) -> ReexecDecision {
        self.exec_with(platform_exec)
    }

    /// The injected-exec test seam behind [`Self::exec`].
    pub(crate) fn exec_with<F>(self, exec: F) -> ReexecDecision
    where
        F: FnOnce(&Path, &[OsString]) -> std::io::Error,
    {
        self.exec_with_seal(exec, seal_process_descriptors)
    }

    /// The injected descriptor-seal test seam behind [`Self::exec_with`].
    pub(crate) fn exec_with_seal<F, G>(self, exec: F, seal: G) -> ReexecDecision
    where
        F: FnOnce(&Path, &[OsString]) -> std::io::Error,
        G: FnOnce() -> std::io::Result<()>,
    {
        if let Err(decision) = self.verify_before_exec(seal) {
            return decision;
        }
        let error = self.replace_image(exec);
        ReexecDecision::exec_failed(error)
    }

    /// The platform exec step, returning only on failure.
    fn replace_image<F>(&self, exec: F) -> std::io::Error
    where
        F: FnOnce(&Path, &[OsString]) -> std::io::Error,
    {
        exec(&self.exe, &self.argv)
    }

    /// The final gate before the image is replaced.
    ///
    /// Two things must hold, in this order:
    ///
    /// 1. **Descriptor hygiene.** `exec` keeps the PID, so every octet-owned
    ///    descriptor above stdio must be provably close-on-exec before the
    ///    replacement image exists. An inherited session append line, or an
    ///    inherited `fs2` lock on the session file, would let the new image
    ///    interleave writes into — or block forever on — the session it already
    ///    owns. A sweep that cannot confirm the flags fails the reload closed.
    /// 2. **Probe → exec identity.** The candidate must still be the exact
    ///    generation that was probed *and* still be a runnable image. The plan
    ///    execs the pinned resolved path, never a path resolved again here: a
    ///    symlink retargeted after the probe cannot silently substitute a
    ///    different image.
    ///
    /// No lock is taken on the executable and nothing is written: see the module
    /// docs on the multi-process invariant.
    fn verify_before_exec<G>(&self, seal: G) -> Result<(), ReexecDecision>
    where
        G: FnOnce() -> std::io::Result<()>,
    {
        if let Err(error) = seal() {
            return Err(ReexecDecision::blocked(
                BlockedReason::DescriptorHygieneFailed(error.to_string()),
            ));
        }
        match capture_executable_image(&self.exe) {
            Ok(current) if current == self.probed_generation => Ok(()),
            Ok(_) => Err(ReexecDecision::blocked(BlockedReason::CandidateChanged)),
            Err(defect) => Err(ReexecDecision::blocked(defect.into_blocked(&self.exe))),
        }
    }
}

/// The `/reload` decision core for one process lifetime.
pub(crate) struct ReexecController {
    /// The executable this process started from.
    ///
    /// It is the comparison baseline and the old-build report; it is never the
    /// exec target, because an update can move the path. See
    /// [`ExecutableObservation`].
    startup_exe: PathBuf,
    original_argv: Vec<OsString>,
    startup_generation: BinaryGeneration,
    session_id: Option<String>,
    probe: Arc<dyn ProbeRunner>,
    probe_timeout: Duration,
    /// How this controller names the executable a re-exec would enter.
    ///
    /// Production resolves the process's own image ([`process_executable`],
    /// i.e. `std::env::current_exe`); tests inject a resolver so a decision is
    /// decided by the fixture the test owns and never by the harness binary
    /// that happens to be running it.
    resolver: ExecutableResolver,
}

/// The resolver behind [`ReexecController::observe_generation`].
///
/// It is a seam rather than a direct `std::env::current_exe()` call because
/// resolution is the one input of a re-exec decision that comes from the
/// process instead of from the caller: injecting it keeps the decision
/// deterministic under test while production keeps the real answer.
type ExecutableResolver = Arc<dyn Fn() -> std::io::Result<PathBuf> + Send + Sync>;

/// Resolve this process's own executable: the production resolver.
fn process_executable() -> std::io::Result<PathBuf> {
    std::env::current_exe()
}

/// Linux appends this marker to `/proc/self/exe` after an atomic replacement.
/// Recover only the already captured launch path, not arbitrary suffixed paths.
/// Its new generation still goes through the ordinary replacement trust gate.
#[cfg(any(target_os = "linux", test))]
fn linux_reload_path(startup: &Path, reported: PathBuf) -> PathBuf {
    let mut deleted = startup.as_os_str().to_owned();
    deleted.push(" (deleted)");
    if reported.as_os_str() == deleted.as_os_str() {
        startup.to_path_buf()
    } else {
        reported
    }
}

impl ReexecController {
    /// Capture the running image, the original invocation, and its generation.
    ///
    /// The session id is deliberately not captured here: it does not exist yet
    /// and must never be guessed. The caller sets it later through
    /// [`Self::set_session_id`] once the session exists.
    pub(crate) fn capture() -> std::io::Result<Self> {
        let startup_exe = process_executable()?;
        let startup_generation = BinaryGeneration::capture(&startup_exe)?;
        let resolver: ExecutableResolver = Arc::new(process_executable);
        Ok(Self {
            startup_exe,
            original_argv: std::env::args_os().collect(),
            startup_generation,
            session_id: None,
            probe: Arc::new(SubprocessProbe),
            probe_timeout: PROBE_TIMEOUT,
            resolver,
        })
    }

    /// Record the exact active session to resume, once the session exists.
    pub(crate) fn set_session_id(&mut self, session_id: impl Into<String>) {
        self.session_id = Some(session_id.into());
    }

    /// The executable image captured at startup.
    #[cfg(test)]
    pub(crate) fn executable(&self) -> &Path {
        &self.startup_exe
    }

    /// Resolve the executable a re-exec would enter, after the caller's
    /// transactional resource reload.
    ///
    /// This resolves the executable *now* rather than re-stat'ing the startup
    /// path, and it reports a partial update (see [`ExecutableObservation`])
    /// instead of an opaque `io::Error`, so each in-flight update case gets its
    /// own notice. The observation is the only source of the candidate path: it
    /// is the path the probe, the exec, and the notice use.
    pub(crate) fn observe_generation(&self) -> ExecutableObservation {
        let reported = (self.resolver)();
        #[cfg(target_os = "linux")]
        let reported = reported.map(|path| linux_reload_path(&self.startup_exe, path));
        classify_executable(reported)
    }

    /// Decide whether `/reload` stays in this process or prepares a re-exec.
    ///
    /// `observed` is what the caller resolved after its resource reload. An
    /// unchanged image at the startup path returns
    /// [`ReexecDecision::ResourcesOnly`]. Anything else re-pins the observed
    /// candidate, runs candidate validation, live-state refusal under
    /// `options`, the redirect confirmation gate, the durable-head hook, the
    /// bounded extension-shutdown hook, and the canonical argv build, in that
    /// order; nothing is torn down before the candidate validates, no probe
    /// result is exec'd into before the pinned generation was re-checked, and a
    /// retargeted image (moved path or replaced inode) is never probed until
    /// [`ReexecOptions::redirect_confirmed`] says the caller confirmed entering
    /// it.
    pub(crate) async fn reexec_if_changed(
        &self,
        observed: &ExecutableObservation,
        safety: &LiveSafetyInputs,
        options: ReexecOptions,
        hooks: &mut dyn ReexecHooks,
    ) -> ReexecDecision {
        // The caller's observation is the trigger: the startup path with the
        // startup identity needs no re-exec. A path that differs is already a
        // change (the same file reached through a new path is a new image to
        // enter), and the partial observations are the update-in-progress cases
        // below.
        if observed.is_startup_image(&self.startup_exe, &self.startup_generation) {
            return ReexecDecision::ResourcesOnly;
        }
        let candidate = match observed.candidate() {
            Ok((candidate, _observed_generation)) => candidate.to_path_buf(),
            Err(reason) => return ReexecDecision::blocked(reason),
        };
        // The caller's read is only a trigger. Re-pin the generation that is
        // about to be probed, so the probe is validated against the state at
        // spawn time; a disk that reverted to the running image is unchanged.
        let probed = match capture_executable_image(&candidate) {
            Ok(generation) => generation,
            Err(defect) => return ReexecDecision::blocked(defect.into_blocked(&candidate)),
        };
        if candidate == self.startup_exe && probed == self.startup_generation {
            return ReexecDecision::ResourcesOnly;
        }
        // Refuse before spawning anything: the cheapest actionable notice wins
        // while live work still owns the process. The detach opt-in softens only
        // the worker input; the order of the rest is fixed.
        if let Some(reason) = safety.refusal_with(options) {
            return ReexecDecision::refused(reason);
        }
        let Some(session_id) = self.session_id.as_deref() else {
            return ReexecDecision::refused(RefusalReason::NoSessionIdentity);
        };
        if !session_identity_is_usable(session_id) {
            return ReexecDecision::refused(RefusalReason::NoSessionIdentity);
        }
        // Trust gate before the probe: the probe is the candidate image's first
        // execution, so a retargeted image (a different path or a replaced
        // inode) is never spawned as a subprocess just to be validated. The
        // caller must confirm entering it first.
        if let Some(redirect) = self.redirect(&candidate, &probed) {
            if !options.redirect_confirmed {
                return ReexecDecision::confirmation(redirect);
            }
        }
        let payload = match self.probe_candidate(&candidate) {
            Ok(payload) => payload,
            Err(reason) => return ReexecDecision::blocked(reason),
        };
        // No probe → teardown TOCTOU: the probed generation must still be the
        // on-disk generation before the durable head or extension shutdown runs.
        match capture_executable_image(&candidate) {
            Ok(current) if current == probed => {}
            Ok(_) => return ReexecDecision::blocked(BlockedReason::CandidateChanged),
            Err(defect) => return ReexecDecision::blocked(defect.into_blocked(&candidate)),
        }
        let argv = match canonical_resume_argv(&self.original_argv, &candidate, session_id) {
            Ok(argv) => argv,
            Err(detail) => {
                return ReexecDecision::blocked(BlockedReason::ResumeArgvInvariant(detail))
            }
        };
        // Durable head first, detach second: with the worker opt-in this hook is
        // what makes every worker record durable before the workers are
        // detached, and it fails the reload when it cannot.
        if let Err(error) = hooks.make_session_durable().await {
            return ReexecDecision::blocked(BlockedReason::DurableHeadFailed(format!("{error:#}")));
        }
        if let Err(error) = hooks.shutdown_extensions().await {
            return ReexecDecision::blocked(BlockedReason::ExtensionShutdownFailed(format!(
                "{error:#}"
            )));
        }
        ReexecDecision::Ready(Box::new(ReexecPlan {
            exe: candidate,
            startup_exe: self.startup_exe.clone(),
            argv,
            session_id: session_id.to_owned(),
            version: payload.version,
            probed_generation: probed,
            #[cfg(test)]
            detached_workers: if options.detach_background_workers {
                safety.background_workers
            } else {
                0
            },
        }))
    }

    /// How `candidate`/`probed` differs from the image this process started
    /// from, if it does.
    ///
    /// A different resolved path is a move; the same path with a different file
    /// identity is a replacement. A same-path in-place rewrite of the same inode
    /// is not a redirect and needs no confirmation.
    fn redirect(&self, candidate: &Path, probed: &BinaryGeneration) -> Option<ExecutableRedirect> {
        if candidate != self.startup_exe {
            return Some(ExecutableRedirect::Moved {
                from: self.startup_exe.clone(),
                to: candidate.to_path_buf(),
            });
        }
        (!probed.shares_identity_with(&self.startup_generation)).then(|| {
            ExecutableRedirect::Replaced {
                path: candidate.to_path_buf(),
            }
        })
    }

    fn probe_candidate(&self, candidate: &Path) -> Result<ProbePayload, BlockedReason> {
        match self.probe.probe(candidate, self.probe_timeout) {
            ProbeOutput::SpawnFailed { detail } => Err(BlockedReason::ProbeSpawnFailed(detail)),
            ProbeOutput::TimedOut => Err(BlockedReason::ProbeTimedOut {
                timeout: self.probe_timeout,
            }),
            ProbeOutput::FailedExit { status } => Err(BlockedReason::ProbeExited { status }),
            ProbeOutput::Completed { stdout } => {
                let payload =
                    ProbePayload::decode(&stdout).map_err(BlockedReason::ProbeMalformed)?;
                payload
                    .check_compatible()
                    .map_err(BlockedReason::ProbeIncompatible)?;
                Ok(payload)
            }
        }
    }
}

/// Replace this process image, returning only on failure.
///
/// Unix uses `std::os::unix::process::CommandExt::exec`; argv[0] is preserved
/// and the working directory is inherited, so workspace/config resolution in
/// the replacement is identical. Only stdin/stdout/stderr are intentionally
/// inherited (see [`ensure_close_on_exec`]). Other hosts return an unsupported
/// error, which the caller reports through `ExecFailed` and recovers from.
pub(crate) fn platform_exec(exe: &Path, argv: &[OsString]) -> std::io::Error {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new(exe);
        if let Some(arg0) = argv.first() {
            command.arg0(arg0);
        }
        command.args(argv.iter().skip(1));
        command.exec()
    }
    #[cfg(not(unix))]
    {
        let _ = (exe, argv);
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "hot re-exec is implemented only on Unix hosts",
        )
    }
}

/// Lowest descriptor number [`seal_process_descriptors`] touches.
///
/// Descriptors 0, 1, and 2 are stdin, stdout, and stderr, which the replacement
/// image inherits on purpose: [`platform_exec`] spawns no shell and reopens no
/// terminal, so the new image must keep the terminal it was given.
#[cfg(unix)]
const FIRST_SEALED_DESCRIPTOR: libc::c_int = 3;

/// Directory the sweep lists to discover every descriptor this process owns.
///
/// `fdescfs` on macOS/BSD and `procfs` on Linux both expose one numeric entry
/// per open descriptor, including duplicates and numbers far above any fixed
/// cap. Enumeration is the point: the previous numeric sweep stopped at 4096,
/// so a descriptor above that cap (a raised `RLIMIT_NOFILE`, an extension pipe
/// numbered late) would have been silently inherited by the replacement image.
#[cfg(unix)]
fn descriptor_directory() -> &'static str {
    if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    }
}

/// Every descriptor number this process currently owns, sorted and deduplicated.
///
/// A listing that cannot be read, or that contains an entry this code cannot
/// name, is an error: a descriptor that cannot be listed cannot be proven
/// close-on-exec, and the reload must fail closed rather than hand it to the
/// replacement image.
#[cfg(unix)]
fn open_descriptor_numbers() -> std::io::Result<Vec<libc::c_int>> {
    let directory = descriptor_directory();
    let entries = std::fs::read_dir(directory).map_err(|error| {
        std::io::Error::other(format!("cannot list descriptors in {directory}: {error}"))
    })?;
    let mut descriptors = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            std::io::Error::other(format!("cannot read a {directory} entry: {error}"))
        })?;
        let name = entry.file_name();
        let parsed = name
            .to_str()
            .and_then(|name| name.parse::<libc::c_int>().ok());
        let Some(descriptor) = parsed else {
            return Err(std::io::Error::other(format!(
                "cannot enumerate descriptors: {directory} contains the non-numeric entry {name:?}"
            )));
        };
        descriptors.push(descriptor);
    }
    descriptors.sort_unstable();
    descriptors.dedup();
    Ok(descriptors)
}

/// Verify that one octet-owned descriptor is close-on-exec, setting the flag
/// when it is not.
///
/// The replacement process intentionally inherits **only** stdin, stdout, and
/// stderr; every other octet-owned descriptor (the session append line, the
/// `fs2` session lock, a log file, an extension pipe) must be `CLOEXEC` so it
/// cannot leak across `exec`. `exec` keeps the PID, so the leak this guards
/// against is not cosmetic: an inherited append line could interleave a second
/// image into one transcript, and an inherited `fs2` lock (an in-flight append,
/// a read-only open's shared lock, or the exclusive lock the session open path
/// takes while it repairs the tail) would leave the replacement image blocking
/// on the session it already owns.
///
/// This helper checks and repairs one descriptor the caller already owns.
/// [`seal_process_descriptors`] is the same check for the descriptors a caller
/// cannot name — notably the session and lock descriptors, which
/// `octet_agent::Session` does not hand out — and it is what
/// [`ReexecPlan::exec`] runs immediately before the image is replaced.
///
/// Returns `Ok(true)` when the descriptor was already close-on-exec and
/// `Ok(false)` when this call set it.
#[cfg(unix)]
#[cfg(test)]
pub(crate) fn ensure_close_on_exec(
    descriptor: std::os::fd::BorrowedFd<'_>,
) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;

    let raw = descriptor.as_raw_fd();
    match descriptor_close_on_exec_state(raw)? {
        Some(true) => Ok(true),
        // The caller holds this descriptor, so it cannot be closed.
        None => Err(std::io::Error::from_raw_os_error(libc::EBADF)),
        Some(false) => {
            seal_raw_descriptor(raw)?;
            Ok(false)
        }
    }
}

/// Make every descriptor this process still owns close-on-exec, or fail closed.
///
/// This is the invariant [`ReexecPlan::verify_before_exec`] enforces: the process
/// that is about to be replaced must not hand the image that inherits its PID a
/// single octet-owned session, session-lock, log, or extension descriptor. The
/// sweep repairs rather than trusts: every descriptor listed by
/// [`open_descriptor_numbers`] above stdio is examined, `FD_CLOEXEC` is set when
/// it is missing, and the flag is read back to confirm it. A descriptor that
/// vanishes mid-sweep was closed by another thread and can no longer be
/// inherited, so it is not an error; a descriptor that is open and still not
/// close-on-exec after the set fails the reload. A listing that cannot be read
/// fails the reload as well: an fd the sweep cannot see cannot be proven sealed.
///
/// Two listing passes are made, so a descriptor opened while the first pass ran
/// is still seen and sealed (the second pass finds it; descriptors already seen
/// are re-verified, and a `None` read-back means it vanished). There is no
/// numeric upper bound to miss: the listing is the process's actual descriptor
/// table, not a guess from `RLIMIT_NOFILE`.
///
/// Nothing is written, no lock is taken, and no descriptor is closed: only this
/// process's own descriptor flags change. That is what keeps a reload private to
/// one pane — see the module docs on the multi-process invariant — and it is also
/// why the executable itself is never locked: a lock on the image would
/// serialize (and deadlock) the independent reloaders.
#[cfg(unix)]
pub(crate) fn seal_process_descriptors() -> std::io::Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    // The pass after the last newly-seen descriptor only re-verifies; two
    // iterations are the common case, and the third bounds a thread that keeps
    // opening descriptors while the sweep runs.
    for _ in 0..3 {
        let mut fresh = false;
        for descriptor in open_descriptor_numbers()? {
            if descriptor < FIRST_SEALED_DESCRIPTOR {
                continue;
            }
            // A new number must be sealed; a number already seen must still be
            // sealed (it may have been closed and reopened in between).
            fresh |= seen.insert(descriptor);
            seal_one_descriptor(descriptor)?;
        }
        if !fresh {
            break;
        }
    }
    Ok(())
}

/// Seal one already-listed descriptor and prove the seal held.
#[cfg(unix)]
fn seal_one_descriptor(descriptor: libc::c_int) -> std::io::Result<()> {
    match descriptor_close_on_exec_state(descriptor)? {
        // Not open (or closed by another thread since listing).
        None => {}
        // Already sealed.
        Some(true) => {}
        Some(false) if seal_raw_descriptor(descriptor)? => {
            match descriptor_close_on_exec_state(descriptor)? {
                Some(true) => {}
                // Closed between the set and the read-back: nothing is left to inherit.
                None => {}
                Some(false) => {
                    return Err(std::io::Error::other(format!(
                        "descriptor {descriptor} is open and still not close-on-exec"
                    )))
                }
            }
        }
        Some(false) => {}
    }
    Ok(())
}

/// Descriptor hygiene is a Unix concept; hosts without it never reach
/// [`ReexecPlan::exec`] at all (see [`platform_exec`]).
#[cfg(not(unix))]
pub(crate) fn seal_process_descriptors() -> std::io::Result<()> {
    Ok(())
}

/// `Ok(None)` when the descriptor is not open at all, `Ok(Some(flag))` when it is.
#[cfg(unix)]
fn descriptor_close_on_exec_state(descriptor: libc::c_int) -> std::io::Result<Option<bool>> {
    // SAFETY: F_GETFD reads the flag set of one descriptor number; no pointer is
    // passed and no memory is touched beyond the integer result.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        if errno_is_vanish() {
            return Ok(None);
        }
        return Err(std::io::Error::last_os_error());
    }
    Ok(Some(flags & libc::FD_CLOEXEC != 0))
}

/// Set `FD_CLOEXEC` on one raw descriptor.
///
/// Returns `Ok(false)` when the descriptor vanished between the read and the
/// set: it cannot be inherited and needs no repair.
#[cfg(unix)]
fn seal_raw_descriptor(descriptor: libc::c_int) -> std::io::Result<bool> {
    // SAFETY: F_GETFD/F_SETFD read and update only this one descriptor's flags.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        if errno_is_vanish() {
            return Ok(false);
        }
        return Err(std::io::Error::last_os_error());
    }
    if flags & libc::FD_CLOEXEC != 0 {
        return Ok(true);
    }
    // SAFETY: same contract as the F_GETFD call above; `FD_CLOEXEC` is the only
    // flag in this flag set.
    let result = unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result < 0 {
        if errno_is_vanish() {
            return Ok(false);
        }
        return Err(std::io::Error::last_os_error());
    }
    Ok(true)
}

/// Whether the current `errno` means the descriptor vanished (another thread
/// closed it mid-sweep) rather than a real failure.
#[cfg(unix)]
fn errno_is_vanish() -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
}

/// A session identity that can be passed as the value of `--resume` without
/// being re-read as an option.
fn session_identity_is_usable(session_id: &str) -> bool {
    !session_id.is_empty() && !session_id.starts_with('-')
}

/// The arguments that select a session on the command line. Every one of them
/// is removed before exactly one `--resume <current>` is appended.
const SESSION_SELECTION_FLAGS: &[&str] =
    &["continue", "resume", "fork", "session-id", "no-session"];

/// Internal invocation flags that must never survive into the replacement.
const INTERNAL_REMOVED_FLAGS: &[&str] = &["reload", "internal-reexec-probe"];

/// Build the canonical restart argv.
///
/// Ordinary options and their values are preserved; every session-selection
/// argument (`--continue`, `--resume` bare/valued/`=`, `--fork`, `--session-id`,
/// `--no-session`) is removed; positional prompts are dropped because the
/// resumed session already contains every submitted prompt; and exactly one
/// `--resume <session_id>` is appended. Option arity is read from the real
/// clap command ([`crate::cli::Cli`]), so a newly added static option cannot
/// silently drift. Unknown (extension-declared) long flags conservatively keep
/// one following non-option token as their value.
fn canonical_resume_argv(
    original: &[OsString],
    fallback_program: &Path,
    session_id: &str,
) -> Result<Vec<OsString>, String> {
    let command = crate::cli::Cli::command();
    let mut argv: Vec<OsString> = Vec::with_capacity(original.len() + 2);
    argv.push(
        original
            .first()
            .cloned()
            .unwrap_or_else(|| fallback_program.as_os_str().to_owned()),
    );
    let mut index = 1;
    while index < original.len() {
        let token = &original[index];
        index += 1;
        let Some(text) = token.to_str() else {
            // A non-UTF-8 token cannot be an option name; it is a value or a
            // positional prompt, and positional prompts are dropped.
            continue;
        };
        if text == "--" {
            // Everything after the terminator is positional.
            break;
        }
        if text == "-" {
            continue;
        }
        if let Some(name) = text.strip_prefix("--") {
            let (name, inline_value) = match name.split_once('=') {
                Some((name, _value)) => (name, true),
                None => (name, false),
            };
            // `--option=value` already carries its value, so a following
            // token is a positional prompt, not this option's value.
            let maximum = if inline_value {
                0
            } else {
                static_long_argument(&command, name)
                    .map(static_argument_value_maximum)
                    .unwrap_or(1)
            };
            let removed =
                SESSION_SELECTION_FLAGS.contains(&name) || INTERNAL_REMOVED_FLAGS.contains(&name);
            let values = option_value_tokens(original, index, maximum);
            if !removed {
                argv.push(token.clone());
                argv.extend(original[index..index + values].iter().cloned());
            }
            index += values;
            continue;
        }
        if text.starts_with('-') && text.len() > 1 {
            let short = text.chars().nth(1);
            let attached_value = text.chars().count() > 2;
            let maximum = if attached_value {
                0
            } else {
                short
                    .and_then(|short| static_short_argument(&command, short))
                    .map(static_argument_value_maximum)
                    .unwrap_or(1)
            };
            argv.push(token.clone());
            let values = option_value_tokens(original, index, maximum);
            argv.extend(original[index..index + values].iter().cloned());
            index += values;
            continue;
        }
        // Positional prompt: the resumed session already contains it.
    }
    argv.push(OsString::from("--resume"));
    argv.push(OsString::from(session_id));
    if !argv_resumes_session(&argv, session_id) {
        return Err("the rebuilt argv does not end in exactly one --resume <session>".to_owned());
    }
    Ok(argv)
}

/// Tokens clap would treat as the value of an option: non-option tokens, up to
/// `maximum`. Mirrors `cli::consume_static_values`.
fn option_value_tokens(args: &[OsString], start: usize, maximum: usize) -> usize {
    let mut consumed = 0;
    while start + consumed < args.len() && consumed < maximum {
        let Some(value) = args[start + consumed].to_str() else {
            break;
        };
        if value == "--" || value.starts_with('-') {
            break;
        }
        consumed += 1;
    }
    consumed
}

/// Mirrors `cli::static_long_argument`: exact long name or alias.
fn static_long_argument<'a>(command: &'a Command, name: &str) -> Option<&'a Arg> {
    command.get_arguments().find(|argument| {
        argument.get_long() == Some(name)
            || argument
                .get_all_aliases()
                .is_some_and(|aliases| aliases.into_iter().any(|alias| alias == name))
    })
}

/// Mirrors `cli::static_argument_value_maximum`, including clap's `None` for a
/// derive-implicit arity that still takes one value.
fn static_argument_value_maximum(argument: &Arg) -> usize {
    argument.get_num_args().map_or_else(
        || {
            if argument.get_action().takes_values() {
                1
            } else {
                0
            }
        },
        |range| range.max_values(),
    )
}

fn static_short_argument(command: &Command, short: char) -> Option<&Arg> {
    command
        .get_arguments()
        .find(|argument| argument.get_short() == Some(short))
}

/// The final invariant: the replacement argv names no other session and ends in
/// exactly one `--resume <session_id>`.
fn argv_resumes_session(argv: &[OsString], session_id: &str) -> bool {
    if argv.len() < 3 {
        return false;
    }
    if argv[argv.len() - 2].as_os_str() != OsStr::new("--resume")
        || argv[argv.len() - 1].as_os_str() != OsStr::new(session_id)
    {
        return false;
    }
    !argv[..argv.len() - 2]
        .iter()
        .filter_map(|token| token.to_str())
        .any(session_selection_token)
}

fn session_selection_token(token: &str) -> bool {
    let name = token.strip_prefix("--").unwrap_or(token);
    let name = name.split_once('=').map_or(name, |(name, _value)| name);
    SESSION_SELECTION_FLAGS.contains(&name)
}

#[cfg(test)]
mod tests;
