//! Tests for the reload supervisor and its component fingerprints.
//!
//! Why this is a separate module: the supervisor's contract is about which
//! components restart, stay, and get reported, and that contract is much easier
//! to audit when it is not interleaved with the watch-loop implementation.

use super::*;

fn base() -> Instant {
    Instant::now()
}

/// Settings that arm the host layer, for tests about the opt-in path.
///
/// The product default is [`ReloadSettings::default`] with the host layer
/// off; those defaults are pinned by
/// `default_settings_never_plan_the_host_layer`.
fn host_settings() -> ReloadSettings {
    ReloadSettings {
        host_enabled: true,
        ..ReloadSettings::default()
    }
}

fn file_fingerprint(len: u64) -> FileFingerprint {
    FileFingerprint {
        present: true,
        is_dir: false,
        is_symlink: false,
        len,
        modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(len)),
    }
}

fn directory_fingerprint() -> FileFingerprint {
    FileFingerprint {
        present: true,
        is_dir: true,
        is_symlink: false,
        len: 0,
        modified: Some(SystemTime::UNIX_EPOCH),
    }
}

/// A metadata source that never touches the filesystem, so every rule in
/// this module is testable in isolation.
#[derive(Default)]
struct Fake {
    entries: BTreeMap<PathBuf, FileFingerprint>,
    dirs: BTreeMap<PathBuf, Vec<PathBuf>>,
    exe: Option<PathBuf>,
}

impl Fake {
    fn new() -> Self {
        Self::default()
    }

    fn path(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    fn file(&mut self, text: &str, len: u64) -> &mut Self {
        let path = Self::path(text);
        self.entries.insert(path.clone(), file_fingerprint(len));
        self.link(path);
        self
    }

    fn dir(&mut self, text: &str) -> &mut Self {
        let path = Self::path(text);
        self.entries.insert(path.clone(), directory_fingerprint());
        self.dirs.entry(path.clone()).or_default();
        self.link(path);
        self
    }

    fn remove(&mut self, text: &str) -> &mut Self {
        let path = Self::path(text);
        self.entries.remove(&path);
        self.dirs.remove(&path);
        for children in self.dirs.values_mut() {
            children.retain(|child| child != &path);
        }
        self
    }

    fn exe(&mut self, text: &str, len: u64) -> &mut Self {
        let path = Self::path(text);
        self.entries.insert(path.clone(), file_fingerprint(len));
        self.exe = Some(path);
        self
    }

    fn link(&mut self, path: PathBuf) {
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        let children = self.dirs.entry(parent).or_default();
        if !children.contains(&path) {
            children.push(path);
        }
        children.sort();
    }
}

impl MetadataSource for Fake {
    fn symlink_metadata(&self, path: &Path) -> Option<FileFingerprint> {
        self.entries.get(path).copied()
    }

    fn target_metadata(&self, path: &Path) -> Option<FileFingerprint> {
        self.entries.get(path).copied()
    }

    fn read_dir(&self, path: &Path) -> Box<dyn Iterator<Item = std::io::Result<PathBuf>> + '_> {
        Box::new(self.dirs.get(path).into_iter().flatten().cloned().map(Ok))
    }

    fn current_exe(&self) -> Option<PathBuf> {
        self.exe.clone()
    }
}

/// Per-poll inspection budget for every test that is not about the budget.
const TEST_BUDGET: usize = DEFAULT_MAX_INSPECTIONS_PER_POLL;

/// A scanner plus the per-poll budget its test samples with.
///
/// Production passes the budget per call so that only [`ReloadSettings`]
/// owns it; this wrapper keeps the staleness tests from repeating it, and
/// pins each of them to an explicit budget at the same time.
struct TestWatcher {
    watcher: ReloadWatcher,
    budget: usize,
}

impl TestWatcher {
    fn scan(&self, source: &impl MetadataSource) -> Scan {
        self.watcher.scan(source, self.budget)
    }

    fn scan_with(&self, source: &impl MetadataSource, budget: usize) -> Scan {
        self.watcher.scan(source, budget)
    }

    /// Same shape as [`ReloadWatcher::poll`], but the per-poll inspection
    /// budget is the one this wrapper pins, so a staleness test states its
    /// budget once instead of at every call.
    fn poll(
        &self,
        supervisor: &mut ReloadSupervisor,
        source: &impl MetadataSource,
        now: Instant,
    ) -> Option<ObserveSummary> {
        if !supervisor.is_enabled() || !supervisor.should_scan(now) {
            return None;
        }
        let scan = self.scan(source);
        Some(supervisor.observe(now, &scan))
    }
}

fn budgeted_watcher(targets: &[(&str, ReloadLayer)], budget: usize) -> TestWatcher {
    let mut watches = ReloadWatchSet::new();
    for (path, layer) in targets {
        assert!(watches.watch_path(*layer, PathBuf::from(*path)));
    }
    TestWatcher {
        watcher: ReloadWatcher::new(watches),
        budget,
    }
}

fn watcher(targets: &[(&str, ReloadLayer)]) -> TestWatcher {
    budgeted_watcher(targets, TEST_BUDGET)
}

fn resources_watcher() -> TestWatcher {
    watcher(&[("/skills", ReloadLayer::Resources)])
}

/// Skipped-inspection count for one layer, read from the accounting
/// array the scanner publishes.
fn skipped_for(scan: &Scan, layer: ReloadLayer) -> usize {
    scan.meta().skipped[layer.index()]
}

/// One report line's outcome, if the pass selected that layer.
fn outcome_of(report: &ReloadReport, layer: ReloadLayer) -> Option<LayerOutcome> {
    report
        .layers
        .iter()
        .find(|line| line.layer == layer)
        .map(|line| line.outcome)
}

/// One report line's named losses, empty when the layer is absent.
fn losses_of(report: &ReloadReport, layer: ReloadLayer) -> Vec<ReloadLoss> {
    report
        .layers
        .iter()
        .find(|line| line.layer == layer)
        .map(|line| line.losses.clone())
        .unwrap_or_default()
}

/// Establish the baseline without triggering a reload.
fn baseline(supervisor: &mut ReloadSupervisor, watcher: &TestWatcher, source: &Fake, now: Instant) {
    let scan = watcher.scan(source);
    let summary = supervisor.observe(now, &scan);
    assert!(summary.first_observation);
    assert!(!supervisor.is_queued());
}

