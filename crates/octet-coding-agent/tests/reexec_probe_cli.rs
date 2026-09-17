#![cfg(unix)]

//! `--internal-reexec-probe` is the contract `/reload` validates a candidate
//! binary with before it replaces the running image. These tests run the real
//! built binary and keep every side effect inside a scratch `HOME`, workspace,
//! and session directory.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Mirrors `PROBE_FLAG` in `src/reexec.rs`.
const PROBE_FLAG: &str = "--internal-reexec-probe";
/// Mirrors `PROBE_SCHEMA` in `src/reexec.rs`. `ProbePayload::check_compatible`
/// refuses any other value, so a schema rename must reach this test.
const PROBE_SCHEMA: &str = "octet-reexec-probe-v1";
/// The probe is a local, side-effect-free early return; five seconds is already
/// far above a healthy answer and keeps a hung binary from stalling the suite.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// The negative case parses a positional prompt and fails on the missing
/// provider; it must still terminate on its own.
const TERMINATOR_TIMEOUT: Duration = Duration::from_secs(20);

/// One disposable `HOME`/workspace/session root.
struct Scratch {
    _root: TempDir,
    home: PathBuf,
    workspace: PathBuf,
    sessions: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("probe scratch root");
        let canonical = root.path().canonicalize().expect("canonical scratch root");
        let home = canonical.join("home");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        fs::create_dir_all(home.join(".octet")).expect("scratch home");
        fs::create_dir_all(&workspace).expect("scratch workspace");
        fs::create_dir_all(&sessions).expect("scratch session directory");
        // A sentinel makes a recursive before/after comparison meaningful: the
        // probe must not create, remove, or rewrite anything under `HOME`.
        fs::write(home.join(".octet/keep"), b"sentinel\n").expect("sentinel");
        Self {
            _root: root,
            home,
            workspace,
            sessions,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(built_binary());
        command
            .current_dir(&self.workspace)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &self.workspace)
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8");
        command
    }

    /// Every entry under the scratch root, as sorted relative paths.
    fn tree(&self) -> BTreeSet<String> {
        fn walk(root: &Path, directory: &Path, entries: &mut BTreeSet<String>) {
            for entry in fs::read_dir(directory).expect("read scratch directory") {
                let entry = entry.expect("scratch entry");
                let path = entry.path();
                entries.insert(
                    path.strip_prefix(root)
                        .expect("entry below the scratch root")
                        .to_string_lossy()
                        .into_owned(),
                );
                if entry.file_type().expect("entry type").is_dir() {
                    walk(root, &path, entries);
                }
            }
        }
        let root = self
            .home
            .parent()
            .expect("scratch home has a parent")
            .to_path_buf();
        let mut entries = BTreeSet::new();
        walk(&root, &root, &mut entries);
        entries
    }
}

/// The built `octet` binary this test must exercise.
fn built_binary() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_octet"));
    assert!(
        path.is_file(),
        "the built octet binary is missing at {}; build the package before running this test",
        path.display()
    );
    path
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

/// Run one process with a hard wall-clock bound, draining both pipes.
fn run_bounded(mut command: Command, timeout: Duration) -> BoundedOutput {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the built octet binary");
    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    let stdout_reader = thread::spawn(move || read_all(stdout));
    let stderr_reader = thread::spawn(move || read_all(stderr));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll octet") {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().expect("reap octet");
        }
        thread::sleep(Duration::from_millis(5));
    };
    BoundedOutput {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
        timed_out,
    }
}

fn read_all(mut reader: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = reader.read_to_end(&mut bytes);
    bytes
}

fn visible(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(512)])
        .escape_default()
        .to_string()
}

#[test]
fn internal_reexec_probe_answers_one_payload_and_leaves_no_trace() {
    let scratch = Scratch::new();
    let before = scratch.tree();
    let mut command = scratch.command();
    command
        .arg(PROBE_FLAG)
        .arg("--workspace")
        .arg(&scratch.workspace)
        .arg("--session-dir")
        .arg(&scratch.sessions)
        .args(["--model", "custom/probe", "--theme", "dark"]);
    let started = Instant::now();
    let output = run_bounded(command, PROBE_TIMEOUT);
    let elapsed = started.elapsed();

    assert!(
        !output.timed_out,
        "the probe did not exit within {PROBE_TIMEOUT:?}"
    );
    assert!(
        output.status.success(),
        "the probe exited with {}; stderr: {}",
        output.status,
        visible(&output.stderr)
    );
    assert!(
        elapsed < PROBE_TIMEOUT,
        "the probe took {elapsed:?}; it must be a cheap early return"
    );
    assert!(
        output.stderr.is_empty(),
        "the probe wrote to stderr: {}",
        visible(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("probe stdout is UTF-8");
    assert!(
        stdout.ends_with('\n'),
        "probe stdout is not newline-terminated: {stdout:?}"
    );
    assert_eq!(
        stdout.lines().count(),
        1,
        "the probe must print exactly one JSON line: {stdout:?}"
    );
    let payload: serde_json::Value =
        serde_json::from_str(stdout.trim_end()).expect("probe stdout is one JSON payload");
    assert_eq!(payload["application"], "octet");
    assert_eq!(payload["schema"], PROBE_SCHEMA);
    assert_eq!(payload["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        payload["session_format"]
            .as_u64()
            .is_some_and(|format| format >= 1),
        "the probe must report a session format: {payload}"
    );
    assert!(
        payload["extension_api"]
            .as_str()
            .is_some_and(|api| !api.is_empty()),
        "the probe must report an extension API version: {payload}"
    );

    assert_eq!(
        scratch.tree(),
        before,
        "the probe wrote to its scratch HOME/workspace/session root"
    );
}

#[test]
fn internal_reexec_probe_after_a_terminator_is_never_recognized() {
    let scratch = Scratch::new();
    let mut command = scratch.command();
    command
        .args(["--offline", "--no-context-files", "--no-tools"])
        .arg("--workspace")
        .arg(&scratch.workspace)
        .arg("--session-dir")
        .arg(&scratch.sessions)
        .arg("--")
        .arg(PROBE_FLAG);
    let output = run_bounded(command, TERMINATOR_TIMEOUT);

    assert!(
        !output.timed_out,
        "a probe flag after `--` must not stall the frontend"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(PROBE_SCHEMA) && !stderr.contains(PROBE_SCHEMA),
        "a probe flag after `--` was answered as a probe; stdout: {stdout:?} stderr: {stderr:?}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout.trim()).is_err(),
        "a probe flag after `--` must be a positional prompt, not a payload: {stdout:?}"
    );
}
