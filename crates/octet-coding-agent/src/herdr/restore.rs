#![allow(missing_docs)]

//! Durable pane records and the Herdr startup restore pass.
//!
//! Herdr restores a workspace's layout after its server stops, but it can only
//! resume agents whose kind it knows how to launch. octet is not one of those
//! kinds, so this module closes the gap from octet's own side:
//!
//! 1. while octet runs in a Herdr pane it keeps a small owner-private record of
//!    that pane (`pane id`, Herdr session scope, session id, cwd, pid);
//! 2. an optional Herdr plugin (`crate::herdr::plugin`) runs
//!    `octet herdr restore` from its startup hook, which Herdr executes after it
//!    restores the session and the API socket is ready;
//! 3. that pass resumes `octet --resume <id>` in each restored pane whose record
//!    still matches the pane it was written for.
//!
//! The record is deleted only on a *deliberate* exit. A Herdr server stop sends
//! `SIGHUP` to the pane's foreground process (measured against Herdr 0.9.0), so
//! a signal-driven exit keeps the record: that is exactly the case where the
//! workspace is about to be restored and the conversation should come back.
//!
//! Every step is bounded and fail-closed: records are small, capped, pruned by
//! age, validated before use, and a pane is only resumed when its current cwd
//! matches the recorded one and it currently hosts no agent. Nothing here ever
//! runs a shell, and no transcript path or credential is stored or printed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Bumped when the record shape changes; older records are ignored.
pub(crate) const RECORD_VERSION: u32 = 2;
/// Records live beside the other octet user state.
const RECORDS_SUBDIR: &str = "herdr/panes";
/// One record per pane; a larger directory means something else wrote there.
const MAX_RECORDS: usize = 64;
/// Each record is a single small JSON object.
const MAX_RECORD_BYTES: u64 = 4 * 1024;
/// A record older than this is never used and is pruned.
const MAX_RECORD_AGE_MS: u64 = 14 * 24 * 60 * 60 * 1000;
/// A single startup pass never launches more than this many panes.
const MAX_RESTORE_PANES: usize = 16;
/// Panes listed by the server that this pass is willing to consider.
const MAX_LISTED_PANES: usize = 512;
/// Bound the captured CLI response independently of the number of panes.
const MAX_CLI_OUTPUT_BYTES: usize = 512 * 1024;
/// Bound for one `herdr` CLI call.
const CLI_TIMEOUT: Duration = Duration::from_secs(10);

/// One durable record of "octet was running in this Herdr pane".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PaneRecord {
    /// Record format version.
    pub version: u32,
    /// Herdr pane id (`w1:p1`).
    pub pane_id: String,
    /// Herdr session scope: the socket path when known, else the session name.
    pub scope: String,
    /// The octet session id `octet --resume <id>` accepts.
    pub session_id: String,
    /// The pane's working directory when octet started there.
    pub cwd: String,
    /// The session store root used by this process (`--session-dir`).
    pub session_dir: String,
    /// The workspace used to namespace the session (`--workspace`).
    pub workspace: String,
    /// The octet process that wrote the record.
    pub pid: u32,
    /// Last refresh time, in Unix milliseconds.
    pub updated_at_ms: u64,
}

/// Why one record was not resumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// Written by a different Herdr session (socket/name).
    OtherScope,
    /// The pane no longer exists in this session.
    PaneMissing,
    /// The pane currently hosts an agent (a live octet, or another agent).
    AgentPresent,
    /// The pane's current directory is not the recorded one.
    CwdMismatch,
    /// The record is older than the retention window.
    Stale,
    /// The session id is not a launchable octet session id.
    UnsafeSessionId,
    /// The octet executable path cannot be placed in a pane command string.
    UnsafeExecutable,
    /// A recorded session directory cannot be safely passed to the pane shell.
    UnsafeSessionDir,
    /// A recorded workspace cannot be safely passed to the pane shell.
    UnsafeWorkspace,
    /// This pass has already scheduled the maximum number of panes.
    LimitReached,
}

