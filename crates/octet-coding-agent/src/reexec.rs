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
                "1 background worker is active; stop it from the /subagents menu, or reload with the worker-detach opt-in"
                    .to_owned()
            }
            RefusalReason::BackgroundWorkers(count) => {
                format!(
                    "{count} background workers are active; stop them from the /subagents menu, or reload with the worker-detach opt-in"
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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const SESSION: &str = "session-123";

    fn buffer(payload: &ProbePayload) -> Vec<u8> {
        let mut line = serde_json::to_vec(payload).unwrap();
        line.push(b'\n');
        line
    }

    fn payload(version: &str, session_format: u32) -> ProbePayload {
        ProbePayload {
            application: APPLICATION.to_owned(),
            schema: PROBE_SCHEMA.to_owned(),
            version: version.to_owned(),
            session_format,
            extension_api: octet_agent::EXTENSION_API_VERSION.to_owned(),
        }
    }

    fn valid_output() -> ProbeOutput {
        ProbeOutput::Completed {
            stdout: buffer(&payload("0.9.0", SESSION_FORMAT_VERSION + 1)),
        }
    }

    /// A probe runner that answers with one fixed outcome.
    struct StaticProbe {
        output: ProbeOutput,
    }

    impl ProbeRunner for StaticProbe {
        fn probe(&self, _candidate: &Path, _timeout: Duration) -> ProbeOutput {
            self.output.clone()
        }
    }

    fn probe_output(output: ProbeOutput) -> Arc<dyn ProbeRunner> {
        Arc::new(StaticProbe { output })
    }

    /// A probe runner that records every candidate path it is asked to probe,
    /// so a test can prove *which* resolved path reached the probe.
    struct RecordingProbe {
        output: ProbeOutput,
        seen: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl ProbeRunner for RecordingProbe {
        fn probe(&self, candidate: &Path, _timeout: Duration) -> ProbeOutput {
            self.seen.lock().unwrap().push(candidate.to_path_buf());
            self.output.clone()
        }
    }

    fn recording_probe(output: ProbeOutput) -> (Arc<dyn ProbeRunner>, Arc<Mutex<Vec<PathBuf>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let probe = Arc::new(RecordingProbe {
            output,
            seen: Arc::clone(&seen),
        });
        (probe, seen)
    }

    /// Write one *executable* fixture image. The candidate checks require an
    /// execute bit, so a plain `std::fs::write` fixture would be classified as
    /// an update still in progress.
    fn write_binary(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Write one fixture file that deliberately has no execute bit.
    fn write_non_executable(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    }

    /// A resolver that answers with one fixed path: the candidate the test owns.
    ///
    /// Production resolves `std::env::current_exe()`; a test that relied on that
    /// would be decided by the test harness binary instead of its own fixture.
    fn resolving(path: &Path) -> ExecutableResolver {
        let path = path.to_path_buf();
        let resolve = move || -> std::io::Result<PathBuf> { Ok(path.clone()) };
        Arc::new(resolve)
    }

    /// A resolver that fails the way an update in flight does.
    fn unresolvable(kind: std::io::ErrorKind, detail: &str) -> ExecutableResolver {
        let detail = detail.to_owned();
        let resolve =
            move || -> std::io::Result<PathBuf> { Err(std::io::Error::new(kind, detail.clone())) };
        Arc::new(resolve)
    }

    #[derive(Default)]
    struct Counters {
        durable: AtomicUsize,
        shutdown: AtomicUsize,
        /// Hook order observed by the decision core.
        order: Mutex<Vec<&'static str>>,
    }

    #[derive(Default)]
    struct TestHooks {
        counters: Arc<Counters>,
        durable_failure: Option<String>,
        shutdown_failure: Option<String>,
    }

    #[async_trait::async_trait(?Send)]
    impl ReexecHooks for TestHooks {
        async fn make_session_durable(&mut self) -> anyhow::Result<()> {
            self.counters.order.lock().unwrap().push("durable-head");
            self.counters.durable.fetch_add(1, Ordering::SeqCst);
            if let Some(detail) = &self.durable_failure {
                anyhow::bail!("{detail}");
            }
            Ok(())
        }

        async fn shutdown_extensions(&mut self) -> anyhow::Result<()> {
            self.counters
                .order
                .lock()
                .unwrap()
                .push("extension-shutdown");
            self.counters.shutdown.fetch_add(1, Ordering::SeqCst);
            if let Some(detail) = &self.shutdown_failure {
                anyhow::bail!("{detail}");
            }
            Ok(())
        }
    }

    fn hook_order(hooks: &TestHooks) -> Vec<&'static str> {
        hooks.counters.order.lock().unwrap().clone()
    }

    /// A changed-image controller plus the observation that triggers it.
    ///
    /// The fixture keeps the *same* path for the startup and candidate images,
    /// which is the replace-in-place case; the moved-path cases build their own
    /// controllers. The observation is resolved through the same seam production
    /// uses ([`ReexecController::observe_generation`]) with a resolver that
    /// answers with this fixture, never with the harness binary this test runs
    /// in.
    fn changed_controller(
        directory: &Path,
        probe: Arc<dyn ProbeRunner>,
    ) -> (ReexecController, ExecutableObservation) {
        let exe = directory.join("octet");
        write_binary(&exe, b"old binary");
        let startup_generation = BinaryGeneration::capture(&exe).unwrap();
        // A different length, so the change is detected by size alone and the
        // fixture cannot depend on timestamp granularity.
        write_binary(&exe, b"new binary build two");
        let controller = ReexecController {
            startup_exe: exe.clone(),
            original_argv: vec![OsString::from("octet")],
            startup_generation,
            session_id: Some(SESSION.to_owned()),
            probe,
            probe_timeout: Duration::from_millis(250),
            resolver: resolving(&exe),
        };
        let observed = controller.observe_generation();
        assert!(
            matches!(observed, ExecutableObservation::Resolved { .. }),
            "the fixture must resolve as a runnable image: {observed:?}"
        );
        (controller, observed)
    }

    async fn ready_plan(
        directory: &Path,
        hooks: &mut TestHooks,
    ) -> (ReexecPlan, ExecutableObservation, std::path::PathBuf) {
        let (controller, observed) = changed_controller(directory, probe_output(valid_output()));
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                hooks,
            )
            .await;
        match decision {
            ReexecDecision::Ready(plan) => {
                let exe = plan.executable().to_path_buf();
                (*plan, observed, exe)
            }
            other => panic!("expected a ready plan, got {other:?}"),
        }
    }

    #[test]
    fn generation_of_an_unchanged_file_is_stable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("octet");
        std::fs::write(&path, b"binary").unwrap();
        let first = BinaryGeneration::capture(&path).unwrap();
        let second = BinaryGeneration::capture(&path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn same_size_rewrite_changes_the_generation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("octet");
        std::fs::write(&path, b"aaaa").unwrap();
        let first = BinaryGeneration::capture(&path).unwrap();
        std::fs::write(&path, b"bbbb").unwrap();
        // Force a distinct mtime so the assertion cannot depend on filesystem
        // timestamp granularity.
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
            .unwrap();
        drop(file);
        let second = BinaryGeneration::capture(&path).unwrap();
        assert_eq!(first.len, second.len, "the sizes deliberately match");
        assert_ne!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_the_image_changes_the_generation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("octet");
        std::fs::write(&path, b"binary").unwrap();
        let first = BinaryGeneration::capture(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"binary").unwrap();
        let second = BinaryGeneration::capture(&path).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn missing_executable_cannot_be_captured() {
        let directory = tempfile::tempdir().unwrap();
        assert!(BinaryGeneration::capture(&directory.path().join("absent")).is_err());
    }

    /// Sorted entry names of one directory, so a test can prove that a reload
    /// created nothing beside the executable or beside another process's state.
    fn directory_entries(directory: &Path) -> Vec<String> {
        let mut entries: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        entries
    }

    #[test]
    fn linux_deleted_image_marker_only_recovers_the_captured_path() {
        let startup = Path::new("/bin/octet");
        assert_eq!(
            linux_reload_path(startup, PathBuf::from("/bin/octet (deleted)")),
            startup
        );
        for reported in [
            "/bin/other (deleted)",
            "/bin/octet",
            "/bin/octet (deleted) extra",
        ] {
            assert_eq!(
                linux_reload_path(startup, PathBuf::from(reported)),
                Path::new(reported)
            );
        }
        // A literal filename ending in the marker remains a literal filename.
        let literal = Path::new("/bin/octet (deleted)");
        assert_eq!(linux_reload_path(literal, literal.to_path_buf()), literal);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_deleted_image_replacement_still_requires_confirmation_before_probe() {
        let directory = tempfile::tempdir().unwrap();
        let (probe, seen) = recording_probe(valid_output());
        let (mut controller, _) = changed_controller(directory.path(), probe);
        let exe = controller.executable().to_path_buf();
        let replacement = directory.path().join("replacement");
        write_binary(&replacement, b"atomically installed replacement");
        std::fs::rename(replacement, &exe).unwrap();
        let mut deleted = exe.as_os_str().to_owned();
        deleted.push(" (deleted)");
        controller.resolver = resolving(Path::new(&deleted));
        let observed = controller.observe_generation();
        assert_eq!(observed, classify_executable(Ok(exe.clone())));
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        assert!(matches!(decision,
            ReexecDecision::ConfirmationRequired {
                redirect: ExecutableRedirect::Replaced { path }, ..
            } if path == exe
        ));
        assert!(seen.lock().unwrap().is_empty(), "no probe before consent");
    }

    #[test]
    fn capture_resolves_the_process_executable() {
        // The production default is the process's own image, resolved at capture
        // time and again at every observation.
        let controller = ReexecController::capture().unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(controller.executable(), exe.as_path());
        match controller.observe_generation() {
            ExecutableObservation::Resolved { path, generation } => {
                assert_eq!(path, exe);
                assert_eq!(generation, BinaryGeneration::capture(&exe).unwrap());
            }
            other => panic!("the running test binary must resolve as a runnable image: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn update_in_progress_observations_block_with_distinct_notices_and_no_probe() {
        let directory = tempfile::tempdir().unwrap();
        let (probe, seen) = recording_probe(valid_output());
        let (mut controller, _resolved_fixture) = changed_controller(directory.path(), probe);
        let missing = directory.path().join("octet-gone");
        let plain = directory.path().join("octet-not-executable");
        write_non_executable(&plain, b"half-written binary");
        // A regular file standing where a directory must be: a resolved path
        // that cannot be inspected at all, distinct from a missing one and not
        // dependent on running as a non-root user.
        let blocking = directory.path().join("blocking-file");
        std::fs::write(&blocking, b"not a directory").unwrap();
        let uninspectable = blocking.join("octet");

        // Each in-flight update shape is injected through the resolver, so the
        // shapes are covered by the same seam production uses: the path no
        // longer resolves, the file is missing, the file cannot be inspected,
        // and the file is not executable yet.
        let mut cases: Vec<(ExecutableObservation, String)> = Vec::new();
        controller.resolver = unresolvable(
            std::io::ErrorKind::NotFound,
            "the update removed the running image",
        );
        let unresolved = controller.observe_generation();
        assert!(
            matches!(unresolved, ExecutableObservation::PathUnresolved { .. }),
            "{unresolved:?}"
        );
        cases.push((
            unresolved,
            "reload blocked · the running executable path no longer resolves: the update removed the running image".to_owned(),
        ));

        controller.resolver = resolving(&missing);
        let absent = controller.observe_generation();
        assert!(
            matches!(absent, ExecutableObservation::Unreadable { .. }),
            "{absent:?}"
        );
        cases.push((
            absent,
            format!(
                "reload blocked · the updated executable at {} could not be read from disk: ",
                missing.display()
            ),
        ));

        controller.resolver = resolving(&uninspectable);
        let unreadable = controller.observe_generation();
        assert!(
            matches!(unreadable, ExecutableObservation::Unreadable { .. }),
            "{unreadable:?}"
        );
        cases.push((
            unreadable,
            format!(
                "reload blocked · the updated executable at {} could not be read from disk: ",
                uninspectable.display()
            ),
        ));

        controller.resolver = resolving(&plain);
        let not_executable = controller.observe_generation();
        assert!(
            matches!(not_executable, ExecutableObservation::NotExecutable { .. }),
            "{not_executable:?}"
        );
        cases.push((
            not_executable,
            format!(
                "reload blocked · the updated executable at {} is not executable yet",
                plain.display()
            ),
        ));

        let mut notices = Vec::new();
        let mut hook_logs = Vec::new();
        for (observed, expected_prefix) in &cases {
            let mut hooks = TestHooks::default();
            let decision = controller
                .reexec_if_changed(
                    observed,
                    &LiveSafetyInputs::default(),
                    ReexecOptions::default(),
                    &mut hooks,
                )
                .await;
            let notice = match &decision {
                ReexecDecision::Blocked { notice, .. } => notice.clone(),
                other => panic!("an update in progress must block, got {other:?}"),
            };
            assert!(
                notice.starts_with(expected_prefix.as_str()),
                "{observed:?}: {notice}"
            );
            notices.push(notice);
            hook_logs.push(hook_order(&hooks));
        }
        assert_eq!(notices.len(), 4);
        let mut distinct = notices.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            notices.len(),
            "each shape has its own notice"
        );
        assert!(
            seen.lock().unwrap().is_empty(),
            "an in-flight update must not probe anything"
        );
        for order in &hook_logs {
            assert!(order.is_empty(), "nothing may be torn down: {order:?}");
        }
    }

    #[test]
    fn probe_payload_round_trips_and_identifies_octet() {
        let line = probe_payload();
        assert!(line.ends_with('\n'));
        let decoded = ProbePayload::decode(line.as_bytes()).unwrap();
        assert_eq!(decoded, ProbePayload::current());
        assert_eq!(decoded.application, "octet");
        assert_eq!(decoded.schema, PROBE_SCHEMA);
        assert_eq!(decoded.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(decoded.session_format, SESSION_FORMAT_VERSION);
        assert_eq!(decoded.extension_api, octet_agent::EXTENSION_API_VERSION);
        decoded.check_compatible().unwrap();
    }

    #[test]
    fn probe_request_is_recognized_only_before_the_terminator() {
        let requested = vec![OsString::from("octet"), OsString::from(PROBE_FLAG)];
        assert!(probe_requested(&requested));
        let ordinary = vec![OsString::from("octet"), OsString::from("--model")];
        assert!(!probe_requested(&ordinary));
        let positional = vec![
            OsString::from("octet"),
            OsString::from("--"),
            OsString::from(PROBE_FLAG),
        ];
        assert!(!probe_requested(&positional));
    }

    #[test]
    fn malformed_and_incompatible_payloads_are_rejected() {
        assert!(ProbePayload::decode(b"").is_err());
        assert!(ProbePayload::decode(b"not json").is_err());
        assert!(ProbePayload::decode(b"{\"application\":\"octet\"}").is_err());
        assert!(ProbePayload::decode(b"{} trailing").is_err());

        let mut wrong_application = payload("0.9.0", SESSION_FORMAT_VERSION);
        wrong_application.application = "other".to_owned();
        assert!(wrong_application.check_compatible().is_err());

        let mut wrong_schema = payload("0.9.0", SESSION_FORMAT_VERSION);
        wrong_schema.schema = "octet-reexec-probe-v0".to_owned();
        assert!(wrong_schema.check_compatible().is_err());

        let mut no_version = payload("", SESSION_FORMAT_VERSION);
        no_version.version.clear();
        assert!(no_version.check_compatible().is_err());

        let mut old_format = payload("0.9.0", 0);
        old_format.session_format = 0;
        assert!(old_format.check_compatible().is_err());

        let newer_format = payload("0.9.0", SESSION_FORMAT_VERSION + 1);
        newer_format.check_compatible().unwrap();
    }

    #[tokio::test]
    async fn unchanged_binary_is_resources_only() {
        let directory = tempfile::tempdir().unwrap();
        let exe = directory.path().join("octet");
        write_binary(&exe, b"binary");
        let controller = ReexecController {
            startup_exe: exe.clone(),
            original_argv: vec![OsString::from("octet")],
            startup_generation: BinaryGeneration::capture(&exe).unwrap(),
            session_id: Some(SESSION.to_owned()),
            probe: probe_output(valid_output()),
            probe_timeout: Duration::from_millis(50),
            resolver: resolving(&exe),
        };
        // Resolution goes through the injected resolver, so this decision is
        // about the fixture and not about the harness binary running the test.
        let observed = controller.observe_generation();
        assert_eq!(observed, classify_executable(Ok(exe.clone())));
        assert!(matches!(observed, ExecutableObservation::Resolved { .. }));
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        assert!(matches!(decision, ReexecDecision::ResourcesOnly));
        assert_eq!(decision.notice(), Some(notice::RESOURCES_ONLY));
        assert_eq!(
            notice::RESOURCES_ONLY,
            "resources reloaded · binary unchanged"
        );
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 0);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn changed_binary_reaches_ready_and_names_the_candidate_build_and_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut hooks = TestHooks::default();
        let (plan, _observed, exe) = ready_plan(directory.path(), &mut hooks).await;
        assert_eq!(plan.session_id(), SESSION);
        assert_eq!(plan.executable(), exe.as_path());
        assert_eq!(plan.startup_executable(), exe.as_path());
        assert_eq!(
            plan.notice(),
            format!("reloading into build 0.9.0 at {}", exe.display())
        );
        assert_eq!(plan.detached_workers(), 0);
        assert_eq!(plan.detach_notice(), None);
        assert_eq!(
            hook_order(&hooks),
            vec!["durable-head", "extension-shutdown"]
        );
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 1);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 1);
        let argv = plan.argv();
        assert_eq!(argv[argv.len() - 2], OsString::from("--resume"));
        assert_eq!(argv[argv.len() - 1], OsString::from(SESSION));
    }

    #[tokio::test]
    async fn a_moved_executable_carries_the_new_path_into_the_probe_exec_and_notice() {
        let directory = tempfile::tempdir().unwrap();
        // The process started from `octet-1`; the update installed `octet-2` at
        // a new path, exactly like a retargeted symlink or a swapped
        // version-pinned directory.
        let startup = directory.path().join("octet-1");
        write_binary(&startup, b"old binary");
        let startup_generation = BinaryGeneration::capture(&startup).unwrap();
        let moved = directory.path().join("octet-2");
        write_binary(&moved, b"new binary build two");
        let (probe, seen) = recording_probe(valid_output());
        let controller = ReexecController {
            startup_exe: startup.clone(),
            original_argv: vec![OsString::from("octet")],
            startup_generation,
            session_id: Some(SESSION.to_owned()),
            probe,
            probe_timeout: Duration::from_millis(250),
            resolver: resolving(&moved),
        };
        // Reload-time resolution returns the new path, not the startup capture.
        let observed = controller.observe_generation();
        assert_eq!(
            observed,
            classify_executable(Ok(moved.clone())),
            "resolution must name the installed image, not the startup path"
        );
        let mut hooks = TestHooks::default();
        // A moved image is a retarget: without the caller's confirmation the
        // decision must ask and must not execute (probe) the candidate.
        let unconfirmed = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        match unconfirmed {
            ReexecDecision::ConfirmationRequired {
                redirect:
                    ExecutableRedirect::Moved {
                        from: seen_from,
                        to: seen_to,
                    },
                notice,
            } => {
                assert_eq!(seen_from, startup);
                assert_eq!(seen_to, moved);
                assert!(notice.starts_with(notice::CONFIRMATION_PREFIX), "{notice}");
            }
            other => panic!("a moved image must ask before it is probed, got {other:?}"),
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "the candidate must not be spawned before the confirmation"
        );
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 0);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);

        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default().confirming_redirect(),
                &mut hooks,
            )
            .await;
        let plan = match decision {
            ReexecDecision::Ready(plan) => plan,
            other => panic!("a moved executable must reload, got {other:?}"),
        };
        // The new path reached the probe ...
        assert_eq!(seen.lock().unwrap().clone(), vec![moved.clone()]);
        // ... the exec ...
        assert_eq!(plan.executable(), moved.as_path());
        // ... and the notice, which names both the image being entered and the
        // build the process started from.
        assert_eq!(
            plan.notice(),
            format!(
                "reloading into build 0.9.0 at {} (started from {})",
                moved.display(),
                startup.display()
            )
        );
        assert_eq!(plan.startup_executable(), startup.as_path());
        let exec_seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&exec_seen);
        let decision = plan.exec_with(move |exe, _argv| {
            recorded.lock().unwrap().push(exe.to_path_buf());
            std::io::Error::other("the harness replaces the image, not this test")
        });
        assert!(matches!(decision, ReexecDecision::ExecFailed { .. }));
        assert_eq!(exec_seen.lock().unwrap().clone(), vec![moved.clone()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_retargeted_symlink_changes_the_generation_at_the_same_path() {
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("octet");
        let old = directory.path().join("octet-1.2.3");
        write_binary(&old, b"old binary");
        std::os::unix::fs::symlink(&old, &link).unwrap();
        let startup_generation = BinaryGeneration::capture(&link).unwrap();
        let (probe, seen) = recording_probe(valid_output());
        let controller = ReexecController {
            startup_exe: link.clone(),
            original_argv: vec![OsString::from("octet")],
            startup_generation,
            session_id: Some(SESSION.to_owned()),
            probe,
            probe_timeout: Duration::from_millis(250),
            resolver: resolving(&link),
        };
        // The launch path is the symlink, so the resolved path does not move;
        // only the image behind it does. The generation must still change.
        let new = directory.path().join("octet-1.2.4");
        write_binary(&new, b"new binary build two");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&new, &link).unwrap();
        let observed = controller.observe_generation();
        assert_eq!(
            observed,
            classify_executable(Ok(link.clone())),
            "the link path still resolves, but to a different image"
        );
        let mut hooks = TestHooks::default();
        // A same-path inode swap (the symlink now points at a different file)
        // is a retarget: it needs the explicit confirmation before the probe.
        let unconfirmed = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        match unconfirmed {
            ReexecDecision::ConfirmationRequired {
                redirect: ExecutableRedirect::Replaced { path },
                notice,
            } => {
                assert_eq!(path, link);
                assert!(notice.starts_with(notice::CONFIRMATION_PREFIX), "{notice}");
            }
            other => panic!("a retargeted symlink must ask before it is probed, got {other:?}"),
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "the candidate must not be spawned before the confirmation"
        );
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 0);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);

        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default().confirming_redirect(),
                &mut hooks,
            )
            .await;
        let plan = match decision {
            ReexecDecision::Ready(plan) => plan,
            other => panic!("a retargeted symlink must reload, got {other:?}"),
        };
        assert_eq!(seen.lock().unwrap().clone(), vec![link.clone()]);
        assert_eq!(plan.executable(), link.as_path());
        assert_eq!(plan.startup_executable(), link.as_path());
    }

    #[tokio::test]
    async fn probe_exit_failure_blocks_without_teardown() {
        let directory = tempfile::tempdir().unwrap();
        let probe = probe_output(ProbeOutput::FailedExit {
            status: "exit status: 3".to_owned(),
        });
        let (controller, observed) = changed_controller(directory.path(), probe);
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        match decision {
            ReexecDecision::Blocked {
                reason: BlockedReason::ProbeExited { status },
                notice,
            } => {
                assert_eq!(status, "exit status: 3");
                assert_eq!(
                    notice,
                    "reload blocked · the candidate binary did not answer the probe (exit status: 3)"
                );
            }
            other => panic!("expected a blocked probe, got {other:?}"),
        }
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 0);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn probe_malformed_output_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let probe = probe_output(ProbeOutput::Completed {
            stdout: b"not a probe".to_vec(),
        });
        let (controller, observed) = changed_controller(directory.path(), probe);
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        assert!(matches!(
            decision,
            ReexecDecision::Blocked {
                reason: BlockedReason::ProbeMalformed(_),
                ..
            }
        ));
        assert!(decision
            .notice()
            .unwrap()
            .starts_with("reload blocked · the candidate binary returned an unreadable probe: "));
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn probe_incompatible_payload_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let mut incompatible = payload("0.9.0", SESSION_FORMAT_VERSION + 1);
        incompatible.session_format = 0;
        let probe = probe_output(ProbeOutput::Completed {
            stdout: buffer(&incompatible),
        });
        let (controller, observed) = changed_controller(directory.path(), probe);
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        match decision {
            ReexecDecision::Blocked {
                reason: BlockedReason::ProbeIncompatible(detail),
                notice,
            } => {
                assert_eq!(
                    detail,
                    "it reads session format 0 but this session is format 1"
                );
                assert_eq!(
                    notice,
                    "reload blocked · the candidate binary cannot serve this session: it reads session format 0 but this session is format 1"
                );
            }
            other => panic!("expected an incompatible candidate, got {other:?}"),
        }
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 0);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn probe_timeout_and_spawn_failure_block() {
        let directory = tempfile::tempdir().unwrap();
        let (controller, observed) =
            changed_controller(directory.path(), probe_output(ProbeOutput::TimedOut));
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        match decision {
            ReexecDecision::Blocked {
                reason: BlockedReason::ProbeTimedOut { timeout },
                notice,
            } => {
                assert_eq!(timeout, Duration::from_millis(250));
                assert_eq!(
                    notice,
                    "reload blocked · the candidate binary did not answer the probe within 250ms"
                );
            }
            other => panic!("expected a timeout, got {other:?}"),
        }

        let directory = tempfile::tempdir().unwrap();
        let probe = probe_output(ProbeOutput::SpawnFailed {
            detail: "permission denied".to_owned(),
        });
        let (controller, observed) = changed_controller(directory.path(), probe);
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        match decision {
            ReexecDecision::Blocked {
                reason: BlockedReason::ProbeSpawnFailed(detail),
                notice,
            } => {
                assert_eq!(detail, "permission denied");
                assert_eq!(
                    notice,
                    "reload blocked · the candidate binary could not be started: permission denied"
                );
            }
            other => panic!("expected a spawn failure, got {other:?}"),
        }
    }

    /// A probe that rewrites the candidate while answering: the pinned
    /// generation must be re-checked before anything is torn down.
    struct MutatingProbe {
        path: PathBuf,
        stdout: Vec<u8>,
    }

    impl ProbeRunner for MutatingProbe {
        fn probe(&self, _candidate: &Path, _timeout: Duration) -> ProbeOutput {
            std::fs::write(&self.path, b"third binary").unwrap();
            ProbeOutput::Completed {
                stdout: self.stdout.clone(),
            }
        }
    }

    #[tokio::test]
    async fn candidate_changing_during_the_probe_blocks_before_teardown() {
        let directory = tempfile::tempdir().unwrap();
        let (mut controller, observed) =
            changed_controller(directory.path(), probe_output(valid_output()));
        let stdout = buffer(&payload("0.9.0", SESSION_FORMAT_VERSION + 1));
        let path = controller.executable().to_path_buf();
        controller.probe = Arc::new(MutatingProbe { path, stdout });
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        assert!(matches!(
            decision,
            ReexecDecision::Blocked {
                reason: BlockedReason::CandidateChanged,
                ..
            }
        ));
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 0);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn candidate_changing_after_preparation_blocks_before_exec() {
        let directory = tempfile::tempdir().unwrap();
        let mut hooks = TestHooks::default();
        let (plan, _observed, exe) = ready_plan(directory.path(), &mut hooks).await;
        std::fs::write(&exe, b"replaced after the probe").unwrap();
        let exec_calls = AtomicUsize::new(0);
        let decision = plan.exec_with(|_, _| {
            exec_calls.fetch_add(1, Ordering::SeqCst);
            std::io::Error::other("must not run")
        });
        assert!(matches!(
            decision,
            ReexecDecision::Blocked {
                reason: BlockedReason::CandidateChanged,
                ..
            }
        ));
        assert_eq!(exec_calls.load(Ordering::SeqCst), 0);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn every_safety_input_refuses_with_its_own_notice() {
        let cases: &[(LiveSafetyInputs, RefusalReason, &str)] = &[
            (
                LiveSafetyInputs {
                    model_turn_active: true,
                    ..LiveSafetyInputs::default()
                },
                RefusalReason::ModelTurnActive,
                "reload refused · a model turn is still streaming; wait for it to finish",
            ),
            (
                LiveSafetyInputs {
                    tool_call_active: true,
                    ..LiveSafetyInputs::default()
                },
                RefusalReason::ToolCallActive,
                "reload refused · a tool call is still running",
            ),
            (
                LiveSafetyInputs {
                    shell_child_active: true,
                    ..LiveSafetyInputs::default()
                },
                RefusalReason::ShellChildActive,
                "reload refused · a shell command is still running",
            ),
            (
                LiveSafetyInputs {
                    pending_effect: true,
                    ..LiveSafetyInputs::default()
                },
                RefusalReason::PendingApprovalOrEffect,
                "reload refused · an effect is awaiting approval",
            ),
            (
                LiveSafetyInputs {
                    session_persistence_in_flight: true,
                    ..LiveSafetyInputs::default()
                },
                RefusalReason::SessionPersistenceInFlight,
                "reload refused · session persistence is still in flight",
            ),
            (
                LiveSafetyInputs {
                    background_workers: 1,
                    ..LiveSafetyInputs::default()
                },
                RefusalReason::BackgroundWorkers(1),
                "reload refused · 1 background worker is active; stop it from the /subagents menu, or reload with the worker-detach opt-in",
            ),
            (
                LiveSafetyInputs {
                    background_workers: 3,
                    ..LiveSafetyInputs::default()
                },
                RefusalReason::BackgroundWorkers(3),
                "reload refused · 3 background workers are active; stop them from the /subagents menu, or reload with the worker-detach opt-in",
            ),
        ];
        for (inputs, reason, expected) in cases {
            let directory = tempfile::tempdir().unwrap();
            let (controller, observed) =
                changed_controller(directory.path(), probe_output(valid_output()));
            let mut hooks = TestHooks::default();
            let decision = controller
                .reexec_if_changed(&observed, inputs, ReexecOptions::default(), &mut hooks)
                .await;
            assert_eq!(decision.notice(), Some(*expected));
            assert!(matches!(
                decision,
                ReexecDecision::Refused {
                    reason: actual,
                    ..
                } if &actual == reason
            ));
            assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 0);
            assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn refusal_order_is_fixed() {
        let inputs = LiveSafetyInputs {
            model_turn_active: true,
            tool_call_active: true,
            shell_child_active: true,
            pending_effect: true,
            session_persistence_in_flight: true,
            background_workers: 4,
        };
        assert_eq!(inputs.refusal(), Some(RefusalReason::ModelTurnActive));
    }

    #[test]
    fn only_the_worker_input_is_softened_by_the_detach_opt_in() {
        let workers = LiveSafetyInputs {
            background_workers: 2,
            ..LiveSafetyInputs::default()
        };
        assert_eq!(workers.refusal(), Some(RefusalReason::BackgroundWorkers(2)));
        assert_eq!(
            workers.refusal_with(ReexecOptions::refusing_workers()),
            Some(RefusalReason::BackgroundWorkers(2))
        );
        assert_eq!(
            workers.refusal_with(ReexecOptions::detaching_workers()),
            None
        );

        // Every run-owned input outranks the workers and stays a hard refusal
        // under the opt-in, in the same fixed order.
        let busy = LiveSafetyInputs {
            model_turn_active: true,
            tool_call_active: true,
            shell_child_active: true,
            pending_effect: true,
            session_persistence_in_flight: true,
            background_workers: 4,
        };
        assert_eq!(
            busy.refusal_with(ReexecOptions::detaching_workers()),
            Some(RefusalReason::ModelTurnActive)
        );
        let with_tool = LiveSafetyInputs {
            tool_call_active: true,
            background_workers: 4,
            ..LiveSafetyInputs::default()
        };
        assert_eq!(
            with_tool.refusal_with(ReexecOptions::detaching_workers()),
            Some(RefusalReason::ToolCallActive)
        );
    }

    #[tokio::test]
    async fn worker_detach_opt_in_warns_names_the_count_and_keeps_the_durable_head_first() {
        let directory = tempfile::tempdir().unwrap();
        let (controller, observed) =
            changed_controller(directory.path(), probe_output(valid_output()));
        let safety = LiveSafetyInputs {
            background_workers: 3,
            ..LiveSafetyInputs::default()
        };

        // The default policy is unchanged: live workers refuse, and nothing is
        // torn down, probed, or detached.
        let mut refused_hooks = TestHooks::default();
        let refused = controller
            .reexec_if_changed(
                &observed,
                &safety,
                ReexecOptions::default(),
                &mut refused_hooks,
            )
            .await;
        assert_eq!(
            refused.notice(),
            Some(
                "reload refused · 3 background workers are active; stop them from the /subagents menu, or reload with the worker-detach opt-in"
            )
        );
        assert!(hook_order(&refused_hooks).is_empty());
        assert_eq!(refused_hooks.counters.shutdown.load(Ordering::SeqCst), 0);

        // The explicit opt-in detaches them, and the ordering is the detach
        // boundary: the durable head runs before the extension shutdown that
        // detaches the workers.
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &safety,
                ReexecOptions::detaching_workers(),
                &mut hooks,
            )
            .await;
        let plan = match decision {
            ReexecDecision::Ready(plan) => plan,
            other => panic!("the opt-in must reach a plan, got {other:?}"),
        };
        assert_eq!(plan.detached_workers(), 3);
        assert_eq!(
            plan.detach_notice(),
            Some(
                "reload detaching workers · 3 background workers are being detached and will be reattachable in the new image"
                    .to_owned()
            )
        );
        assert_eq!(
            hook_order(&hooks),
            vec!["durable-head", "extension-shutdown"],
            "the durable head must run before the detach"
        );
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 1);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 1);

        // A durable-head failure while detaching is still a hard block, and the
        // detach never runs.
        let mut failing_hooks = TestHooks {
            durable_failure: Some("worker roster could not be flushed".to_owned()),
            ..TestHooks::default()
        };
        let decision = controller
            .reexec_if_changed(
                &observed,
                &safety,
                ReexecOptions::detaching_workers(),
                &mut failing_hooks,
            )
            .await;
        assert_eq!(
            decision.notice(),
            Some(
                "reload blocked · the active session could not be made durable at its exact head: worker roster could not be flushed"
            )
        );
        assert_eq!(
            hook_order(&failing_hooks),
            vec!["durable-head"],
            "the detach must not run after a failed durable head"
        );
    }

    #[tokio::test]
    async fn no_session_identity_refuses_without_probing() {
        let directory = tempfile::tempdir().unwrap();
        let (mut controller, observed) =
            changed_controller(directory.path(), probe_output(valid_output()));
        controller.session_id = None;
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        assert!(matches!(
            decision,
            ReexecDecision::Refused {
                reason: RefusalReason::NoSessionIdentity,
                ..
            }
        ));
        assert_eq!(
            decision.notice(),
            Some("reload refused · this run has no durable session identity to resume in the new process")
        );

        controller.session_id = Some("-leading-dash".to_owned());
        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        assert!(matches!(
            decision,
            ReexecDecision::Refused {
                reason: RefusalReason::NoSessionIdentity,
                ..
            }
        ));
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn durable_head_failure_blocks_before_extension_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let (controller, observed) =
            changed_controller(directory.path(), probe_output(valid_output()));
        let mut hooks = TestHooks {
            durable_failure: Some("append failed".to_owned()),
            ..TestHooks::default()
        };
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        match decision {
            ReexecDecision::Blocked {
                reason: BlockedReason::DurableHeadFailed(detail),
                notice,
            } => {
                assert_eq!(detail, "append failed");
                assert_eq!(
                    notice,
                    "reload blocked · the active session could not be made durable at its exact head: append failed"
                );
            }
            other => panic!("expected a durable-head failure, got {other:?}"),
        }
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 1);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn extension_shutdown_failure_blocks_without_exec() {
        let directory = tempfile::tempdir().unwrap();
        let (controller, observed) =
            changed_controller(directory.path(), probe_output(valid_output()));
        let mut hooks = TestHooks {
            shutdown_failure: Some("children survived".to_owned()),
            ..TestHooks::default()
        };
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        match decision {
            ReexecDecision::Blocked {
                reason: BlockedReason::ExtensionShutdownFailed(detail),
                notice,
            } => {
                assert_eq!(detail, "children survived");
                assert_eq!(
                    notice,
                    "reload blocked · extension processes could not be stopped: children survived"
                );
            }
            other => panic!("expected a shutdown failure, got {other:?}"),
        }
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 1);
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failing_exec_yields_exec_failed_and_calls_shutdown_once() {
        let directory = tempfile::tempdir().unwrap();
        let mut hooks = TestHooks::default();
        let (plan, _observed, _exe) = ready_plan(directory.path(), &mut hooks).await;
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 1);
        let decision = plan.exec_with(|exe, argv| {
            assert!(exe.ends_with("octet"));
            assert_eq!(argv[argv.len() - 2], OsString::from("--resume"));
            std::io::Error::other("execve denied")
        });
        match decision {
            ReexecDecision::ExecFailed { notice, source } => {
                assert_eq!(source.kind(), std::io::ErrorKind::Other);
                assert_eq!(
                    notice,
                    "reload failed · the new binary could not take over: execve denied"
                );
            }
            other => panic!("expected an exec failure decision, got {other:?}"),
        }
        assert_eq!(hooks.counters.shutdown.load(Ordering::SeqCst), 1);
        assert_eq!(hooks.counters.durable.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failing_descriptor_sweep_blocks_before_exec() {
        let directory = tempfile::tempdir().unwrap();
        let mut hooks = TestHooks::default();
        let (plan, _observed, _exe) = ready_plan(directory.path(), &mut hooks).await;
        let exec_calls = AtomicUsize::new(0);
        let decision = plan.exec_with_seal(
            |_, _| {
                exec_calls.fetch_add(1, Ordering::SeqCst);
                std::io::Error::other("must not run")
            },
            || Err(std::io::Error::other("descriptor sweep unavailable")),
        );
        match decision {
            ReexecDecision::Blocked { reason, notice } => {
                assert_eq!(
                    reason,
                    BlockedReason::DescriptorHygieneFailed(
                        "descriptor sweep unavailable".to_owned()
                    )
                );
                assert_eq!(
                    notice,
                    "reload blocked · octet-owned descriptors could not be made close-on-exec: descriptor sweep unavailable"
                );
            }
            other => panic!("expected a blocked reload, got {other:?}"),
        }
        assert_eq!(exec_calls.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reload_takes_no_lock_on_the_executable_and_writes_nothing() {
        use fs2::FileExt;

        let directory = tempfile::tempdir().unwrap();
        let (controller, observed) =
            changed_controller(directory.path(), probe_output(valid_output()));
        let exe = controller.executable().to_path_buf();
        let before = BinaryGeneration::capture(&exe).unwrap();
        let entries_before = directory_entries(directory.path());
        // Another pane, or a wrapper script, already holds an exclusive lock on
        // the image. A reload must neither need it nor disturb it: a lock on the
        // executable is exactly what would serialize (and deadlock) N panes plus
        // a running `octet serve`, so the path never takes one.
        let foreign = std::fs::File::open(&exe).unwrap();
        foreign.lock_exclusive().unwrap();

        let mut hooks = TestHooks::default();
        let decision = tokio::time::timeout(
            Duration::from_secs(5),
            controller.reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            ),
        )
        .await
        .expect("a reload must not wait on a lock held by another process");
        let plan = match decision {
            ReexecDecision::Ready(plan) => plan,
            other => panic!("expected a ready plan, got {other:?}"),
        };
        let exec_calls = AtomicUsize::new(0);
        let decision = plan.exec_with(|_, _| {
            exec_calls.fetch_add(1, Ordering::SeqCst);
            std::io::Error::other("the harness replaces the image, not this test")
        });
        assert!(matches!(decision, ReexecDecision::ExecFailed { .. }));
        assert_eq!(exec_calls.load(Ordering::SeqCst), 1);

        // The foreign lock survived: this process still holds it, so nobody
        // took it, replaced it, or waited on it.
        let probe = std::fs::File::open(&exe).unwrap();
        assert!(
            fs2::FileExt::try_lock_exclusive(&probe).is_err(),
            "another process's lock on the image must survive a reload"
        );
        // The image is unchanged by identity, and nothing new appeared beside
        // it: no lock file, no marker, no global state.
        assert_eq!(BinaryGeneration::capture(&exe).unwrap(), before);
        assert_eq!(directory_entries(directory.path()), entries_before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_reload_in_one_pane_leaves_another_pane_and_a_server_untouched() {
        use fs2::FileExt;

        let directory = tempfile::tempdir().unwrap();
        let (controller, observed) =
            changed_controller(directory.path(), probe_output(valid_output()));
        // A second pane's transcript, held open by a live writer (the same `fs2`
        // boundary `octet_agent::Session` holds), and the lock a running
        // `octet serve` keeps for the store it owns.
        let other_pane = directory.path().join("other-pane.jsonl");
        std::fs::write(&other_pane, b"{\"kind\":\"user\",\"id\":1}\n").unwrap();
        let pane_holder = std::fs::File::options()
            .read(true)
            .write(true)
            .open(&other_pane)
            .unwrap();
        pane_holder.lock_exclusive().unwrap();
        let serve_lock = directory.path().join("serve.lock");
        let serve_holder = std::fs::File::create(&serve_lock).unwrap();
        serve_holder.lock_exclusive().unwrap();
        let pane_bytes = std::fs::read(&other_pane).unwrap();
        let pane_identity = BinaryGeneration::capture(&other_pane).unwrap();
        let entries_before = directory_entries(directory.path());

        let mut hooks = TestHooks::default();
        let decision = controller
            .reexec_if_changed(
                &observed,
                &LiveSafetyInputs::default(),
                ReexecOptions::default(),
                &mut hooks,
            )
            .await;
        let plan = match decision {
            ReexecDecision::Ready(plan) => plan,
            other => panic!("expected a ready plan, got {other:?}"),
        };
        let _ = plan.exec_with(|_, _| std::io::Error::other("stop before the image is replaced"));

        // Same bytes, same identity, same directory, locks still held: a reload
        // in this pane is invisible to the other pane and to the server.
        assert_eq!(std::fs::read(&other_pane).unwrap(), pane_bytes);
        assert_eq!(
            BinaryGeneration::capture(&other_pane).unwrap(),
            pane_identity
        );
        assert_eq!(directory_entries(directory.path()), entries_before);
        assert!(
            fs2::FileExt::try_lock_exclusive(&std::fs::File::open(&other_pane).unwrap()).is_err(),
            "the other pane still owns its transcript lock"
        );
        assert!(
            fs2::FileExt::try_lock_exclusive(&std::fs::File::open(&serve_lock).unwrap()).is_err(),
            "the serving lock is untouched"
        );
    }

    #[test]
    fn plain_invocation_gains_exactly_one_resume() {
        let argv =
            canonical_resume_argv(&strings(&["octet"]), Path::new("/bin/octet"), SESSION).unwrap();
        assert_eq!(argv, strings(&["octet", "--resume", "session-123"]));
    }

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn rebuilt(original: &[&str]) -> Vec<OsString> {
        let argv =
            canonical_resume_argv(&strings(original), Path::new("/bin/octet"), SESSION).unwrap();
        assert_eq!(argv[argv.len() - 2], OsString::from("--resume"));
        assert_eq!(argv[argv.len() - 1], OsString::from(SESSION));
        assert!(argv_resumes_session(&argv, SESSION));
        argv
    }

    #[test]
    fn session_selection_shapes_are_all_removed() {
        let shapes: &[&[&str]] = &[
            &["octet", "--continue"],
            &["octet", "--resume"],
            &["octet", "--resume", "old-id"],
            &["octet", "--resume=old-id"],
            &["octet", "--resume="],
            &["octet", "--fork"],
            &["octet", "--fork", "old-id"],
            &["octet", "--fork=old-id"],
            &["octet", "--session-id", "old-id"],
            &["octet", "--session-id=old-id"],
            &["octet", "--no-session", "--print", "hi"],
            &["octet", "--continue", "--model", "sonnet"],
            &["octet", "--resume", "old-id", "--model", "sonnet"],
            &["octet", "--reload"],
        ];
        for shape in shapes {
            let argv = rebuilt(shape);
            // The last two tokens are the canonical resume pair the builder
            // appends; everything before them must be free of session selectors.
            let prefix = &argv[..argv.len() - 2];
            let text = prefix
                .iter()
                .map(|token| token.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            for selector in SESSION_SELECTION_FLAGS {
                assert!(
                    !text.iter().any(|token| {
                        token == &format!("--{selector}")
                            || token.starts_with(&format!("--{selector}="))
                    }),
                    "{shape:?} leaked --{selector}: {text:?}"
                );
            }
            assert!(
                !text.iter().any(|token| token == "--reload"),
                "{shape:?} leaked --reload: {text:?}"
            );
        }
    }

    #[test]
    fn bare_resume_does_not_swallow_a_following_option() {
        let argv = rebuilt(&["octet", "--resume", "--model", "sonnet"]);
        assert!(argv.contains(&OsString::from("--model")));
        assert!(argv.contains(&OsString::from("sonnet")));
    }

    #[test]
    fn ordinary_options_and_short_names_survive() {
        let argv = rebuilt(&[
            "octet",
            "--model",
            "sonnet",
            "--color",
            "always",
            "--name",
            "work",
            "-n",
            "other",
            "prompt text",
        ]);
        assert_eq!(argv[0], OsString::from("octet"));
        assert!(argv.contains(&OsString::from("--model")));
        assert!(argv.contains(&OsString::from("sonnet")));
        assert!(argv.contains(&OsString::from("--name")));
        assert!(argv.contains(&OsString::from("work")));
        assert!(argv.contains(&OsString::from("-n")));
        assert!(argv.contains(&OsString::from("other")));
        // Positional prompts are dropped: the resumed session already has them.
        assert!(!argv.contains(&OsString::from("prompt text")));
    }

    #[test]
    fn terminator_and_positional_prompts_never_survive() {
        let argv = rebuilt(&["octet", "--", "--continue", "prompt text"]);
        assert_eq!(argv, strings(&["octet", "--resume", SESSION]));
        let argv = rebuilt(&["octet", "prompt text"]);
        assert_eq!(argv, strings(&["octet", "--resume", SESSION]));
    }

    #[test]
    fn extension_declared_flags_keep_their_value() {
        let argv = rebuilt(&["octet", "--web-search", "auto", "--model", "sonnet"]);
        assert!(argv.contains(&OsString::from("--web-search")));
        assert!(argv.contains(&OsString::from("auto")));
        assert!(argv.contains(&OsString::from("--model")));
        assert!(argv.contains(&OsString::from("sonnet")));
    }

    #[test]
    fn rebuilt_argv_parses_as_an_exact_resume() {
        use clap::Parser;

        let shapes: &[&[&str]] = &[
            &["octet"],
            &["octet", "--continue"],
            &["octet", "--resume"],
            &["octet", "--resume", "old-id"],
            &["octet", "--resume=old-id"],
            &["octet", "--fork"],
            &["octet", "--fork", "old-id"],
            &["octet", "fix the bug"],
            &[
                "octet",
                "--model",
                "sonnet",
                "--color",
                "always",
                "fix the bug",
            ],
            &["octet", "--session-id", "old-id", "--name", "work"],
            &["octet", "--no-session", "--print", "hi"],
            &["octet", "--reload"],
            &["octet", "--", "--continue"],
            &["octet", "-n", "work"],
        ];
        for shape in shapes {
            let argv = rebuilt(shape);
            let parsed = crate::cli::Cli::try_parse_from(&argv)
                .unwrap_or_else(|error| panic!("{shape:?} rebuilt to {argv:?}: {error}"));
            assert_eq!(parsed.resume, Some(Some(SESSION.to_owned())), "{shape:?}");
            assert!(!parsed.continue_, "{shape:?}");
            assert!(parsed.fork.is_none(), "{shape:?}");
            assert!(parsed.parity.session_id.is_none(), "{shape:?}");
            assert!(!parsed.parity.no_session, "{shape:?}");
            assert!(parsed.message.is_none(), "{shape:?}");
        }
    }

    #[test]
    fn every_session_selection_flag_is_a_real_cli_option() {
        let command = crate::cli::Cli::command();
        for name in SESSION_SELECTION_FLAGS {
            assert!(
                static_long_argument(&command, name).is_some(),
                "--{name} is not a current CLI option; update SESSION_SELECTION_FLAGS"
            );
        }
    }

    #[test]
    fn argv_invariant_rejects_a_second_session_selector() {
        let argv = strings(&["octet", "--continue", "--resume", "session-123"]);
        assert!(!argv_resumes_session(&argv, "session-123"));
        let argv = strings(&["octet", "--resume", "other-id", "--resume", "session-123"]);
        assert!(!argv_resumes_session(&argv, "session-123"));
        let argv = strings(&["octet", "--resume", "session-123"]);
        assert!(argv_resumes_session(&argv, "session-123"));
    }

    #[test]
    fn notices_are_pinned_in_one_place() {
        assert_eq!(
            notice::RESOURCES_ONLY,
            "resources reloaded · binary unchanged"
        );
        assert_eq!(
            notice::reloading("0.9.0", Path::new("/opt/homebrew/bin/octet")),
            "reloading into build 0.9.0 at /opt/homebrew/bin/octet"
        );
        assert_eq!(
            notice::detaching_workers(1),
            "reload detaching workers · 1 background worker is being detached and will be reattachable in the new image"
        );
        assert_eq!(
            notice::detaching_workers(3),
            "reload detaching workers · 3 background workers are being detached and will be reattachable in the new image"
        );
        assert_eq!(
            notice::blocked(&BlockedReason::CandidateChanged),
            "reload blocked · the binary on disk changed while it was being verified; run /reload again"
        );
        assert_eq!(
            notice::blocked(&BlockedReason::ExecutablePathUnresolved("gone".to_owned())),
            "reload blocked · the running executable path no longer resolves: gone"
        );
        assert_eq!(
            notice::blocked(&BlockedReason::ExecutableUnreadable {
                path: PathBuf::from("/opt/homebrew/bin/octet"),
                detail: "no such file".to_owned(),
            }),
            "reload blocked · the updated executable at /opt/homebrew/bin/octet could not be read from disk: no such file"
        );
        assert_eq!(
            notice::blocked(&BlockedReason::ExecutableNotExecutable {
                path: PathBuf::from("/opt/homebrew/bin/octet"),
            }),
            "reload blocked · the updated executable at /opt/homebrew/bin/octet is not executable yet"
        );
        assert_eq!(
            notice::blocked(&BlockedReason::DescriptorHygieneFailed("leak".to_owned())),
            "reload blocked · octet-owned descriptors could not be made close-on-exec: leak"
        );
        assert_eq!(
            notice::refused(RefusalReason::ToolCallActive),
            "reload refused · a tool call is still running"
        );
        assert_eq!(
            notice::refused(RefusalReason::SessionPersistenceInFlight),
            "reload refused · session persistence is still in flight"
        );
        assert_eq!(
            notice::refused(RefusalReason::BackgroundWorkers(2)),
            "reload refused · 2 background workers are active; stop them from the /subagents menu, or reload with the worker-detach opt-in"
        );
        assert_eq!(
            notice::exec_failed(&std::io::Error::other("boom")),
            "reload failed · the new binary could not take over: boom"
        );
    }

    #[cfg(unix)]
    fn clear_close_on_exec(descriptor: std::os::fd::BorrowedFd<'_>) {
        use std::os::fd::AsRawFd;
        // SAFETY: test-only; F_SETFD on one borrowed descriptor, no pointers.
        unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, 0) };
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_hygiene_verifies_and_sets_close_on_exec() {
        use std::os::fd::AsFd;
        let file = tempfile::tempfile().unwrap();
        clear_close_on_exec(file.as_fd());
        // Another test in this binary may sweep the process's descriptors
        // concurrently (that is what `ReexecPlan::exec` does), so the first call
        // may either set the flag or find it already set. What must hold is that
        // the descriptor is close-on-exec afterwards.
        let _ = ensure_close_on_exec(file.as_fd()).unwrap();
        assert!(ensure_close_on_exec(file.as_fd()).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_sweep_seals_an_open_session_descriptor() {
        use std::os::fd::{AsFd, AsRawFd};
        // This is the session-append-line / session-lock case: an open
        // octet-owned descriptor that is not close-on-exec when the image is
        // replaced would be inherited by the image that keeps this PID.
        let file = tempfile::tempfile().unwrap();
        clear_close_on_exec(file.as_fd());
        seal_process_descriptors().unwrap();
        assert_eq!(
            descriptor_close_on_exec_state(file.as_raw_fd()).unwrap(),
            Some(true),
            "the sweep must leave every open descriptor above stdio close-on-exec"
        );
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_sweep_seals_a_descriptor_above_the_old_numeric_cap() {
        use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

        // The previous sweep stopped at the fixed 4096 cap, so this fixture
        // must own a descriptor above it. Raising the soft limit is the only
        // way to allocate one; a host that cannot raise it cannot host the
        // fixture, and says so instead of claiming the cap is unreachable.
        const ABOVE_CAP: libc::c_int = 4097;
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit/setrlimit read and write exactly one `rlimit`.
        let raised = unsafe {
            libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 && {
                limit.rlim_cur = limit.rlim_max.min(20_000).max(ABOVE_CAP as u64);
                libc::setrlimit(libc::RLIMIT_NOFILE, &limit) == 0
            }
        };
        if !raised {
            eprintln!("skipping: cannot raise RLIMIT_NOFILE to own a descriptor above {ABOVE_CAP}");
            return;
        }

        let file = tempfile::tempfile().unwrap();
        // SAFETY: F_DUPFD duplicates this one open descriptor onto a number at
        // or above `ABOVE_CAP` and clears FD_CLOEXEC, which is exactly the leak
        // the sweep exists to repair.
        let high = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, ABOVE_CAP) };
        assert!(
            high >= ABOVE_CAP,
            "the fixture must own a descriptor above the old 4096 cap: {high}"
        );
        // SAFETY: `high` is a fresh descriptor this test now owns.
        let owned = unsafe { OwnedFd::from_raw_fd(high) };
        clear_close_on_exec(owned.as_fd());
        assert_eq!(
            descriptor_close_on_exec_state(high).unwrap(),
            Some(false),
            "the fixture must start without close-on-exec"
        );

        seal_process_descriptors().unwrap();
        assert_eq!(
            descriptor_close_on_exec_state(high).unwrap(),
            Some(true),
            "the sweep must seal a descriptor above the old numeric cap"
        );
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_sweep_leaves_stdio_for_the_replacement_image() {
        let state = || -> Vec<Option<bool>> {
            (0..FIRST_SEALED_DESCRIPTOR)
                .map(|descriptor| descriptor_close_on_exec_state(descriptor).unwrap())
                .collect()
        };
        let before = state();
        seal_process_descriptors().unwrap();
        assert_eq!(before, state(), "stdio is inherited on purpose");
    }
}