#[test]
fn the_arming_notice_names_the_cadence_and_every_bound_it_hit() {
    let mut watches = ReloadWatchSet::new();
    assert!(watches.watch_path(ReloadLayer::Resources, "/skills"));
    assert!(watches.watch_path(ReloadLayer::Resources, "/prompts"));
    for index in 0..MAX_WATCH_TARGETS {
        watches.watch_path(
            ReloadLayer::Extensions,
            PathBuf::from(format!("/extensions/{index}")),
        );
    }
    let notice = watches.arming_notice(ReloadSettings::default());
    assert!(notice.contains("live reload armed"), "{notice}");
    assert!(notice.contains("poll 1000 ms"), "{notice}");
    assert!(notice.contains("debounce 200 ms"), "{notice}");
    assert!(notice.contains("over the target cap"), "{notice}");
    assert!(notice.contains("/reload --dry-run"), "{notice}");
    assert!(
        notice.contains("host re-exec off"),
        "the notice must never imply an automatic re-exec: {notice}"
    );
    assert!(notice.len() <= MAX_NOTE_BYTES, "{}", notice.len());

    let armed = watches.arming_notice(host_settings());
    assert!(armed.contains("host re-exec armed"), "{armed}");

    let disabled = watches.arming_notice(ReloadSettings::disabled());
    assert!(disabled.contains("disabled"), "{disabled}");
}

#[test]
fn startup_notice_only_reports_incomplete_watch_coverage() {
    let mut watches = ReloadWatchSet::new();
    assert_eq!(watches.limit_notice(), None);
    for index in 0..MAX_WATCH_TARGETS {
        assert!(watches.watch_path(ReloadLayer::Resources, format!("/skills/{index}")));
    }
    assert_eq!(watches.limit_notice(), None);
    assert!(!watches.watch_path(ReloadLayer::Resources, "/overflow"));
    assert_eq!(
        watches.limit_notice().as_deref(),
        Some("live reload: watch limit reached; 1 path not watched")
    );
}

#[test]
fn default_settings_never_plan_the_host_layer() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = watcher(&[]);
    let mut source = Fake::new();
    source.exe("/opt/octet/1.2/octet", 100);
    baseline(&mut supervisor, &watcher, &source, start);

    // The binary is replaced on disk. With the default settings the host
    // layer is observed but never queued, so nothing can execve it.
    source.exe("/opt/octet/1.3/octet", 200);
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(1), &scan);
    assert!(
        summary.changed_layers.is_empty(),
        "a host-only change must not queue under the defaults: {summary:?}"
    );
    assert!(!supervisor.is_queued());
    assert!(supervisor
        .begin(start + Duration::from_secs(2), ReloadBoundary::Idle)
        .is_none());

    // `/reload --force` still takes every layer, host included, without any
    // opt-in: it is the explicit user decision.
    let plan = supervisor.force();
    assert_eq!(plan.layers(), ReloadLayer::ORDER.to_vec());
    assert!(plan.is_forced());
    assert!(supervisor.finish(plan).is_some());
}

#[test]
fn only_the_two_documented_flags_reach_the_supervisor() {
    assert_eq!(
        ReloadUserAction::parse("  /reload --dry-run "),
        Some(ReloadUserAction::DryRun)
    );
    assert_eq!(
        ReloadUserAction::parse("/reload --force"),
        Some(ReloadUserAction::Force)
    );
    // Everything else stays with `commands::parse`, including plain
    // `/reload`, a near miss, and a flag with extra words.
    for input in [
        "/reload",
        "/reload ",
        "/reload --forc",
        "/reload --force extra",
        "/reloads --force",
        "--force",
        "reload --force",
        "/reload --dry-run --force",
    ] {
        assert_eq!(ReloadUserAction::parse(input), None, "{input}");
    }
    assert_eq!(ReloadUserAction::DryRun.label(), "/reload --dry-run");
    assert_eq!(ReloadUserAction::Force.label(), "/reload --force");
}

#[test]
fn first_observation_establishes_the_baseline_and_never_reloads() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 10);
    source.exe("/opt/octet/octet", 100);

    baseline(&mut supervisor, &watcher, &source, start);
    assert!(supervisor.begin(start, ReloadBoundary::Idle).is_none());
    assert!(!supervisor.is_queued());
    assert!(supervisor.dry_run(start, ReloadBoundary::Idle).is_noop());
}

#[test]
fn unchanged_everything_is_a_no_op() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 10);

    baseline(&mut supervisor, &watcher, &source, start);
    for step in 1..5 {
        let scan = watcher.scan(&source);
        let summary = supervisor.observe(start + Duration::from_millis(step * 1000), &scan);
        assert!(!summary.first_observation);
        assert!(!summary.anything_changed());
        assert!(summary.queued_layers.is_empty());
    }
    assert!(!supervisor.is_queued());
    assert!(supervisor
        .begin(start + Duration::from_secs(30), ReloadBoundary::Idle)
        .is_none());
}

#[test]
fn a_save_burst_collapses_into_one_debounced_reload() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/seed.md", 1);
    baseline(&mut supervisor, &watcher, &source, start);

    for index in 0..5 {
        source.file(&format!("/skills/{index}.md"), 1);
    }
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_millis(10), &scan);
    assert_eq!(summary.changed_layers, vec![ReloadLayer::Resources]);
    assert_eq!(summary.changed_paths, 5);
    assert!(supervisor.is_queued());

    // Five saves, one pending pass: nothing is admitted before the debounce.
    assert!(supervisor
        .begin(start + Duration::from_millis(150), ReloadBoundary::Idle)
        .is_none());
    let plan = supervisor
        .begin(start + Duration::from_millis(210), ReloadBoundary::Idle)
        .expect("due");
    assert_eq!(plan.layers(), vec![ReloadLayer::Resources]);
    assert!(!supervisor.is_queued());
    let report = supervisor.finish(plan).expect("current");
    // The reporting list is bounded; the count is not lost.
    assert_eq!(report.layers[0].changed.len(), MAX_REPORTED_PATHS_PER_LAYER);
    assert_eq!(report.layers[0].changed_overflow, 1);
}