/// One planned resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Launch {
    /// Target pane.
    pub pane_id: String,
    /// The session to resume.
    pub session_id: String,
    /// The exact command string handed to `herdr pane run`.
    pub command: String,
}

/// The result of planning one restore pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RestorePlan {
    /// Panes to resume, in record order.
    pub launches: Vec<Launch>,
    /// Records intentionally not resumed, with the reason.
    pub skipped: Vec<(PaneRecord, SkipReason)>,
}

/// One pane as reported by `herdr pane list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneEntry {
    /// Pane id.
    pub pane_id: String,
    /// The agent label Herdr associates with the pane, if any.
    pub agent: Option<String>,
    /// The pane's current working directory, if Herdr reports one.
    pub cwd: Option<String>,
}

/// The session lookup scope that must accompany an opaque resume id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LaunchScope {
    pub session_dir: String,
    pub workspace: String,
}

impl LaunchScope {
    /// Resolve relative session roots against the invocation directory, not the
    /// startup hook's (possibly different) working directory.
    pub fn new(session_dir: &Path, workspace: &Path, invocation_cwd: &Path) -> Self {
        let absolute = |path: &Path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                invocation_cwd.join(path)
            }
        };
        Self {
            session_dir: canonical_text(&absolute(session_dir)),
            workspace: canonical_text(&absolute(workspace)),
        }
    }
}

/// The octet user state directory (`~/.octet`).
pub(crate) fn octet_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".octet")
}

/// Directory holding one record per pane.
pub(crate) fn records_dir() -> PathBuf {
    octet_home().join(RECORDS_SUBDIR)
}

/// The Herdr session scope used to key records: the socket path when Herdr
/// exported one, else its session name, else the default session.
pub(crate) fn session_scope() -> String {
    for name in ["HERDR_SOCKET_PATH", "HERDR_SESSION"] {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    "default".to_owned()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// A stable, filesystem-safe record file name for one pane in one session.
///
/// The pane id keeps the file readable; the scope is hashed so two Herdr
/// sessions that both use `w1:p1` cannot collide, and so no socket path (which
/// can carry a user name) is written into a file name.
fn record_file_name(scope: &str, pane_id: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in scope.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let sanitized: String = pane_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    format!("{hash:016x}-{sanitized}.json")
}

/// Record that octet is running in this pane, in `directory`. Best effort: a
/// failure here never affects the session, it only means the pane will not be
/// restored later.
pub(crate) fn write_record_in(
    directory: &Path,
    pane_id: &str,
    session_id: &str,
    cwd: &Path,
    launch_scope: &LaunchScope,
) {
    let record = PaneRecord {
        version: RECORD_VERSION,
        pane_id: pane_id.to_owned(),
        scope: session_scope(),
        session_id: session_id.to_owned(),
        cwd: canonical_text(cwd),
        session_dir: launch_scope.session_dir.clone(),
        workspace: launch_scope.workspace.clone(),
        pid: std::process::id(),
        updated_at_ms: now_ms(),
    };
    let Ok(encoded) = serde_json::to_vec(&record) else {
        return;
    };
    if encoded.len() as u64 > MAX_RECORD_BYTES {
        return;
    }
    let path = directory.join(record_file_name(&record.scope, &record.pane_id));
    // Do not admit new panes beyond the bounded restore set. Existing records
    // must still refresh even when the directory is full.
    if !path.exists() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            // A missing directory has no records; private atomic write creates it.
            if directory.exists() {
                return;
            }
            return write_private_record(&path, &encoded);
        };
        let mut count = 0;
        for (index, entry) in entries.enumerate() {
            // Refuse an unexpectedly large directory rather than scanning it
            // on the interactive path. Other file types do not consume panes.
            if index >= MAX_RECORDS * 4 {
                return;
            }
            let Ok(entry) = entry else { return };
            if entry.path().extension().and_then(|part| part.to_str()) == Some("json") {
                count += 1;
                if count >= MAX_RECORDS {
                    return;
                }
            }
        }
    }
    write_private_record(&path, &encoded);
}

