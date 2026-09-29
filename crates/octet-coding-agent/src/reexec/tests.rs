//! Tests for the re-exec bootstrap: probe executables, re-exec eligibility,
//! and the self-copy that makes a re-exec possible.
//!
//! Moved out of reexec.rs so the re-exec sequence stays readable on its own.
//! The suite is mostly about filesystem state that must be shaped before a
//! re-exec is attempted, which is a concern of its own.

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
#[cfg(unix)]
fn write_non_executable(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

/// Sorted entry names of one directory, so a test can prove that a reload
/// created nothing beside the executable or beside another process's state.
#[cfg(unix)]
fn directory_entries(directory: &Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    entries
}

/// A resolver that fails the way an update in flight does.
#[cfg(unix)]
fn unresolvable(kind: std::io::ErrorKind, detail: &str) -> ExecutableResolver {
    let detail = detail.to_owned();
    let resolve =
        move || -> std::io::Result<PathBuf> { Err(std::io::Error::new(kind, detail.clone())) };
    Arc::new(resolve)
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
            "reload refused · 1 background worker is active; stop it from /extensions, or reload with the worker-detach opt-in",
        ),
        (
            LiveSafetyInputs {
                background_workers: 3,
                ..LiveSafetyInputs::default()
            },
            RefusalReason::BackgroundWorkers(3),
            "reload refused · 3 background workers are active; stop them from /extensions, or reload with the worker-detach opt-in",
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
    let (controller, observed) = changed_controller(directory.path(), probe_output(valid_output()));
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
            "reload refused · 3 background workers are active; stop them from /extensions, or reload with the worker-detach opt-in"
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
    let (controller, observed) = changed_controller(directory.path(), probe_output(valid_output()));
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
    let (controller, observed) = changed_controller(directory.path(), probe_output(valid_output()));
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
                BlockedReason::DescriptorHygieneFailed("descriptor sweep unavailable".to_owned())
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
    let (controller, observed) = changed_controller(directory.path(), probe_output(valid_output()));
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
    let (controller, observed) = changed_controller(directory.path(), probe_output(valid_output()));
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
    let argv = canonical_resume_argv(&strings(original), Path::new("/bin/octet"), SESSION).unwrap();
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
        "reload refused · 2 background workers are active; stop them from /extensions, or reload with the worker-detach opt-in"
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