#[test]
fn a_burst_always_flushes_at_the_hard_ceiling() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    base_files(&mut source);
    baseline(&mut supervisor, &watcher, &source, start);

    for step in 0..8u64 {
        let last = start + Duration::from_millis(step * 300);
        source.file(&format!("/skills/{step}.md"), 1);
        let scan = watcher.scan(&source);
        supervisor.observe(last, &scan);
    }
    // The most recent change was 100 ms ago, yet the ceiling already passed.
    assert_eq!(supervisor.deadline(), Some(start + MAX_DEBOUNCE));
    assert!(supervisor.is_due(start + MAX_DEBOUNCE));
    let plan = supervisor
        .begin(start + MAX_DEBOUNCE, ReloadBoundary::Idle)
        .expect("ceiling flush");
    assert_eq!(plan.layers(), vec![ReloadLayer::Resources]);
}

fn base_files(source: &mut Fake) {
    source.file("/skills/seed.md", 1);
}

#[test]
fn only_stale_layers_are_selected_and_the_order_is_fixed() {
    let start = base();
    // Host-armed settings: this test is about layer selection order, and
    // the default-off host layer is pinned separately.
    let mut supervisor = ReloadSupervisor::new(host_settings());
    let watcher = watcher(&[
        ("/skills", ReloadLayer::Resources),
        ("/extensions", ReloadLayer::Extensions),
    ]);
    let mut source = Fake::new();
    source.dir("/skills");
    source.dir("/extensions");
    source.dir("/extensions/demo");
    source.file("/skills/a.md", 1);
    source.exe("/opt/octet/1.2/octet", 100);
    baseline(&mut supervisor, &watcher, &source, start);

    // A resource edit and an extension manifest edit in the same window.
    source.file("/skills/a.md", 2);
    source.file("/extensions/demo/extension.toml", 5);
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(1), &scan);
    assert_eq!(
        summary.changed_layers,
        vec![ReloadLayer::Resources, ReloadLayer::Extensions]
    );

    let plan = supervisor
        .begin(start + Duration::from_secs(2), ReloadBoundary::Idle)
        .expect("due");
    assert_eq!(
        plan.layers(),
        vec![ReloadLayer::Resources, ReloadLayer::Extensions]
    );
    assert!(!plan.contains(ReloadLayer::Host));
    assert!(supervisor.finish(plan).is_some());

    // A later executable change selects the host layer alone.
    source.exe("/opt/octet/1.3/octet", 100);
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(3), &scan);
    assert_eq!(summary.changed_layers, vec![ReloadLayer::Host]);
    let plan = supervisor
        .begin(start + Duration::from_secs(4), ReloadBoundary::Idle)
        .expect("due");
    assert_eq!(plan.layers(), vec![ReloadLayer::Host]);
    assert!(supervisor.finish(plan).is_some());
}

#[test]
fn a_busy_boundary_keeps_the_change_queued() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 1);
    baseline(&mut supervisor, &watcher, &source, start);

    source.file("/skills/a.md", 2);
    let scan = watcher.scan(&source);
    supervisor.observe(start + Duration::from_secs(1), &scan);

    for step in 0..10 {
        assert!(supervisor
            .begin(start + Duration::from_secs(2 + step), ReloadBoundary::Busy)
            .is_none());
    }
    assert!(supervisor.is_queued());
    assert!(supervisor.is_due(start + Duration::from_secs(2)));
    let plan = supervisor
        .begin(start + Duration::from_secs(3), ReloadBoundary::Idle)
        .expect("applied at the idle boundary");
    assert_eq!(plan.layers(), vec![ReloadLayer::Resources]);
}

#[test]
fn a_removed_path_is_a_change_when_the_layer_was_fully_inspected() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 1);
    source.file("/skills/b.md", 1);
    baseline(&mut supervisor, &watcher, &source, start);

    // A child disappears mid-poll, and the watched target itself disappears.
    source.remove("/skills/a.md");
    source.remove("/skills");
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(1), &scan);
    assert_eq!(summary.changed_layers, vec![ReloadLayer::Resources]);
    let plan = supervisor
        .begin(start + Duration::from_secs(2), ReloadBoundary::Idle)
        .expect("due");
    let report = supervisor.finish(plan).expect("current");
    let changed = &report.layers[0].changed;
    assert!(
        changed.contains(&PathBuf::from("/skills/a.md")),
        "{changed:?}"
    );
    assert!(changed.contains(&PathBuf::from("/skills")), "{changed:?}");

    // The next unchanged pass is a no-op: a vanished path is reported once
    // (its baseline entry is dropped above) instead of reloading forever.
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(3), &scan);
    assert!(!summary.anything_changed());
    assert!(summary.changed_layers.is_empty());
}

#[test]
fn the_executable_is_re_resolved_and_a_move_is_a_host_change() {
    let start = base();
    // The host layer is armed here: this pins what the opt-in observes, not
    // the product default (see `default_settings_never_plan_the_host_layer`).
    let mut supervisor = ReloadSupervisor::new(host_settings());
    let watcher = watcher(&[]);
    let mut source = Fake::new();
    source.exe("/opt/octet/1.2/octet", 100);
    baseline(&mut supervisor, &watcher, &source, start);

    // A package-manager update moves the versioned directory. Identical
    // size and type: only the resolved path changed.
    source.exe("/opt/octet/1.3/octet", 100);
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(1), &scan);
    assert_eq!(summary.changed_layers, vec![ReloadLayer::Host]);
    let plan = supervisor
        .begin(start + Duration::from_secs(2), ReloadBoundary::Idle)
        .expect("due");
    assert_eq!(plan.layers(), vec![ReloadLayer::Host]);
    let report = supervisor.finish(plan).expect("current");
    assert_eq!(
        report.layers[0].changed,
        vec![PathBuf::from("/opt/octet/1.3/octet")]
    );

    // An in-place rebuild at the same path is also a host change.
    source.exe("/opt/octet/1.3/octet", 101);
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(3), &scan);
    assert_eq!(summary.changed_layers, vec![ReloadLayer::Host]);
}

#[test]
fn force_supersedes_a_queued_pass_without_inventing_losses() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 1);
    baseline(&mut supervisor, &watcher, &source, start);
    source.file("/skills/a.md", 2);
    let scan = watcher.scan(&source);
    supervisor.observe(start + Duration::from_secs(1), &scan);
    assert!(supervisor.is_queued());

    let mut plan = supervisor.force();
    assert!(plan.is_forced());
    assert_eq!(plan.layers(), ReloadLayer::ORDER.to_vec());
    assert!(!supervisor.is_queued());
    plan.record_reload(ReloadLayer::Resources);
    let report = supervisor.finish(plan).expect("current plan");
    // Force authorizes a pass; it does not prove any work was lost.
    for layer in ReloadLayer::ORDER {
        assert!(losses_of(&report, layer).is_empty());
    }
    assert_eq!(report.layers[0].outcome, LayerOutcome::Reloaded);
    assert!(report.notices()[0].contains("reload (forced)"));
    assert!(!summary_mentions(&report, "named loss"));
}