fn write_private_record(path: &Path, encoded: &[u8]) {
    // The temporary file and directory are owner-only from creation, not just
    // after publication. The shared secure writer also rejects symlink swaps.
    let _ = octet_agent::secure_fs::write_private_atomic(path, encoded, MAX_RECORD_BYTES as usize);
}

/// Forget this pane. Called on a deliberate exit, so a workspace restore does
/// not resurrect a session the user closed.
pub(crate) fn remove_record_in(directory: &Path, pane_id: &str) {
    let path = directory.join(record_file_name(&session_scope(), pane_id));
    let _ = std::fs::remove_file(path);
}

/// Canonical text for a directory, falling back to the literal path.
fn canonical_text(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Read every well-formed record in the directory, bounded in count and size.
pub(crate) fn read_records(directory: &Path) -> Vec<PaneRecord> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        if records.len() >= MAX_RECORDS {
            break;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
            continue;
        }
        let Ok(bytes) =
            octet_agent::secure_fs::read_private_file_bounded(&path, MAX_RECORD_BYTES as usize)
        else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<PaneRecord>(&bytes) else {
            continue;
        };
        if record.version != RECORD_VERSION {
            continue;
        }
        records.push(record);
    }
    records
}

/// Plan one restore pass from durable records and the live pane list.
///
/// Pure and total: every record either becomes exactly one launch or one
/// reported skip, so the caller can log what happened without guessing.
pub(crate) fn plan_restore(
    records: &[PaneRecord],
    panes: &[PaneEntry],
    scope: &str,
    octet_executable: &Path,
    now: u64,
) -> RestorePlan {
    let executable = canonical_text(octet_executable);
    let quoted_executable = quote_shell_path(&executable);
    let mut plan = RestorePlan::default();
    for record in records {
        let skip = |reason: SkipReason, plan: &mut RestorePlan| {
            plan.skipped.push((record.clone(), reason));
        };
        if record.scope != scope {
            skip(SkipReason::OtherScope, &mut plan);
            continue;
        }
        if now.saturating_sub(record.updated_at_ms) > MAX_RECORD_AGE_MS {
            skip(SkipReason::Stale, &mut plan);
            continue;
        }
        let Some(pane) = panes.iter().find(|pane| pane.pane_id == record.pane_id) else {
            skip(SkipReason::PaneMissing, &mut plan);
            continue;
        };
        if pane.agent.is_some() {
            skip(SkipReason::AgentPresent, &mut plan);
            continue;
        }
        // A restored pane must still be in the directory octet ran in, or the
        // session id would resolve against the wrong workspace store.
        let pane_cwd = pane
            .cwd
            .as_deref()
            .map(|cwd| canonical_text(Path::new(cwd)))
            .unwrap_or_default();
        if pane_cwd != record.cwd {
            skip(SkipReason::CwdMismatch, &mut plan);
            continue;
        }
        if !is_safe_command_token(&record.session_id) {
            skip(SkipReason::UnsafeSessionId, &mut plan);
            continue;
        }
        let Some(executable) = quoted_executable.as_ref() else {
            skip(SkipReason::UnsafeExecutable, &mut plan);
            continue;
        };
        let Some(session_dir) = quote_shell_path(&record.session_dir) else {
            skip(SkipReason::UnsafeSessionDir, &mut plan);
            continue;
        };
        let Some(workspace) = quote_shell_path(&record.workspace) else {
            skip(SkipReason::UnsafeWorkspace, &mut plan);
            continue;
        };
        if plan.launches.len() >= MAX_RESTORE_PANES {
            skip(SkipReason::LimitReached, &mut plan);
            continue;
        }
        plan.launches.push(Launch {
            pane_id: record.pane_id.clone(),
            session_id: record.session_id.clone(),
            command: format!(
                "{executable} --resume {} --session-dir {session_dir} --workspace {workspace}",
                record.session_id
            ),
        });
    }
    plan
}