#[test]
fn a_dry_run_reports_and_changes_nothing() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 1);
    baseline(&mut supervisor, &watcher, &source, start);

    let idle = supervisor.dry_run(start, ReloadBoundary::Idle);
    assert!(idle.dry_run);
    assert!(idle.is_noop());
    assert_eq!(
        idle.layer(ReloadLayer::Resources).map(|line| line.outcome),
        Some(LayerOutcome::Skipped(SkipReason::NoChange))
    );

    source.file("/skills/a.md", 2);
    let scan = watcher.scan(&source);
    supervisor.observe(start + Duration::from_secs(1), &scan);
    let deadline = supervisor.deadline();
    let queued = supervisor.is_queued();

    let report = supervisor.dry_run(start + Duration::from_secs(2), ReloadBoundary::Busy);
    assert_eq!(
        outcome_of(&report, ReloadLayer::Resources),
        Some(LayerOutcome::WouldReload)
    );
    assert!(!report.boundary_idle);
    assert!(report.due);
    assert!(!report.is_noop());
    assert!(report
        .notices()
        .iter()
        .any(|notice| notice.contains("would reload")));

    // Nothing moved: the same evidence is still queued, still due.
    assert_eq!(supervisor.deadline(), deadline);
    assert_eq!(supervisor.is_queued(), queued);
    assert!(!supervisor.is_in_flight());
    assert!(supervisor
        .begin(start + Duration::from_secs(2), ReloadBoundary::Idle)
        .is_some());
}

#[test]
fn the_inspection_budget_stops_every_poll_and_reports_a_skipped_lower_bound() {
    let start = base();
    // Fixture: one watched directory with ten files, so eleven candidates
    // exist (the target itself plus each enumerable entry).
    const CAP: usize = 3;
    const FILES: usize = 10;

    // The budget is an argument of the scan, not state of the watcher: the
    // supervisor's `ReloadSettings` is the one authority, which is exactly
    // what `ReloadWatcher::poll` passes here.
    let watcher = budgeted_watcher(&[("/skills", ReloadLayer::Resources)], CAP);
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let mut source = Fake::new();
    source.dir("/skills");
    for index in 0..FILES {
        source.file(&format!("/skills/{index}.md"), 1);
    }

    // Rule 1: inspection stops exactly at the bound.
    let scan = watcher.scan(&source);
    assert_eq!(scan.inspected(), CAP);
    assert!(scan.capped_for(ReloadLayer::Resources));
    // Only the overflow witness is known. Counting all omitted files would
    // require the very unbounded enumeration the budget must prevent.
    assert_eq!(skipped_for(&scan, ReloadLayer::Resources), 1);
    assert!(scan.truncated());
    // Rule 3: the budget is clamped, so no caller can switch the bound off.
    assert_eq!(watcher.scan_with(&source, 0).inspected(), 1);
    const BIG_FILES: usize = MAX_INSPECTIONS_PER_POLL + 8;
    let ceiling = budgeted_watcher(&[("/many", ReloadLayer::Resources)], usize::MAX);
    let mut many = Fake::new();
    many.dir("/many");
    for index in 0..BIG_FILES {
        many.file(&format!("/many/{index}.md"), 1);
    }
    let wide = ceiling.scan(&many);
    assert_eq!(wide.inspected(), MAX_INSPECTIONS_PER_POLL);
    assert_eq!(skipped_for(&wide, ReloadLayer::Resources), 1);
    assert!(wide.truncated());

    // The supervisor records the cap once, on the layer it truncated, and
    // reports it rather than claiming "no change".
    let summary = supervisor.observe(start, &scan);
    assert!(summary.cap_reached);
    assert_eq!(summary.inspected, CAP);
    assert!(summary.first_observation);
    let report = supervisor.dry_run(start, ReloadBoundary::Idle);
    let capped = report
        .layers
        .iter()
        .filter(|line| line.outcome == LayerOutcome::Skipped(SkipReason::WatcherCapReached))
        .count();
    assert_eq!(capped, 1, "the cap belongs to the layer it truncated");
    assert_eq!(
        report
            .layer(ReloadLayer::Resources)
            .map(|line| line.outcome),
        Some(LayerOutcome::Skipped(SkipReason::WatcherCapReached))
    );
    assert!(report.watcher_cap_reached);
    assert_eq!(report.skipped_paths, 1);
    assert!(report
        .notices()
        .iter()
        .any(|notice| notice.contains("per-poll cap")));
}

/// Generates entries lazily, without allocating the enormous fixture tree.
struct GeneratedDirectory {
    root_entries: usize,
    nested: bool,
    errors: bool,
    next_calls: std::cell::Cell<usize>,
    yielded: std::cell::Cell<usize>,
    inspections: std::cell::Cell<usize>,
    opens: std::cell::Cell<usize>,
}

impl MetadataSource for GeneratedDirectory {
    fn symlink_metadata(&self, path: &Path) -> Option<FileFingerprint> {
        self.inspections.set(self.inspections.get() + 1);
        Some(
            if path == Path::new("/skills")
                || (self.nested && path.parent() == Some(Path::new("/skills")))
            {
                directory_fingerprint()
            } else {
                file_fingerprint(1)
            },
        )
    }

    fn target_metadata(&self, _path: &Path) -> Option<FileFingerprint> {
        Some(file_fingerprint(1))
    }

    fn read_dir(&self, path: &Path) -> Box<dyn Iterator<Item = std::io::Result<PathBuf>> + '_> {
        self.opens.set(self.opens.get() + 1);
        let count = if path == Path::new("/skills") {
            self.root_entries
        } else {
            usize::MAX
        };
        let path = path.to_path_buf();
        let mut indices = 0..count;
        Box::new(std::iter::from_fn(move || {
            self.next_calls.set(self.next_calls.get() + 1);
            let index = indices.next()?;
            self.yielded.set(self.yielded.get() + 1);
            Some(if self.errors {
                Err(std::io::Error::other("unreadable entry"))
            } else {
                Ok(path.join(format!("{:020}", count - index - 1)))
            })
        }))
    }

    fn current_exe(&self) -> Option<PathBuf> {
        Some(PathBuf::from("/bin/octet"))
    }
}

#[test]
fn enumeration_work_is_bounded_before_collecting_sorting_or_filtering_errors() {
    const CAP: usize = 32;
    let watcher = budgeted_watcher(&[("/skills", ReloadLayer::Resources)], CAP);
    for root_entries in [1_000, usize::MAX] {
        for errors in [false, true] {
            let source = GeneratedDirectory {
                root_entries,
                nested: false,
                errors,
                next_calls: Default::default(),
                yielded: Default::default(),
                inspections: Default::default(),
                opens: Default::default(),
            };
            let scan = watcher.scan(&source);
            // Root inspection leaves CAP - 1 entries plus one witness.
            assert_eq!(source.yielded.get(), CAP);
            assert_eq!(source.next_calls.get(), CAP);
            assert_eq!(source.opens.get(), 1);
            assert_eq!(source.inspections.get(), if errors { 1 } else { CAP });
            assert_eq!(
                skipped_for(&scan, ReloadLayer::Resources),
                usize::from(!errors)
            );
            assert!(scan.capped_for(ReloadLayer::Resources));
            assert!(
                scan.executable().is_some(),
                "executable sampling is independent"
            );
        }
    }
}

#[test]
fn nested_enumeration_shares_the_poll_budget_and_complete_directories_are_sorted() {
    const CAP: usize = 32;
    let watcher = budgeted_watcher(&[("/skills", ReloadLayer::Resources)], CAP);
    let mut source = GeneratedDirectory {
        root_entries: CAP / 2,
        nested: true,
        errors: false,
        next_calls: Default::default(),
        yielded: Default::default(),
        inspections: Default::default(),
        opens: Default::default(),
    };
    let scan = watcher.scan(&source);
    assert_eq!(source.yielded.get(), CAP + 1);
    // The fully read parent also needs one EOF probe; directories opened
    // (and therefore EOF probes) are bounded by admitted inspections.
    assert_eq!(source.next_calls.get(), CAP + 2);
    assert_eq!(source.opens.get(), 2);
    assert!(source.inspections.get() <= CAP);
    assert!(scan.capped_for(ReloadLayer::Resources));

    source.nested = false;
    source.yielded.set(0);
    source.next_calls.set(0);
    let scan = watcher.scan(&source);
    assert!(!scan.truncated());
    assert_eq!(source.yielded.get(), CAP / 2);
    assert_eq!(source.next_calls.get(), CAP / 2 + 1);
    assert!(scan
        .entries()
        .windows(2)
        .all(|pair| pair[0].path() < pair[1].path()));
}

#[test]
fn capped_scans_neither_compare_nor_advance_a_layers_baseline() {
    let start = base();
    let mut source = Fake::new();
    source
        .dir("/skills")
        .file("/skills/a.md", 1)
        .file("/skills/b.md", 1);
    let watcher = budgeted_watcher(&[("/skills", ReloadLayer::Resources)], TEST_BUDGET);
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    supervisor.observe(start, &watcher.scan(&source));
    source
        .file("/skills/a.md", 2)
        .remove("/skills/b.md")
        .file("/skills/c.md", 1);
    let partial = watcher.scan_with(&source, 2);
    assert!(partial.truncated());
    let summary = supervisor.observe(start + Duration::from_secs(1), &partial);
    assert!(summary.changed_layers.is_empty());
    assert!(!supervisor.is_queued());
    let summary = supervisor.observe(start + Duration::from_secs(2), &watcher.scan(&source));
    assert_eq!(
        summary.changed_paths, 3,
        "change, removal, and addition survive the cap"
    );

    // A first partial observation cannot manufacture additions when the
    // layer later obtains its first complete baseline.
    let mut fresh = ReloadSupervisor::new(ReloadSettings::default());
    fresh.observe(start, &partial);
    assert!(fresh
        .observe(start + Duration::from_secs(1), &watcher.scan(&source))
        .changed_layers
        .is_empty());
    assert!(!fresh.is_queued());
}

#[test]
fn a_directory_the_budget_could_not_read_is_capped_not_removed() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let mut source = Fake::new();
    source.dir("/skills");
    source.dir("/skills/nested");
    source.file("/skills/nested/deep.md", 1);

    let targets = [("/skills", ReloadLayer::Resources)];
    // target + nested directory + the file inside it.
    const CANDIDATES: usize = 3;
    // Baseline: every level is read, so `nested/deep.md` is known.
    let full = budgeted_watcher(&targets, TEST_BUDGET);
    let scan = full.scan(&source);
    assert_eq!(scan.inspected(), CANDIDATES);
    assert_eq!(scan.meta().skipped_total(), 0);
    assert!(!scan.truncated());
    supervisor.observe(start, &scan);
    assert!(!supervisor.is_queued());

    // A budget of two reaches the nested directory but cannot read it. The
    // scanner must record that as `capped` even though there is nothing to
    // count; otherwise "deep.md was not observed" would read as "deep.md
    // was removed" and reload the world on every poll.
    const CAP: usize = 2;
    let tight = budgeted_watcher(&targets, CAP);
    let scan = tight.scan(&source);
    assert_eq!(scan.inspected(), CAP);
    assert!(scan.capped_for(ReloadLayer::Resources));
    assert_eq!(skipped_for(&scan, ReloadLayer::Resources), 0);
    assert_eq!(scan.inspected() + scan.meta().skipped_total(), CAP);
    assert!(scan.truncated());
    let summary = supervisor.observe(start + Duration::from_secs(1), &scan);
    assert!(
        summary.changed_layers.is_empty(),
        "a capped layer must not claim a change: {:?}",
        summary.changed_layers
    );
    assert!(summary.cap_reached);
    assert!(!supervisor.is_queued());
    // The depth bound is a rule, not a cap: a read scan never records it.
    assert!(!full.scan(&source).truncated());
}