/// Whether a token can appear inside a `herdr pane run` command string.
///
/// `pane run` hands one string to the pane's shell, so anything outside this
/// allowlist (whitespace, quotes, `;`, `$`, globs) is refused rather than
/// escaped: the same rule the subagents launcher applies to Herdr commands.
fn is_safe_command_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
}

/// Quote an absolute path as one POSIX-shell token. A path may contain spaces
/// or apostrophes, but never control bytes or a second shell command.
fn quote_shell_path(value: &str) -> Option<String> {
    if !value.starts_with('/') || value.len() > 4096 || value.chars().any(char::is_control) {
        return None;
    }
    Some(format!("'{}'", value.replace('\'', "'\\''")))
}

/// Delete records that can never be used again: stale ones, and (when the pane
/// list is known) records for panes that no longer exist in this session.
fn prune_records(
    directory: &Path,
    records: &[PaneRecord],
    scope: &str,
    panes: &[PaneEntry],
    now: u64,
) {
    for record in records {
        let stale = now.saturating_sub(record.updated_at_ms) > MAX_RECORD_AGE_MS;
        let pane_gone =
            record.scope == scope && !panes.iter().any(|pane| pane.pane_id == record.pane_id);
        if stale || pane_gone {
            let _ = std::fs::remove_file(
                directory.join(record_file_name(&record.scope, &record.pane_id)),
            );
        }
    }
}

/// The `herdr` binary to call: the plugin-provided path when present.
fn herdr_binary() -> PathBuf {
    match std::env::var("HERDR_BIN_PATH") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => PathBuf::from("herdr"),
    }
}

/// Run one `herdr` CLI call with a bounded wait and bounded captured output.
/// Drain a pane list while the child runs: waiting for exit before reading a
/// full stdout pipe deadlocks even on a healthy server with many panes.
fn run_herdr(binary: &Path, args: &[String], capture_stdout: bool) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut stdout = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut too_large = false;
            loop {
                let read = pipe.read(&mut chunk).ok()?;
                if read == 0 {
                    break;
                }
                if !too_large && stdout.len().saturating_add(read) <= MAX_CLI_OUTPUT_BYTES {
                    stdout.extend_from_slice(&chunk[..read]);
                } else {
                    stdout.clear();
                    too_large = true;
                }
            }
            (!too_large).then_some(stdout)
        })
    });
    let deadline = std::time::Instant::now() + CLI_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                return match reader {
                    Some(reader) => reader.join().ok().flatten(),
                    None => Some(Vec::new()),
                };
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Parse `herdr pane list` output, bounded in pane count.
fn parse_panes(stdout: &[u8]) -> Vec<PaneEntry> {
    #[derive(Deserialize)]
    struct Envelope {
        result: Result_,
    }
    #[derive(Deserialize)]
    struct Result_ {
        panes: Vec<RawPane>,
    }
    #[derive(Deserialize)]
    struct RawPane {
        pane_id: String,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
    }

    let Ok(envelope) = serde_json::from_slice::<Envelope>(stdout) else {
        return Vec::new();
    };
    envelope
        .result
        .panes
        .into_iter()
        .take(MAX_LISTED_PANES)
        .map(|pane| PaneEntry {
            pane_id: pane.pane_id,
            agent: pane.agent.filter(|agent| !agent.is_empty()),
            cwd: pane.cwd,
        })
        .collect()
}