#[test]
fn a_truncated_layer_never_infers_a_disappearance() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 1);
    source.file("/skills/b.md", 1);

    let targets = [("/skills", ReloadLayer::Resources)];
    let full = budgeted_watcher(&targets, TEST_BUDGET);
    let scan = full.scan(&source);
    assert!(!scan.truncated());
    supervisor.observe(start, &scan);
    assert!(!supervisor.is_queued());

    // The same tree sampled with a budget that cannot reach `b.md`:
    // "not inspected" must never be reported as "removed".
    let tight = budgeted_watcher(&targets, 2);
    let scan = tight.scan(&source);
    assert!(scan.truncated());
    assert_eq!(skipped_for(&scan, ReloadLayer::Resources), 1);
    let summary = supervisor.observe(start + Duration::from_secs(1), &scan);
    assert!(
        summary.changed_layers.is_empty(),
        "a truncated layer must not claim a change: {:?}",
        summary.changed_layers
    );
    assert!(!supervisor.is_queued());

    // A real removal inside a fully inspected layer is still a change.
    source.remove("/skills/b.md");
    let scan = full.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(2), &scan);
    assert_eq!(summary.changed_layers, vec![ReloadLayer::Resources]);
}

#[test]
fn a_cancelled_or_superseded_plan_finishes_as_none() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 1);
    baseline(&mut supervisor, &watcher, &source, start);
    source.file("/skills/a.md", 2);
    let scan = watcher.scan(&source);
    supervisor.observe(start + Duration::from_secs(1), &scan);

    let plan = supervisor
        .begin(start + Duration::from_secs(2), ReloadBoundary::Idle)
        .expect("due");
    // A forced pass supersedes the admitted one: the earlier completion is
    // discarded instead of being applied against newer state.
    let forced = supervisor.force();
    assert!(supervisor.finish(plan).is_none());
    assert!(supervisor.finish(forced).is_some());

    // Taking a forced pass also clears evidence that was queued behind it,
    // and a forced pass covers every layer whether or not it is stale.
    source.file("/skills/a.md", 3);
    let scan = watcher.scan(&source);
    supervisor.observe(start + Duration::from_secs(3), &scan);
    assert!(supervisor.is_queued());
    let forced = supervisor.force();
    assert!(!supervisor.is_queued());
    assert_eq!(forced.layers(), ReloadLayer::ORDER.to_vec());
    assert!(supervisor.finish(forced).is_some());
    assert!(!supervisor.is_in_flight());
}

#[test]
fn an_explicit_request_selects_resources_and_extensions_but_not_the_host() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = watcher(&[
        ("/skills", ReloadLayer::Resources),
        ("/extensions", ReloadLayer::Extensions),
    ]);
    let mut source = Fake::new();
    source.dir("/skills");
    source.dir("/extensions");
    source.exe("/opt/octet/octet", 10);
    baseline(&mut supervisor, &watcher, &source, start);

    supervisor.request(
        start + Duration::from_secs(1),
        ReloadRequester::ExtensionSessionReload,
    );
    assert!(supervisor.is_queued());
    // An explicit request is not debounced, but the boundary still applies.
    assert!(supervisor
        .begin(start + Duration::from_secs(1), ReloadBoundary::Busy)
        .is_none());
    let plan = supervisor
        .begin(start + Duration::from_secs(1), ReloadBoundary::Idle)
        .expect("due");
    assert_eq!(
        plan.layers(),
        vec![ReloadLayer::Resources, ReloadLayer::Extensions]
    );
    assert!(!plan.contains(ReloadLayer::Host));
    // The requester is named in the report, not just recorded internally.
    let report = supervisor.finish(plan).expect("current");
    assert!(report
        .notices()
        .iter()
        .any(|notice| notice.contains("requested by extension session/reload")));
}

#[test]
fn extension_and_host_reloads_do_not_invent_host_request_losses() {
    let start = base();

    // An explicit request (or an extension's `session/reload`) selects the
    // resources and extensions layers.
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let mut plan = {
        supervisor.request(start, ReloadRequester::ExtensionSessionReload);
        supervisor
            .begin(start, ReloadBoundary::Idle)
            .expect("explicit request is not debounced")
    };
    plan.record_reload(ReloadLayer::Extensions);
    let report = supervisor.finish(plan).expect("current");
    assert!(losses_of(&report, ReloadLayer::Extensions).is_empty());
    assert!(!report
        .diagnostics()
        .iter()
        .any(|notice| notice.contains("lost work")));

    // A host pass justified by a real executable change carries no
    // extension-restart loss: the replacement image rebuilds them.
    let mut supervisor = ReloadSupervisor::new(host_settings());
    let watcher = watcher(&[]);
    let mut source = Fake::new();
    source.exe("/opt/octet/1.0/octet", 10);
    baseline(&mut supervisor, &watcher, &source, start);
    source.exe("/opt/octet/1.1/octet", 10);
    let scan = watcher.scan(&source);
    supervisor.observe(start + Duration::from_secs(1), &scan);
    let mut plan = supervisor
        .begin(start + Duration::from_secs(2), ReloadBoundary::Idle)
        .expect("due");
    plan.record_reload(ReloadLayer::Host);
    let report = supervisor.finish(plan).expect("current");
    assert!(losses_of(&report, ReloadLayer::Host).is_empty());
    assert!(ReloadLayer::ORDER
        .iter()
        .all(|layer| losses_of(&report, *layer).is_empty()));
}

#[test]
fn notes_and_path_labels_are_bounded_and_control_free() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    // A path whose final component is far longer than any label budget.
    let long = format!("/skills/{}", "x".repeat(400));
    let scan = Scan::from_parts(
        vec![WatchedFingerprint::new(
            PathBuf::from(&long),
            ReloadLayer::Resources,
            file_fingerprint(1),
        )],
        None,
        ScanMeta {
            inspected: 1,
            skipped: [0; 3],
            capped: [false; 3],
        },
    );
    supervisor.observe(start, &scan);

    let mut plan = supervisor.force();
    assert!(plan.note(ReloadLayer::Resources, "bad\u{7}\nnote   with\tcontrols"));
    assert!(plan.detached(ReloadLayer::Resources, vec!["theme binding".to_owned()]));
    plan.record_reload(ReloadLayer::Resources);
    let report = supervisor.finish(plan).expect("current");
    let notices = report.notices();
    assert!(!notices.is_empty());
    for notice in &notices {
        assert!(!notice.chars().any(char::is_control), "{notice:?}");
        assert!(notice.len() <= MAX_NOTE_BYTES, "{}", notice.len());
    }
    assert!(notices
        .iter()
        .any(|notice| notice.contains("theme binding")));
    for label in changed_labels_of(&report.layers[0]) {
        assert!(label.len() <= MAX_LABEL_BYTES, "{label}");
    }
    assert_eq!(report.layers.len(), ReloadLayer::ORDER.len());
    assert_eq!(
        outcome_of(&report, ReloadLayer::Extensions),
        Some(LayerOutcome::Skipped(SkipReason::NoChange))
    );
}

#[test]
fn compact_reload_diagnostics_omit_routine_bookkeeping() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    supervisor.request(start, ReloadRequester::ExtensionSessionReload);
    let mut plan = supervisor.begin(start, ReloadBoundary::Idle).unwrap();
    plan.record_reload(ReloadLayer::Resources);
    plan.detached(ReloadLayer::Resources, vec!["theme binding".into()]);
    plan.reattached(ReloadLayer::Resources, vec!["theme binding".into()]);
    let report = supervisor.finish(plan).unwrap();

    assert!(report.diagnostics().is_empty());
    assert!(report.summary().contains("resources reloaded"));
    let detailed = report.notices().join("\n");
    for detail in [
        "requested by",
        "resources reloaded",
        "detached",
        "will reattach",
    ] {
        assert!(detailed.contains(detail), "{detailed}");
    }
}

#[test]
fn unchanged_host_is_reported_on_demand_but_not_in_automatic_diagnostics() {
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let mut plan = supervisor.force();
    plan.record_reload(ReloadLayer::Resources);
    plan.record(
        ReloadLayer::Host,
        LayerOutcome::Skipped(SkipReason::NoChange),
    );
    let report = supervisor.finish(plan).unwrap();
    assert!(report
        .notices()
        .iter()
        .any(|line| line == "reload: binary unchanged"));
    assert!(!report
        .diagnostics()
        .iter()
        .any(|line| line.contains("binary unchanged")));
}

#[test]
fn reload_problems_suppress_repeats_but_recur_after_checked_clear() {
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let host = ReloadComponent::Host;
    let extension = ReloadComponent::Extension("fixture".into());
    let problem = vec!["unavailable".to_owned()];
    assert_eq!(
        supervisor.checked_problems(host.clone(), problem.clone(), false),
        problem
    );
    assert!(supervisor
        .checked_problems(host.clone(), problem.clone(), false)
        .is_empty());
    // A successful, unrelated component does not clear the skipped host.
    supervisor.checked_problems(extension, Vec::new(), false);
    assert!(supervisor
        .checked_problems(host.clone(), problem.clone(), false)
        .is_empty());
    assert_eq!(
        supervisor.checked_problems(host.clone(), problem.clone(), true),
        problem
    );
    let changed = vec!["consent required".to_owned()];
    assert_eq!(
        supervisor.checked_problems(host.clone(), changed.clone(), false),
        changed
    );
    supervisor.checked_problems(host.clone(), Vec::new(), false);
    assert_eq!(
        supervisor.checked_problems(host, changed.clone(), false),
        changed
    );
}

#[test]
fn compact_reload_diagnostics_preserve_failures_limits_losses_and_caller_notes() {
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let mut plan = supervisor.force();
    plan.record(ReloadLayer::Resources, LayerOutcome::Failed);
    plan.note(
        ReloadLayer::Extensions,
        "fixture warning\u{7}: previous child retained",
    );
    let mut report = supervisor.finish(plan).unwrap();
    report.watcher_cap_reached = true;
    report.inspected_paths = 3;
    report.skipped_paths = 2;
    let notices = report.diagnostics();
    let text = notices.join("\n");
    for detail in [
        "reload (forced)",
        "resources reload failed",
        "fixture warning",
        "skipped at least 2",
    ] {
        assert!(text.contains(detail), "{text}");
    }
    assert!(!text.contains("lost work"), "{text}");
    for notice in notices {
        assert!(!notice.chars().any(char::is_control), "{notice:?}");
        assert!(notice.len() <= MAX_NOTE_BYTES);
    }
}

#[test]
fn settings_are_clamped_and_a_disabled_supervisor_stays_inert() {
    let settings = ReloadSettings {
        enabled: true,
        host_enabled: false,
        poll_interval: Duration::from_secs(9_000),
        debounce: Duration::from_secs(60),
        max_inspections_per_poll: usize::MAX,
    }
    .sanitized();
    assert_eq!(settings.poll_interval, MAX_POLL_INTERVAL);
    assert_eq!(settings.debounce, MAX_DEBOUNCE);
    assert_eq!(settings.max_inspections_per_poll, MAX_INSPECTIONS_PER_POLL);
    assert_eq!(ReloadSettings::default().tick_interval(), DEFAULT_DEBOUNCE);

    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::disabled());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 1);
    let scan = watcher.scan(&source);
    supervisor.observe(start, &scan);
    source.file("/skills/a.md", 2);
    let scan = watcher.scan(&source);
    let summary = supervisor.observe(start + Duration::from_secs(1), &scan);
    assert!(!summary.anything_changed());
    assert!(!supervisor.is_queued());
    assert!(supervisor
        .begin(start + Duration::from_secs(2), ReloadBoundary::Idle)
        .is_none());
    // A disabled supervisor is never sampled at all.
    assert!(watcher
        .poll(&mut supervisor, &source, start + Duration::from_secs(3))
        .is_none());
}

#[test]
fn the_poll_interval_gates_scanning_but_not_due_checks() {
    let start = base();
    let mut supervisor = ReloadSupervisor::new(ReloadSettings::default());
    let watcher = resources_watcher();
    let mut source = Fake::new();
    source.dir("/skills");
    source.file("/skills/a.md", 1);

    assert!(watcher.poll(&mut supervisor, &source, start).is_some());
    assert!(watcher
        .poll(&mut supervisor, &source, start + Duration::from_millis(999))
        .is_none());
    let summary = watcher
        .poll(
            &mut supervisor,
            &source,
            start + Duration::from_millis(1000),
        )
        .expect("sampled");
    assert!(!summary.anything_changed());

    source.file("/skills/a.md", 2);
    let summary = watcher
        .poll(
            &mut supervisor,
            &source,
            start + Duration::from_millis(2000),
        )
        .expect("sampled");
    assert_eq!(summary.changed_layers, vec![ReloadLayer::Resources]);
    assert_eq!(summary.queued_layers, vec![ReloadLayer::Resources]);
    // 200 ms debounce, so the first tick after the sample is not due yet.
    assert!(!supervisor.is_due(start + Duration::from_millis(2100)));
    assert!(supervisor.is_due(start + Duration::from_millis(2200)));
}