/// Resume every recorded octet pane in this Herdr session.
///
/// This is the plugin's startup hook. It is deliberately quiet and total: it
/// prints one line per decision, never fails the caller, and does nothing at
/// all when it is not running inside a Herdr pane.
pub(crate) fn run_restore() -> anyhow::Result<()> {
    if std::env::var("HERDR_ENV").ok().as_deref() != Some("1") {
        crate::output::stdout_line(
            "Not inside a Herdr pane (HERDR_ENV is not 1); nothing to restore.".to_owned(),
        );
        return Ok(());
    }
    let binary = herdr_binary();
    let directory = records_dir();
    let scope = session_scope();
    let records = read_records(&directory);
    let Some(stdout) = run_herdr(&binary, &["pane".to_owned(), "list".to_owned()], true) else {
        crate::output::stdout_line(
            "Could not list Herdr panes; leaving every pane record untouched.".to_owned(),
        );
        return Ok(());
    };
    let panes = parse_panes(&stdout);
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("octet"));
    let now = now_ms();
    let plan = plan_restore(&records, &panes, &scope, &executable, now);
    for (record, reason) in &plan.skipped {
        crate::output::stdout_line(format!(
            "skip {} ({})",
            record.pane_id,
            match reason {
                SkipReason::OtherScope => "another Herdr session",
                SkipReason::PaneMissing => "pane is gone",
                SkipReason::AgentPresent => "pane already runs an agent",
                SkipReason::CwdMismatch => "pane is in a different directory",
                SkipReason::Stale => "record is stale",
                SkipReason::UnsafeSessionId => "recorded session id is not launchable",
                SkipReason::UnsafeExecutable => "octet path cannot be used in a pane command",
                SkipReason::UnsafeSessionDir => "recorded session directory is not launchable",
                SkipReason::UnsafeWorkspace => "recorded workspace is not launchable",
                SkipReason::LimitReached => "restore limit reached (16 panes)",
            }
        ));
    }
    for launch in &plan.launches {
        let args = vec![
            "pane".to_owned(),
            "run".to_owned(),
            launch.pane_id.clone(),
            launch.command.clone(),
        ];
        let outcome = run_herdr(&binary, &args, false);
        crate::output::stdout_line(format!(
            "{} {} (session {})",
            if outcome.is_some() {
                "resumed"
            } else {
                "failed to resume"
            },
            launch.pane_id,
            launch.session_id
        ));
    }
    if plan.launches.is_empty() && plan.skipped.is_empty() {
        crate::output::stdout_line("No octet panes to restore.".to_owned());
    }
    prune_records(&directory, &records, &scope, &panes, now);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pane_id: &str, session_id: &str, cwd: &str, age_ms: u64) -> PaneRecord {
        PaneRecord {
            version: RECORD_VERSION,
            pane_id: pane_id.to_owned(),
            scope: "/tmp/herdr.sock".to_owned(),
            session_id: session_id.to_owned(),
            cwd: cwd.to_owned(),
            session_dir: "/tmp/octet-sessions".to_owned(),
            workspace: "/work/repo".to_owned(),
            pid: 4242,
            updated_at_ms: 4_000_000_000_000u64.saturating_sub(age_ms),
        }
    }

    fn pane(pane_id: &str, agent: Option<&str>, cwd: &str) -> PaneEntry {
        PaneEntry {
            pane_id: pane_id.to_owned(),
            agent: agent.map(str::to_owned),
            cwd: Some(cwd.to_owned()),
        }
    }

    const EXE: &str = "/usr/local/bin/octet";

    #[test]
    fn a_matching_record_plans_one_resume() {
        let records = vec![record("w1:p1", "session-abc", "/work/repo", 0)];
        let panes = vec![pane("w1:p1", None, "/work/repo")];
        let plan = plan_restore(
            &records,
            &panes,
            "/tmp/herdr.sock",
            Path::new(EXE),
            4_000_000_000_000,
        );
        assert_eq!(
            plan.launches,
            vec![Launch {
                pane_id: "w1:p1".to_owned(),
                session_id: "session-abc".to_owned(),
                command: "'/usr/local/bin/octet' --resume session-abc --session-dir '/tmp/octet-sessions' --workspace '/work/repo'".to_owned(),
            }]
        );
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn every_skip_rule_is_reported() {
        let records = vec![
            PaneRecord {
                scope: "/other/herdr.sock".to_owned(),
                ..record("w1:p1", "s1", "/work", 0)
            },
            record("w1:p2", "s2", "/work", 0),
            record("w1:p3", "s3", "/work", 0),
            record("w1:p4", "s4", "/work", 0),
            record("w1:p5", "s5", "/work", MAX_RECORD_AGE_MS + 1),
            record("w1:p6", "bad id;rm -rf", "/work", 0),
        ];
        let panes = vec![
            pane("w1:p1", None, "/work"),
            pane("w1:p2", Some("octet"), "/work"),
            pane("w1:p3", None, "/elsewhere"),
            pane("w1:p6", None, "/work"),
        ];
        let plan = plan_restore(
            &records,
            &panes,
            "/tmp/herdr.sock",
            Path::new(EXE),
            4_000_000_000_000,
        );
        assert!(plan.launches.is_empty());
        let reasons: Vec<SkipReason> = plan
            .skipped
            .iter()
            .map(|(_, reason)| reason.clone())
            .collect();
        assert_eq!(
            reasons,
            vec![
                SkipReason::OtherScope,
                SkipReason::AgentPresent,
                SkipReason::CwdMismatch,
                SkipReason::PaneMissing,
                SkipReason::Stale,
                SkipReason::UnsafeSessionId,
            ]
        );
    }

    #[test]
    fn paths_with_spaces_and_apostrophes_are_quoted_as_single_shell_tokens() {
        let mut record = record("w1:p1", "session-abc", "/work", 0);
        record.session_dir = "/tmp/my sessions".to_owned();
        record.workspace = "/work/owner's repo".to_owned();
        let plan = plan_restore(
            &[record],
            &[pane("w1:p1", None, "/work")],
            "/tmp/herdr.sock",
            Path::new("/Applications/My App/octet"),
            4_000_000_000_000,
        );
        assert_eq!(plan.launches.len(), 1);
        assert_eq!(
            plan.launches[0].command,
            "'/Applications/My App/octet' --resume session-abc --session-dir '/tmp/my sessions' --workspace '/work/owner'\\''s repo'"
        );
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn the_launch_count_is_capped() {
        let records: Vec<PaneRecord> = (0..MAX_RESTORE_PANES + 4)
            .map(|index| record(&format!("w1:p{index}"), "session", "/work", 0))
            .collect();
        let panes: Vec<PaneEntry> = (0..MAX_RESTORE_PANES + 4)
            .map(|index| pane(&format!("w1:p{index}"), None, "/work"))
            .collect();
        let plan = plan_restore(
            &records,
            &panes,
            "/tmp/herdr.sock",
            Path::new(EXE),
            4_000_000_000_000,
        );
        assert_eq!(plan.launches.len(), MAX_RESTORE_PANES);
        assert!(plan
            .skipped
            .iter()
            .all(|(_, reason)| *reason == SkipReason::LimitReached));
    }

    #[test]
    fn a_relative_session_root_uses_the_original_invocation_directory() {
        let scope = LaunchScope::new(
            Path::new("sessions"),
            Path::new("/work/project"),
            Path::new("/work"),
        );
        assert_eq!(scope.session_dir, "/work/sessions");
        assert_eq!(scope.workspace, "/work/project");
    }

    #[cfg(unix)]
    #[test]
    fn private_records_refresh_at_capacity_without_admitting_more_panes() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("panes");
        let scope = LaunchScope::new(
            Path::new("/tmp/sessions"),
            Path::new("/work"),
            Path::new("/"),
        );
        for index in 0..MAX_RECORDS {
            write_record_in(
                &directory,
                &format!("w1:p{index}"),
                "old",
                root.path(),
                &scope,
            );
        }
        assert_eq!(read_records(&directory).len(), MAX_RECORDS);
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let first = directory.join(record_file_name(&session_scope(), "w1:p0"));
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );

        write_record_in(&directory, "w1:p0", "refreshed", root.path(), &scope);
        write_record_in(&directory, "w1:p64", "overflow", root.path(), &scope);
        assert!(!directory
            .join(record_file_name(&session_scope(), "w1:p64"))
            .exists());
        assert!(read_records(&directory).iter().any(|record| {
            record.pane_id == "w1:p0"
                && record.session_id == "refreshed"
                && record.session_dir == "/tmp/sessions"
                && record.workspace == "/work"
        }));
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_list_larger_than_the_os_pipe_is_drained() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let payload = vec![b'x'; 128 * 1024];
        let data = root.path().join("list.json");
        std::fs::write(&data, &payload).unwrap();
        let script = root.path().join("herdr-stub.sh");
        std::fs::write(&script, b"#!/bin/sh\ncat \"$1\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            run_herdr(&script, &[data.to_string_lossy().into_owned()], true),
            Some(payload)
        );
    }

    #[test]
    fn record_names_are_stable_and_scope_scoped() {
        let first = record_file_name("/tmp/a/herdr.sock", "w1:p1");
        assert_eq!(first, record_file_name("/tmp/a/herdr.sock", "w1:p1"));
        assert_ne!(first, record_file_name("/tmp/b/herdr.sock", "w1:p1"));
        assert!(first.ends_with("-w1-p1.json"), "{first}");
        assert!(!first.contains('/'), "no path separators: {first}");
    }

    #[test]
    fn command_tokens_reject_shell_metacharacters() {
        assert!(is_safe_command_token("session-abc_1.2"));
        assert!(quote_shell_path("/work/owner's repo").is_some());
        assert!(quote_shell_path("/work/bad\ncommand").is_none());
        assert!(quote_shell_path("relative/path").is_none());
        for hostile in [
            "", "a b", "a;b", "a$b", "a`b`", "a|b", "a&&b", "a'b", "a\"b", "a*b",
        ] {
            assert!(!is_safe_command_token(hostile), "{hostile}");
        }
    }

    #[test]
    fn pane_list_parsing_is_bounded_and_tolerant() {
        let payload = br#"{"result":{"panes":[
            {"pane_id":"w1:p1","cwd":"/work"},
            {"pane_id":"w1:p2","agent":"octet","agent_status":"idle","cwd":"/work"},
            {"pane_id":"w1:p3","agent":"","cwd":null}
        ]}}"#;
        let panes = parse_panes(payload);
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[0].agent, None);
        assert_eq!(panes[1].agent.as_deref(), Some("octet"));
        assert_eq!(panes[2].agent, None, "an empty label is not an agent");
        assert!(parse_panes(b"not json").is_empty());
    }

    #[test]
    fn records_round_trip_and_reject_foreign_files() {
        let directory = std::env::temp_dir().join(format!(
            "octet-herdr-records-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let record = record("w1:p1", "session-abc", "/work", 0);
        let path = directory.join(record_file_name(&record.scope, &record.pane_id));
        octet_agent::secure_fs::write_private_atomic(
            &path,
            &serde_json::to_vec(&record).unwrap(),
            MAX_RECORD_BYTES as usize,
        )
        .unwrap();
        // A wrong version, a non-JSON file, and a directory are all ignored.
        let mut future = record.clone();
        future.version = RECORD_VERSION + 1;
        octet_agent::secure_fs::write_private_atomic(
            &directory.join(record_file_name(&record.scope, "w1:p2")),
            &serde_json::to_vec(&future).unwrap(),
            MAX_RECORD_BYTES as usize,
        )
        .unwrap();
        std::fs::write(directory.join("notes.txt"), b"ignore me").unwrap();
        std::fs::create_dir_all(directory.join("subdir")).unwrap();

        let read = read_records(&directory);
        assert_eq!(read, vec![record.clone()]);

        prune_records(&directory, &read, &record.scope, &[], 4_000_000_000_000);
        assert!(
            read_records(&directory).is_empty(),
            "a missing pane prunes its record"
        );
        std::fs::remove_dir_all(&directory).ok();
    }
}