#[test]
fn watch_sets_derive_the_resolver_paths_and_reject_duplicates() {
    let scopes = ReloadScopes {
        workspace: Some(PathBuf::from("/work")),
        invocation_cwd: Some(PathBuf::from("/work/src")),
        workspace_trusted: true,
        context_files: true,
        home: Some(PathBuf::from("/home/dev")),
        global_octet_dir: Some(PathBuf::from("/home/dev/.octet")),
        extensions_root: Some(PathBuf::from("/home/dev/.octet/extensions")),
        skill_paths: vec![PathBuf::from("/extra/skills")],
        prompt_paths: vec![PathBuf::from("/extra/prompts")],
        theme_paths: vec![PathBuf::from("/extra/themes")],
        extension_paths: vec![PathBuf::from("/extra/extensions")],
    };
    let watches = ReloadWatchSet::from_scopes(&scopes);
    let resources = watches
        .targets()
        .iter()
        .filter(|target| target.layer() == ReloadLayer::Resources)
        .map(|target| target.path().to_path_buf())
        .collect::<Vec<_>>();
    for expected in [
        "/home/dev/.octet/skills",
        "/home/dev/.octet/prompts",
        "/home/dev/.octet/themes",
        "/home/dev/.octet/keybindings.json",
        "/home/dev/.octet/config.toml",
        "/home/dev/.octet/AGENTS.md",
        "/home/dev/.agents/skills",
        "/work/.octet/skills",
        "/work/.octet/prompts",
        "/work/.octet/themes",
        "/work/AGENTS.md",
        "/work/src/AGENTS.md",
        "/extra/skills",
        "/extra/prompts",
        "/extra/themes",
    ] {
        assert!(
            resources.contains(&PathBuf::from(expected)),
            "missing {expected}"
        );
    }
    let extensions = watches
        .targets()
        .iter()
        .filter(|target| target.layer() == ReloadLayer::Extensions)
        .map(|target| target.path().to_path_buf())
        .collect::<Vec<_>>();
    assert!(extensions.contains(&PathBuf::from("/home/dev/.octet/extensions")));
    assert!(extensions.contains(&PathBuf::from("/work/.octet/extensions")));
    assert!(extensions.contains(&PathBuf::from("/extra/extensions")));
    assert!(!watches
        .targets()
        .iter()
        .any(|target| target.layer() == ReloadLayer::Host));

    let mut watches = watches;
    let before = watches.targets().len();
    assert!(!watches.watch_path(ReloadLayer::Resources, "/work/.octet/skills"));
    assert_eq!(watches.targets().len(), before);
    // A duplicate is a no-op, not a dropped target: `dropped` counts only
    // targets rejected because the set was full.
    assert_eq!(watches.dropped(), 0);

    let untrusted = ReloadScopes {
        workspace_trusted: false,
        context_files: false,
        ..scopes.clone()
    };
    let watches = ReloadWatchSet::from_scopes(&untrusted);
    assert!(!watches
        .targets()
        .iter()
        .any(|target| target.path() == Path::new("/work/.octet/skills")));
    assert!(!watches
        .targets()
        .iter()
        .any(|target| target.path() == Path::new("/home/dev/.octet/AGENTS.md")));
}

#[test]
fn the_watch_target_cap_is_reported_instead_of_growing_without_bound() {
    // This is the *target count* cap ([`MAX_WATCH_TARGETS`]), which is a
    // different bound from the per-poll inspection budget: it bounds how
    // many paths a watch set can describe at all, and each rejected target
    // is counted in `dropped` so the loss is never silent.
    let mut watches = ReloadWatchSet::new();
    for index in 0..(MAX_WATCH_TARGETS + 4) {
        watches.watch_path(
            ReloadLayer::Resources,
            PathBuf::from(format!("/skills/{index}.md")),
        );
    }
    assert_eq!(watches.targets().len(), MAX_WATCH_TARGETS);
    assert_eq!(watches.dropped(), 4);
}

#[test]
fn the_real_scanner_reads_metadata_and_never_follows_a_final_symlink() {
    let directory = tempfile::tempdir().unwrap();
    let skills = directory.path().join("skills");
    std::fs::create_dir(&skills).unwrap();
    let skill = skills.join("demo.md");
    std::fs::write(&skill, "one").unwrap();

    let mut watches = ReloadWatchSet::new();
    assert!(watches.watch_path(ReloadLayer::Resources, &skills));
    assert!(watches.watch_path(ReloadLayer::Resources, skills.join("missing.md")));
    let watcher = TestWatcher {
        watcher: ReloadWatcher::new(watches),
        budget: TEST_BUDGET,
    };
    let source = SystemMetadata;

    let first = watcher.scan(&source);
    let entry = first
        .entries()
        .iter()
        .find(|entry| entry.path() == skill)
        .expect("skill observed");
    assert!(entry.fingerprint().present);
    assert!(!entry.fingerprint().is_dir);
    let missing = first
        .entries()
        .iter()
        .find(|entry| entry.path() == skills.join("missing.md"))
        .expect("missing target is still reported");
    assert!(!missing.fingerprint().present);
    assert!(!first.truncated());

    // Contents of a different length change the fingerprint without mtime
    // precision being relied on.
    std::fs::write(&skill, "a longer body").unwrap();
    let second = watcher.scan(&source);
    let entry = second
        .entries()
        .iter()
        .find(|entry| entry.path() == skill)
        .expect("skill observed");
    assert_ne!(
        first
            .entries()
            .iter()
            .find(|entry| entry.path() == skill)
            .unwrap()
            .fingerprint(),
        entry.fingerprint()
    );
    assert!(second.executable().is_some());

    // A symlink is fingerprinted as a symlink and never followed into a
    // directory, so a watched link cannot pull in an unbounded tree.
    #[cfg(unix)]
    {
        let link = skills.join("linked");
        std::os::unix::fs::symlink(&skills, &link).unwrap();
        let third = watcher.scan(&source);
        let entry = third
            .entries()
            .iter()
            .find(|entry| entry.path() == link)
            .expect("symlink observed");
        assert!(entry.fingerprint().is_symlink);
        assert_eq!(
            third
                .entries()
                .iter()
                .filter(|entry| entry.path().starts_with(&link))
                .count(),
            1
        );
    }
}

fn summary_mentions(report: &ReloadReport, needle: &str) -> bool {
    report.summary().contains(needle)
}
