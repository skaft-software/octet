#![allow(missing_docs)]

//! Live-reload supervisor: bounded metadata polling plus an idle-boundary state
//! machine that decides *which* layer reloads and *when*.
//!
//! This module owns no watcher thread, no `notify` adapter, and no filesystem
//! callback. A caller (the interactive frontend) drives it from its idle tick:
//!
//! ```text
//! watcher.poll(&mut supervisor, &SystemMetadata, now)   // sample, debounce
//! if supervisor.is_due(now) { return Idle::ReloadDue }  // boundary is the caller's
//! ... at the idle boundary ...
//! let mut plan = supervisor.begin(now, ReloadBoundary::Idle)?;
//! if plan.contains(ReloadLayer::Resources) { /* caller reloads resources */ }
//! if plan.contains(ReloadLayer::Extensions) { /* caller restarts children */ }
//! plan.record(ReloadLayer::Resources, LayerOutcome::Reloaded);
//! let report = supervisor.finish(plan)?;                // one pass, one report
//! for notice in report.diagnostics() { /* transcript */ }
//! // Show report.summary() for explicit commands, not automatic passes.
//! ```
//!
//! The decision functions are pure: [`ReloadSupervisor::observe`] consumes a
//! [`Scan`] (data), never the filesystem, so every rule below is unit-testable.
//! The only I/O lives in [`MetadataSource`] and its [`SystemMetadata`]
//! implementation: `symlink_metadata`, `read_dir`, and a *fresh*
//! `std::env::current_exe()` on every poll.
//!
//! # Layers (Pi parity)
//!
//! The layer taxonomy matches Pi's reload surface, not a new one:
//!
//! | layer | contents | Pi receipt |
//! |---|---|---|
//! | [`ReloadLayer::Resources`] | skills, prompts, themes, context files (`AGENTS.md`), settings, keybindings | `packages/coding-agent/docs/extensions.md:1320`; Pi's `/reload` notice lists extensions, skills, prompts, themes, context files, settings, keybindings |
//! | [`ReloadLayer::Extensions`] | executable extension children | same notice; Pi emits `session_shutdown` then `session_start`/`resources_discover` with reason `reload` |
//! | [`ReloadLayer::Host`] | the octet process image (`current_exe()`) | ours only: Pi reloads in process, octet additionally re-execs (`crates/octet-coding-agent/src/reexec.rs`) |
//!
//! Pi watches exactly one thing automatically (git `HEAD` in its footer at a
//! 1000 ms interval, and custom theme files); everything else reloads on
//! demand. This module deliberately goes further: it *samples* all three
//! layers on the same bounded schedule, but it applies nothing outside the
//! same boundary discipline `/reload` already uses (queued to idle, debounced,
//! fixed order `resources → extensions → host`). A watched theme file is one
//! resource path, so theme auto-reload falls out of the resources layer
//! without wiring a second watcher.
//!
//! The **host** layer is the one layer that is off by default: resources and
//! extensions auto-reload from [`ReloadSettings::default`], but an executable
//! change is only planned when the user opted in with
//! [`ReloadSettings::host_enabled`] (`reload_host = true`), and `/reload
//! --force` can always take a host pass now. A changed binary is otherwise
//! observed into the baseline and reported as nothing, because executing a
//! replaced image is a decision one generic `reload = true` must never make.
//!
//! # What a reload loses (honest limits)
//!
//! Nothing here is lossless. At an idle boundary:
//!
//! * A **model call in flight** dies when the run is stopped for a reload; the
//!   records already appended to the session survive, the unrecorded turn does
//!   not ([`ReloadLoss::ModelCall`]).
//! * A **tool call in flight** is abandoned mid-effect
//!   ([`ReloadLoss::ToolCall`]); a half-applied edit or a killed shell
//!   child is possible.
//! * An extension restart drops that extension's **in-flight host request**
//!   ([`ReloadLoss::ExtensionHostRequest`]); the child is replaced and
//!   the previous generation's session binding is fenced, so the replacement
//!   must re-establish it.
//! * A delegated **worker mid-call** loses the in-flight call
//!   ([`ReloadLoss::WorkerCall`]); its durable record survives and stays
//!   reattachable.
//!
//! Because sampling is metadata-only, two further limits are real and
//! documented rather than hidden: a change that leaves `mtime`, size,
//! type, and presence identical is not observed (a same-size rewrite inside
//! one filesystem timestamp granule), and a watched file rewritten repeatedly
//! with different metadata but identical contents is reloaded again. Neither
//! is detected by hashing contents, which this module deliberately never reads:
//! it must not be able to leak a file's contents into a transcript notice.
//!
//! # Deliberately out of scope
//!
//! * Applying a layer. The caller owns `/reload`'s transactional resource
//!   reload, `ExecutableExtensions::reload()`, and the re-exec plan; this
//!   module only selects layers, orders them, and reports.
//! * Watching unbounded trees. Directory targets expand to a fixed depth and a
//!   fixed per-poll inspection budget, and the budget being hit is reported.
//!   The budget bounds `symlink_metadata` calls; directory reads stop with it,
//!   and the layer whose enumeration stopped is recorded as capped per layer
//!   (`ScanMeta::capped`), with every entry already read but not inspected
//!   counted (`ScanMeta::skipped`) — never silently ignored.
//! * Worker-level reload. Delegated workers keep running across a resources or
//!   extensions reload; only a forced/host reload can interrupt one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::config::Config;

/// Boundary type shared with the existing theme reload engine. A reload is
/// admitted only at [`ReloadBoundary::Idle`]; [`ReloadBoundary::Busy`] keeps the
/// evidence queued exactly like the `PendingIdleAction` path.
/// The only boundary at which a loaded resource may be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadBoundary {
    /// The prompt is idle and no active run or modal owns the shell.
    Idle,
    /// Input, a model run, or a modal currently owns the shell.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Busy remains part of the supervisor safety contract; production callers currently enter only at idle boundaries."
        )
    )]
    Busy,
}

/// Default interval between filesystem samples.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// Lower bound on a configured poll interval; also the fastest idle tick.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Upper bound on a configured poll interval.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(300);
/// Default save-burst debounce.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(200);
/// Hard ceiling on a burst: a queued change is flushed by this delay even if
/// more changes keep arriving.
pub const MAX_DEBOUNCE: Duration = Duration::from_secs(2);
/// Default number of metadata inspections one poll may perform.
pub const DEFAULT_MAX_INSPECTIONS_PER_POLL: usize = 512;
/// Hard cap on the per-poll inspection budget. [`ReloadWatcher::scan`] clamps
/// every incoming budget into `1..=MAX_INSPECTIONS_PER_POLL`, so this ceiling
/// cannot be raised by a caller.
pub const MAX_INSPECTIONS_PER_POLL: usize = 4096;
/// Directory expansion depth below a watch target (target → entry → entry).
pub const MAX_SCAN_DEPTH: usize = 2;
/// Paths named per layer in a report; the rest become an overflow count.
pub const MAX_REPORTED_PATHS_PER_LAYER: usize = 4;
/// Caller-supplied notes retained per layer.
pub const MAX_LAYER_NOTES: usize = 8;
/// Caller-supplied detached/reattached labels retained per layer.
pub const MAX_LAYER_LABELS: usize = 8;
/// Bytes retained per rendered note. Sized so a note naming every reported
/// path of a layer still fits without being cut.
pub const MAX_NOTE_BYTES: usize = 400;
/// Bytes retained per path or caller-supplied label inside a note.
pub const MAX_LABEL_BYTES: usize = 64;
/// Transcript notices one report may contain.
pub const MAX_NOTICES: usize = 8;
/// Watch targets one set may hold.
pub const MAX_WATCH_TARGETS: usize = 4096;

/// One reloadable layer, in the fixed order a pass applies them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReloadLayer {
    /// Skills, prompts, themes, context files, settings, keybindings.
    Resources,
    /// Executable extension child processes.
    Extensions,
    /// The octet process image.
    Host,
}

impl ReloadLayer {
    /// Fixed application order: resources → extensions → host.
    pub const ORDER: [ReloadLayer; 3] = [Self::Resources, Self::Extensions, Self::Host];

    /// Short label used in bounded transcript wording.
    pub fn label(self) -> &'static str {
        match self {
            Self::Resources => "resources",
            Self::Extensions => "extensions",
            Self::Host => "host",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Resources => 0,
            Self::Extensions => 1,
            Self::Host => 2,
        }
    }
}

/// Why a pass was requested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReloadRequester {
    /// A watched path changed: the pass the sampler queues for itself.
    Watcher,
    /// API 0.3 `session/reload` from an extension process: Pi's `ctx.reload()`.
    /// The protocol request carries no sender identity, so the notice names the
    /// method rather than claiming to know which extension asked.
    ExtensionSessionReload,
    /// The explicit force path.
    Forced,
}

impl ReloadRequester {
    /// Short label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Watcher => "watcher",
            Self::ExtensionSessionReload => "extension",
            Self::Forced => "forced",
        }
    }

    /// Bounded description, safe for a transcript.
    pub fn describe(&self) -> String {
        match self {
            Self::ExtensionSessionReload => "extension session/reload".to_owned(),
            other => other.label().to_owned(),
        }
    }
}

/// The interface's explicit live-reload actions, as typed by the owner.
///
/// Plain `/reload` keeps its existing transactional path untouched; these two
/// documented flags hand control to the supervisor, which is the only place
/// that can preview a pass or name its losses. [`Self::parse`] matches the whole
/// trimmed invocation, so every other command still belongs to
/// `commands::parse` — including `/reload` itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadUserAction {
    /// Report what a pass would do now and change nothing.
    DryRun,
    /// Take a pass now, at any boundary, naming every loss.
    Force,
}

impl ReloadUserAction {
    /// Recognize `/reload --dry-run` and `/reload --force` exactly.
    pub fn parse(input: &str) -> Option<Self> {
        let mut parts = input.split_whitespace();
        if parts.next()? != "/reload" {
            return None;
        }
        let action = match parts.next()? {
            "--dry-run" => Self::DryRun,
            "--force" => Self::Force,
            _ => return None,
        };
        parts.next().is_none().then_some(action)
    }

    /// The exact text that selects this action.
    #[cfg(test)]
    pub fn label(self) -> &'static str {
        match self {
            Self::DryRun => "/reload --dry-run",
            Self::Force => "/reload --force",
        }
    }
}

/// Work that a reload can destroy. Each variant is a named, documented loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReloadLoss {
    /// The in-flight provider call dies; already-persisted records survive.
    ModelCall,
    /// The in-flight tool call is abandoned mid-effect.
    ToolCall,
    /// An extension restart drops that extension's in-flight host request.
    ExtensionHostRequest,
    /// A delegated worker's in-flight call is lost; its durable record survives
    /// and stays reattachable.
    WorkerCall,
}

impl ReloadLoss {
    /// Every named loss, in reporting order.
    pub const ALL: [ReloadLoss; 4] = [
        Self::ModelCall,
        Self::ToolCall,
        Self::ExtensionHostRequest,
        Self::WorkerCall,
    ];

    /// Bounded, secret-free description.
    pub fn description(self) -> &'static str {
        match self {
            Self::ModelCall => {
                "the in-flight model call is abandoned; persisted records survive"
            }
            Self::ToolCall => {
                "the in-flight tool call is abandoned; a partial effect may remain"
            }
            Self::ExtensionHostRequest => {
                "an extension restart drops that extension's in-flight host request"
            }
            Self::WorkerCall => {
                "a worker's in-flight call is lost; its durable record survives and stays reattachable"
            }
        }
    }

    /// Short transcript label.
    pub fn label(self) -> &'static str {
        match self {
            Self::ModelCall => "in-flight model call",
            Self::ToolCall => "in-flight tool call",
            Self::ExtensionHostRequest => "extension host request in flight",
            Self::WorkerCall => "worker mid-call",
        }
    }
}

/// Supervisor settings. Copyable so a resolved `Config` can hand it over by value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReloadSettings {
    /// Whether sampling and applying happen at all.
    pub enabled: bool,
    /// Whether an executable change may queue the host layer for an automatic
    /// re-exec. Off by default: the resources and extensions layers stay
    /// automatic, but replacing the process image needs its own explicit
    /// opt-in (`reload_host = true`) or `/reload --force`.
    pub host_enabled: bool,
    /// Interval between filesystem samples.
    pub poll_interval: Duration,
    /// Save-burst debounce.
    pub debounce: Duration,
    /// Metadata inspections one sample may perform.
    pub max_inspections_per_poll: usize,
}

impl Default for ReloadSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            // Host re-exec is never armed by the generic live-reload default:
            // one bool (`reload = true`) must not be able to `execve` a
            // replaced `current_exe`.
            host_enabled: false,
            poll_interval: DEFAULT_POLL_INTERVAL,
            debounce: DEFAULT_DEBOUNCE,
            max_inspections_per_poll: DEFAULT_MAX_INSPECTIONS_PER_POLL,
        }
    }
}

impl ReloadSettings {
    /// Disabled settings: no sampling, no plans.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Clamp every value into its documented range.
    pub fn sanitized(self) -> Self {
        Self {
            enabled: self.enabled,
            host_enabled: self.host_enabled,
            poll_interval: clamp_duration(self.poll_interval, MIN_POLL_INTERVAL, MAX_POLL_INTERVAL),
            debounce: if self.debounce > MAX_DEBOUNCE {
                MAX_DEBOUNCE
            } else {
                self.debounce
            },
            max_inspections_per_poll: self
                .max_inspections_per_poll
                .clamp(1, MAX_INSPECTIONS_PER_POLL),
        }
    }

    /// Idle-tick interval: short enough to catch the debounce, never shorter
    /// than [`MIN_POLL_INTERVAL`] and never longer than the poll interval.
    ///
    /// Sampling is still gated by [`ReloadSupervisor::should_scan`], so a short
    /// tick costs a wake-up, not a scan.
    pub fn tick_interval(self) -> Duration {
        let debounce = if self.debounce < MIN_POLL_INTERVAL {
            MIN_POLL_INTERVAL
        } else {
            self.debounce
        };
        if debounce < self.poll_interval {
            debounce
        } else {
            self.poll_interval
        }
    }
}

/// Metadata-only fingerprint of one path.
///
/// `modified` keeps the filesystem's own precision; `len`, `is_dir`,
/// `is_symlink`, and presence catch the rest. Contents are never read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileFingerprint {
    /// Whether the path exists at all.
    pub present: bool,
    /// Whether the path is a directory.
    pub is_dir: bool,
    /// Whether the path itself is a symlink (never followed by the scanner).
    pub is_symlink: bool,
    /// Byte length, or `0` for a missing path or a directory.
    pub len: u64,
    /// Last modification time, when the platform reports one.
    pub modified: Option<SystemTime>,
}

impl FileFingerprint {
    /// The fingerprint of a path that does not exist.
    pub const ABSENT: Self = Self {
        present: false,
        is_dir: false,
        is_symlink: false,
        len: 0,
        modified: None,
    };

    /// Whether this fingerprint differs from `previous` in a way a reload has
    /// to react to: presence, kind, size, or modification time.
    ///
    /// Spelled out rather than left to a derived `PartialEq` so the rule is
    /// visible where it is applied, and so every field of the fingerprint is
    /// part of the decision.
    fn differs_from(&self, previous: &Self) -> bool {
        self.present != previous.present
            || self.is_dir != previous.is_dir
            || self.is_symlink != previous.is_symlink
            || self.len != previous.len
            || self.modified != previous.modified
    }

    /// Fingerprint an existing `std::fs` metadata value.
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        let file_type = metadata.file_type();
        Self {
            present: true,
            is_dir: file_type.is_dir(),
            is_symlink: file_type.is_symlink(),
            len: if file_type.is_dir() {
                0
            } else {
                metadata.len()
            },
            modified: metadata.modified().ok(),
        }
    }
}

/// One watched path and its latest fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchedFingerprint {
    path: PathBuf,
    layer: ReloadLayer,
    fingerprint: FileFingerprint,
}

impl WatchedFingerprint {
    /// Build one observation.
    pub fn new(path: PathBuf, layer: ReloadLayer, fingerprint: FileFingerprint) -> Self {
        Self {
            path,
            layer,
            fingerprint,
        }
    }

    /// Observed path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Layer that owns the path.
    pub fn layer(&self) -> ReloadLayer {
        self.layer
    }

    /// Observed fingerprint.
    pub fn fingerprint(&self) -> FileFingerprint {
        self.fingerprint
    }
}

/// The resolved executable and its fingerprint.
///
/// The path is part of the observation on purpose: a package-manager update
/// that swaps a symlink or moves a versioned directory changes the path even
/// when the target's metadata looks identical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutableFingerprint {
    path: PathBuf,
    fingerprint: FileFingerprint,
}

impl ExecutableFingerprint {
    /// Build one executable observation.
    pub fn new(path: PathBuf, fingerprint: FileFingerprint) -> Self {
        Self { path, fingerprint }
    }

    /// The path `std::env::current_exe()` resolved to.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Result accounting for one sampling pass.
///
/// The cap is recorded at two different levels on purpose:
///
/// * [`Self::capped`] — per layer, a boolean: the budget was exhausted while
///   that layer was being enumerated, so part of the layer is unobserved. This
///   boolean is the record; it is what refuses a disappearance inference and
///   what makes a layer report [`SkipReason::WatcherCapReached`].
/// * [`Self::skipped`] — per layer, a count: candidates the scanner had already
///   read (a target, or an entry of a directory it did read) but could not
///   inspect. A layer can be capped with a skipped count of zero, because a
///   directory the budget never read has unknown contents and is therefore not
///   counted — that is why the boolean, not the count, is the rule.
///
/// The depth bound ([`MAX_SCAN_DEPTH`]) is *not* a cap: looking no deeper than a
/// fixed level is a rule about which files a layer consists of, so it never sets
/// either field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanMeta {
    /// Metadata inspections performed.
    pub inspected: usize,
    /// Inspections skipped because the budget was exhausted, per layer.
    pub skipped: [usize; 3],
    /// Whether the budget stopped this layer's enumeration, per layer.
    pub capped: [bool; 3],
}

impl ScanMeta {
    /// Total skipped count.
    pub fn skipped_total(&self) -> usize {
        self.skipped.iter().sum()
    }

    /// Whether the budget stopped this layer's enumeration.
    pub fn capped_for(&self, layer: ReloadLayer) -> bool {
        self.capped[layer.index()]
    }

    /// Whether the inspection budget was hit at all.
    pub fn truncated(&self) -> bool {
        self.capped.iter().any(|capped| *capped)
    }
}

/// One bounded sampling pass. Pure data: the state machine consumes it and
/// performs no I/O of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    entries: Vec<WatchedFingerprint>,
    executable: Option<ExecutableFingerprint>,
    meta: ScanMeta,
}

impl Scan {
    /// Assemble a scan. Used by the scanner, by tests, and by embedders that
    /// sample through some other mechanism.
    pub fn from_parts(
        entries: Vec<WatchedFingerprint>,
        executable: Option<ExecutableFingerprint>,
        meta: ScanMeta,
    ) -> Self {
        Self {
            entries,
            executable,
            meta,
        }
    }

    /// Every observed path.
    pub fn entries(&self) -> &[WatchedFingerprint] {
        &self.entries
    }

    /// The resolved executable, when the platform could report one.
    pub fn executable(&self) -> Option<&ExecutableFingerprint> {
        self.executable.as_ref()
    }

    /// Inspections performed.
    pub fn inspected(&self) -> usize {
        self.meta.inspected
    }

    /// Whether the budget stopped this layer's enumeration.
    pub fn capped_for(&self, layer: ReloadLayer) -> bool {
        self.meta.capped_for(layer)
    }

    /// Full accounting.
    pub fn meta(&self) -> ScanMeta {
        self.meta
    }

    /// Whether the inspection budget was hit.
    pub fn truncated(&self) -> bool {
        self.meta.truncated()
    }
}

/// The only I/O the scanner performs; injectable so the decision rules never
/// need a filesystem.
pub trait MetadataSource {
    /// Metadata for `path` without following a final symlink. `None` means the
    /// path could not be inspected (treat as missing).
    fn symlink_metadata(&self, path: &Path) -> Option<FileFingerprint>;

    /// Metadata for `path` following symlinks; used for the resolved
    /// executable, where the target's own mtime is the interesting signal.
    fn target_metadata(&self, path: &Path) -> Option<FileFingerprint>;

    /// Immediate entries of `path` in deterministic order. A missing or
    /// unreadable directory yields no entries.
    fn read_dir(&self, path: &Path) -> Vec<PathBuf>;

    /// The executable currently selected by the package manager or PATH. This
    /// is re-resolved on every call, never cached.
    fn current_exe(&self) -> Option<PathBuf>;
}

/// Real filesystem sampling: `std::fs` metadata plus a fresh
/// `std::env::current_exe()` per poll.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMetadata;

impl MetadataSource for SystemMetadata {
    fn symlink_metadata(&self, path: &Path) -> Option<FileFingerprint> {
        std::fs::symlink_metadata(path)
            .ok()
            .as_ref()
            .map(FileFingerprint::from_metadata)
    }

    fn target_metadata(&self, path: &Path) -> Option<FileFingerprint> {
        std::fs::metadata(path)
            .ok()
            .as_ref()
            .map(FileFingerprint::from_metadata)
    }

    fn read_dir(&self, path: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let mut paths = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
}

/// One watched path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchTarget {
    layer: ReloadLayer,
    path: PathBuf,
}

impl WatchTarget {
    /// Layer the path belongs to.
    pub fn layer(&self) -> ReloadLayer {
        self.layer
    }

    /// Watched path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Everything the supervisor samples, grouped by layer.
///
/// A path is fingerprinted as given; if it happens to be a directory it is also
/// expanded to [`MAX_SCAN_DEPTH`] levels, so a target may be a file, a
/// directory, or a path that does not exist yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReloadWatchSet {
    targets: Vec<WatchTarget>,
    seen: BTreeSet<(ReloadLayer, PathBuf)>,
    dropped: usize,
}

impl ReloadWatchSet {
    /// An empty set. The host layer needs no target: the scanner re-resolves
    /// the executable on every poll.
    pub fn new() -> Self {
        Self::default()
    }

    /// Watch one path for one layer. Returns `false` when the path is already
    /// watched for that layer or the set is full.
    pub fn watch_path(&mut self, layer: ReloadLayer, path: impl Into<PathBuf>) -> bool {
        let path = path.into();
        if self.targets.len() >= MAX_WATCH_TARGETS {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        if !self.seen.insert((layer, path.clone())) {
            return false;
        }
        self.targets.push(WatchTarget { layer, path });
        true
    }

    /// Watch many paths for one layer.
    pub fn watch_paths(
        &mut self,
        layer: ReloadLayer,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> usize {
        let mut added = 0;
        for path in paths {
            if self.watch_path(layer, path) {
                added += 1;
            }
        }
        added
    }

    /// Every configured target, in insertion order.
    pub fn targets(&self) -> &[WatchTarget] {
        &self.targets
    }

    /// Number of targets that could not be added.
    #[cfg(test)]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Report incomplete watch coverage, without a routine startup banner.
    pub fn limit_notice(&self) -> Option<String> {
        (self.dropped > 0).then(|| {
            format!(
                "live reload: watch limit reached; {} path{} not watched",
                self.dropped,
                plural(self.dropped)
            )
        })
    }

    /// On-demand settings detail for `/reload --dry-run`, not startup output.
    /// Counts and durations are bounded and secret-free. Name the host opt-in
    /// explicitly so a resource watcher never implies automatic re-exec.
    pub fn arming_notice(&self, settings: ReloadSettings) -> String {
        if !settings.enabled {
            return "live reload: disabled (set reload = true to arm it)".to_owned();
        }
        let dropped = if self.dropped == 0 {
            String::new()
        } else {
            format!(", {} over the target cap", self.dropped)
        };
        let host = if settings.host_enabled {
            "host re-exec armed by reload_host = true".to_owned()
        } else {
            "host re-exec off (use /reload --force or reload_host = true)".to_owned()
        };
        sanitize_note(&format!(
            "live reload armed: {} watched paths{dropped}, poll {} ms, debounce {} ms, applied at \
             the idle prompt; {host}; /reload --dry-run previews a pass",
            self.targets.len(),
            settings.poll_interval.as_millis(),
            settings.debounce.as_millis()
        ))
    }

    /// The resource, extension, and context paths this product reads today.
    ///
    /// This mirrors the resolvers rather than enumerating the tree:
    /// [`crate::resource_resolver`] reads global then trusted-project then
    /// explicit roots, [`crate::resources`] reads `AGENTS.md` from the global
    /// directory and from every workspace-to-cwd directory, the keymap manager
    /// reads `<octet dir>/keybindings.json`, and installed extensions live under
    /// the package root. Missing paths are intentionally kept: a directory that
    /// appears later is then a change.
    pub fn from_config(config: &Config) -> Self {
        let global_octet_dir =
            crate::cli::global_config_path().and_then(|path| path.parent().map(Path::to_path_buf));
        let scopes = ReloadScopes {
            workspace: Some(config.workspace.clone()),
            invocation_cwd: Some(config.invocation_cwd.clone()),
            workspace_trusted: config.workspace_trusted,
            context_files: config.context_files,
            home: dirs::home_dir(),
            global_octet_dir,
            extensions_root: crate::extension_package::extensions_root().ok(),
            skill_paths: config.skill_paths.clone(),
            prompt_paths: config.prompt_paths.clone(),
            theme_paths: config.theme_paths.clone(),
            extension_paths: config.extension_paths.clone(),
        };
        Self::from_scopes(&scopes)
    }

    /// Derive the watch set from resolved scopes. Kept separate from
    /// [`Self::from_config`] so the derivation is testable without a `Config`.
    pub fn from_scopes(scopes: &ReloadScopes) -> Self {
        let mut watches = Self::new();
        if let Some(global) = scopes.global_octet_dir.as_deref() {
            for directory in ["skills", "prompts", "themes"] {
                watches.watch_path(ReloadLayer::Resources, global.join(directory));
            }
            watches.watch_path(ReloadLayer::Resources, global.join("keybindings.json"));
            watches.watch_path(ReloadLayer::Resources, global.join("config.toml"));
            if scopes.context_files {
                watches.watch_path(ReloadLayer::Resources, global.join("AGENTS.md"));
            }
        }
        if let Some(home) = scopes.home.as_deref() {
            watches.watch_path(ReloadLayer::Resources, home.join(".agents").join("skills"));
        }
        if let Some(workspace) = scopes.workspace.as_deref() {
            if scopes.workspace_trusted {
                let project = workspace.join(".octet");
                for directory in ["skills", "prompts", "themes", "extensions"] {
                    let layer = if directory == "extensions" {
                        ReloadLayer::Extensions
                    } else {
                        ReloadLayer::Resources
                    };
                    watches.watch_path(layer, project.join(directory));
                }
                watches.watch_path(ReloadLayer::Resources, project.join("config.toml"));
            }
            if scopes.context_files {
                let cwd = scopes.invocation_cwd.as_deref().unwrap_or(workspace);
                for directory in crate::resources::dirs_from_workspace_to_cwd(workspace, cwd) {
                    watches.watch_path(ReloadLayer::Resources, directory.join("AGENTS.md"));
                }
            }
        }
        if let Some(root) = scopes.extensions_root.clone() {
            watches.watch_path(ReloadLayer::Extensions, root);
        }
        watches.watch_paths(ReloadLayer::Resources, scopes.skill_paths.iter().cloned());
        watches.watch_paths(ReloadLayer::Resources, scopes.prompt_paths.iter().cloned());
        watches.watch_paths(ReloadLayer::Resources, scopes.theme_paths.iter().cloned());
        watches.watch_paths(
            ReloadLayer::Extensions,
            scopes.extension_paths.iter().cloned(),
        );
        watches
    }
}

/// Inputs for [`ReloadWatchSet::from_scopes`].
///
/// This is the whole filesystem policy of the supervisor in one place; nothing
/// here is read, only referenced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReloadScopes {
    /// Workspace root.
    pub workspace: Option<PathBuf>,
    /// Invocation directory, used for the workspace-to-cwd `AGENTS.md` walk.
    pub invocation_cwd: Option<PathBuf>,
    /// Whether project (`.octet`) resources are trusted for this process.
    pub workspace_trusted: bool,
    /// Whether `AGENTS.md` context files are loaded at all.
    pub context_files: bool,
    /// User home, for the `.agents/skills` root.
    pub home: Option<PathBuf>,
    /// Resolved `~/.octet` directory.
    pub global_octet_dir: Option<PathBuf>,
    /// Resolved installed-extension root.
    pub extensions_root: Option<PathBuf>,
    /// Explicit `--skill-dir` paths.
    pub skill_paths: Vec<PathBuf>,
    /// Explicit `--prompt-template` paths.
    pub prompt_paths: Vec<PathBuf>,
    /// Explicit legacy theme paths.
    pub theme_paths: Vec<PathBuf>,
    /// Explicit `--extension-dir` paths.
    pub extension_paths: Vec<PathBuf>,
}

/// Bounded scanner: turns a watch set into a [`Scan`].
#[derive(Clone, Debug)]
pub struct ReloadWatcher {
    watches: ReloadWatchSet,
}

impl ReloadWatcher {
    /// Build a scanner over `watches`.
    ///
    /// The inspection budget deliberately does **not** live here. It belongs to
    /// [`ReloadSettings`], which the supervisor already owns — a scanner holding
    /// a second copy of a policy is a scanner that can disagree with the
    /// settings the caller constructed. Pass the budget to [`Self::scan`], or
    /// use [`Self::poll`], which reads it from the supervisor.
    pub fn new(watches: ReloadWatchSet) -> Self {
        Self { watches }
    }

    /// The watch set being sampled.
    pub fn watches(&self) -> &ReloadWatchSet {
        &self.watches
    }

    /// Sample once with `max_inspections` as the per-poll inspection budget.
    ///
    /// The budget is clamped into `1..=`[`MAX_INSPECTIONS_PER_POLL`], so no
    /// caller can disable the bound with `0` or `usize::MAX`. The rule, exactly:
    ///
    /// * at most `max_inspections` `symlink_metadata` calls happen per poll;
    /// * once the budget is exhausted the scanner stops expanding directories,
    ///   so no further directory read happens either (a directory is only read
    ///   for a target or entry whose own inspection was admitted, which bounds
    ///   the directory reads by the budget as well);
    /// * the layer whose enumeration stopped is recorded in
    ///   [`ScanMeta::capped`] — including the case where a directory could not
    ///   be read at all, where there is nothing to count;
    /// * every entry already read but not inspected is counted into
    ///   [`ScanMeta::skipped`] for its layer.
    pub fn scan(&self, source: &impl MetadataSource, max_inspections: usize) -> Scan {
        let mut budget = ScanBudget {
            max_inspections: max_inspections.clamp(1, MAX_INSPECTIONS_PER_POLL),
            ..ScanBudget::default()
        };
        let mut entries = Vec::new();

        for target in self.watches.targets() {
            let layer = target.layer();
            if budget.exhausted() {
                budget.skip(layer);
                continue;
            }
            let fingerprint = source
                .symlink_metadata(target.path())
                .unwrap_or(FileFingerprint::ABSENT);
            budget.inspect();
            let is_directory = fingerprint.present && fingerprint.is_dir && !fingerprint.is_symlink;
            entries.push(WatchedFingerprint::new(
                target.path().to_path_buf(),
                layer,
                fingerprint,
            ));
            if is_directory {
                Self::expand(source, layer, target.path(), 1, &mut budget, &mut entries);
            }
        }

        let executable = source.current_exe().map(|path| {
            let fingerprint = source
                .target_metadata(&path)
                .unwrap_or(FileFingerprint::ABSENT);
            ExecutableFingerprint::new(path, fingerprint)
        });

        Scan::from_parts(entries, executable, budget.meta())
    }

    fn expand(
        source: &impl MetadataSource,
        layer: ReloadLayer,
        directory: &Path,
        depth: usize,
        budget: &mut ScanBudget,
        entries: &mut Vec<WatchedFingerprint>,
    ) {
        // The depth bound is a rule about which files a layer consists of, so
        // stopping there is not an unfinished scan and is not recorded as one.
        if depth > MAX_SCAN_DEPTH {
            return;
        }
        // Exhausting the budget is different: this directory was admitted, but
        // its contents cannot be known without reading it, so the layer is
        // recorded as capped without inventing a skipped count.
        if budget.exhausted() {
            budget.stop(layer);
            return;
        }
        let children = source.read_dir(directory);
        for child in children {
            if budget.exhausted() {
                budget.skip(layer);
                continue;
            }
            let fingerprint = source
                .symlink_metadata(&child)
                .unwrap_or(FileFingerprint::ABSENT);
            budget.inspect();
            let is_directory = fingerprint.present && fingerprint.is_dir && !fingerprint.is_symlink;
            entries.push(WatchedFingerprint::new(child.clone(), layer, fingerprint));
            if is_directory {
                Self::expand(source, layer, &child, depth + 1, budget, entries);
            }
        }
    }

    /// One idle-tick entry point: sample when the poll interval has elapsed,
    /// then feed the state machine.
    ///
    /// The budget comes from the supervisor's own settings, so the sampling
    /// cadence and the per-poll cost always come from one [`ReloadSettings`].
    ///
    /// Returns `None` when the supervisor is disabled or the poll interval has
    /// not elapsed, so a short tick costs a wake-up rather than a scan.
    pub fn poll(
        &self,
        supervisor: &mut ReloadSupervisor,
        source: &impl MetadataSource,
        now: Instant,
    ) -> Option<ObserveSummary> {
        if !supervisor.is_enabled() || !supervisor.should_scan(now) {
            return None;
        }
        let max_inspections = supervisor.settings().max_inspections_per_poll;
        let scan = self.scan(source, max_inspections);
        Some(supervisor.observe(now, &scan))
    }
}

/// Mutable accounting for one scan. Private: [`ScanMeta`] is the value that
/// leaves the scanner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ScanBudget {
    max_inspections: usize,
    inspected: usize,
    skipped: [usize; 3],
    capped: [bool; 3],
}

impl ScanBudget {
    fn exhausted(&self) -> bool {
        self.inspected >= self.max_inspections
    }

    fn inspect(&mut self) {
        self.inspected = self.inspected.saturating_add(1);
    }

    /// One candidate this layer could enumerate but the budget could not
    /// inspect. Counting it also records the cap for that layer.
    fn skip(&mut self, layer: ReloadLayer) {
        self.capped[layer.index()] = true;
        self.skipped[layer.index()] = self.skipped[layer.index()].saturating_add(1);
    }

    /// A budget stop with nothing countable: a directory the scanner was
    /// admitted to but never read, whose contents are therefore unknown.
    fn stop(&mut self, layer: ReloadLayer) {
        self.capped[layer.index()] = true;
    }

    fn meta(&self) -> ScanMeta {
        ScanMeta {
            inspected: self.inspected,
            skipped: self.skipped,
            capped: self.capped,
        }
    }
}

/// What one observation pass saw, before any boundary decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObserveSummary {
    /// True when this pass established the baseline instead of comparing to it.
    /// The first pass never reloads.
    pub first_observation: bool,
    /// Layers with at least one changed path.
    pub changed_layers: Vec<ReloadLayer>,
    /// Number of changed paths observed (before per-layer reporting caps).
    pub changed_paths: usize,
    /// Inspections performed by this pass.
    pub inspected: usize,
    /// Whether this pass hit the inspection budget.
    pub cap_reached: bool,
    /// Layers currently queued for the next idle boundary.
    pub queued_layers: Vec<ReloadLayer>,
    /// Whether the queued work is past its debounce deadline now.
    pub due: bool,
}

impl ObserveSummary {
    /// Whether anything changed in this pass.
    pub fn anything_changed(&self) -> bool {
        !self.changed_layers.is_empty()
    }

    /// Bounded notice for a pass that hit the per-poll inspection budget.
    ///
    /// The cap is a real limit on what the watcher could see, so it is reported
    /// instead of being inferred from an absence of changes.
    pub fn cap_notice(&self) -> Option<String> {
        self.cap_reached.then(|| {
            sanitize_note(&format!(
                "live reload: inspection cap reached after {} paths; part of a layer was not \
                 inspected, so it is reported as capped rather than as a change",
                self.inspected
            ))
        })
    }
}

/// Per-layer evidence for one queued or admitted pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LayerChange {
    changed: Vec<PathBuf>,
    overflow: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BaselineEntry {
    layer: ReloadLayer,
    fingerprint: FileFingerprint,
}

/// Cancellation/commit token, mirroring the theme reload engine's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReloadToken {
    generation: u64,
    sequence: u64,
}

/// How far a layer got in one pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerOutcome {
    /// The layer had no stale evidence.
    Skipped(SkipReason),
    /// The layer was stale and the caller reloaded it.
    Reloaded,
    /// The layer was stale; a dry run reported it and applied nothing.
    WouldReload,
    /// The caller's reload failed; the previous state is kept.
    Failed,
}

/// Why a layer was skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// No watched path in this layer changed.
    NoChange,
    /// Part of this layer was not inspected because the budget was hit. See
    /// [`ScanMeta::capped_for`].
    WatcherCapReached,
    /// The layer was not part of this pass.
    NotSelected,
}

/// One layer's line in the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerReport {
    /// Layer this line describes.
    pub layer: ReloadLayer,
    /// How far the layer got.
    pub outcome: LayerOutcome,
    /// Changed paths, capped at [`MAX_REPORTED_PATHS_PER_LAYER`].
    pub changed: Vec<PathBuf>,
    /// Changed paths not listed because of the cap.
    pub changed_overflow: usize,
    /// Names detached by this reload (fenced bindings, replaced children).
    pub detached: Vec<String>,
    /// Names that will be reattached (rebuilt children, resumed session).
    pub reattached: Vec<String>,
    /// Named losses for this layer.
    pub losses: Vec<ReloadLoss>,
    /// Caller-supplied bounded notes.
    pub notes: Vec<String>,
}

impl LayerReport {}

/// One pass, one report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadReport {
    /// Sequence number of the pass.
    pub sequence: u64,
    /// Whether the caller took the force path.
    pub forced: bool,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether the pass was admitted at an idle boundary.
    pub boundary_idle: bool,
    /// Whether the queued evidence was already past its debounce deadline.
    pub due: bool,
    /// Inspections performed before the pass.
    pub inspected_paths: usize,
    /// Inspections skipped by the budget, per layer.
    pub skipped_paths: usize,
    /// Whether the inspection budget was hit.
    pub watcher_cap_reached: bool,
    /// Why the pass happened.
    pub requesters: Vec<ReloadRequester>,
    /// Per-layer lines, in fixed order.
    pub layers: Vec<LayerReport>,
}

impl ReloadReport {
    /// One layer's line.
    #[cfg(test)]
    pub fn layer(&self, layer: ReloadLayer) -> Option<&LayerReport> {
        self.layers.iter().find(|line| line.layer == layer)
    }

    /// Whether this report describes no work at all: nothing was reloaded and
    /// nothing would reload. A dry run with stale layers is therefore not a
    /// no-op, while a dry run with none is.
    #[cfg(test)]
    pub fn is_noop(&self) -> bool {
        self.layers.iter().all(|line| {
            !matches!(
                line.outcome,
                LayerOutcome::Reloaded | LayerOutcome::WouldReload
            )
        })
    }

    /// Every named loss, deduplicated, in canonical order.
    fn named_losses(&self) -> Vec<ReloadLoss> {
        let mut losses = Vec::new();
        for loss in ReloadLoss::ALL {
            if self.layers.iter().any(|line| line.losses.contains(&loss)) {
                losses.push(loss);
            }
        }
        losses
    }

    /// Single transcript line for the pass.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for line in &self.layers {
            match line.outcome {
                LayerOutcome::Reloaded => parts.push(format!("{} reloaded", line.layer.label())),
                LayerOutcome::WouldReload => {
                    parts.push(format!("{} would reload", line.layer.label()));
                }
                LayerOutcome::Failed => parts.push(format!("{} failed", line.layer.label())),
                LayerOutcome::Skipped(_) => {}
            }
        }
        let mut summary = if parts.is_empty() {
            "no stale layers; nothing reloaded".to_owned()
        } else {
            parts.join(", ")
        };
        if self.dry_run {
            summary.push_str(if self.due {
                " (preview: due now)"
            } else {
                " (preview: waiting on the debounce deadline)"
            });
        }
        let losses = self.named_losses().len();
        if losses > 0 {
            summary.push_str(&format!(" ({losses} named loss{})", plural(losses)));
        }
        if self.watcher_cap_reached {
            summary.push_str(&format!(" (watcher cap: {} skipped)", self.skipped_paths));
        }
        let label = if self.dry_run {
            "reload preview"
        } else {
            "reload pass"
        };
        sanitize_note(&format!("{label} #{}: {summary}", self.sequence))
    }

    /// Detailed on-demand notices for the pass. Never contains file contents.
    pub fn notices(&self) -> Vec<String> {
        self.collect_notices(true)
    }

    /// Keep failures, limits, losses and caller diagnostics visible, without
    /// successful layer/path and detach/reattach bookkeeping.
    pub fn diagnostics(&self) -> Vec<String> {
        self.collect_notices(false)
    }

    fn collect_notices(&self, detailed: bool) -> Vec<String> {
        let mut notices = Vec::new();
        if self.forced {
            notices.push(
                "reload (forced): in-flight work is abandoned; persisted records survive and \
                 workers stay reattachable"
                    .to_owned(),
            );
        }
        // Detailed output distinguishes the watcher noticing a save from an
        // owner or extension asking for a pass.
        if let Some(reason) = self
            .requesters
            .iter()
            .find(|requester| {
                detailed
                    && !matches!(
                        requester,
                        ReloadRequester::Watcher | ReloadRequester::Forced
                    )
            })
            .map(ReloadRequester::describe)
        {
            notices.push(format!("reload: requested by {reason}"));
        }
        for line in &self.layers {
            match line.outcome {
                LayerOutcome::Reloaded if detailed => notices.push(format!(
                    "reload: {} reloaded{}",
                    line.layer.label(),
                    changed_clause(line)
                )),
                LayerOutcome::WouldReload => notices.push(format!(
                    "reload --dry-run: {} would reload{}",
                    line.layer.label(),
                    changed_clause(line)
                )),
                LayerOutcome::Failed => notices.push(format!(
                    "reload: {} reload failed; the previous state is kept",
                    line.layer.label()
                )),
                LayerOutcome::Reloaded | LayerOutcome::Skipped(_) => {}
            }
            if detailed && !line.detached.is_empty() {
                notices.push(format!(
                    "reload: {} detached {}",
                    line.layer.label(),
                    join_labels(&line.detached)
                ));
            }
            if detailed && !line.reattached.is_empty() {
                notices.push(format!(
                    "reload: {} will reattach {}",
                    line.layer.label(),
                    join_labels(&line.reattached)
                ));
            }
            for note in &line.notes {
                notices.push(format!("reload: {}", note));
            }
        }
        if self.watcher_cap_reached {
            notices.push(format!(
                "reload: watcher inspected {} paths and skipped {} (per-poll cap)",
                self.inspected_paths, self.skipped_paths
            ));
        }
        let losses = self.named_losses();
        if !losses.is_empty() {
            notices.push(format!(
                "reload: lost work — {}",
                losses
                    .iter()
                    .map(|loss| loss.label())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        notices.truncate(MAX_NOTICES);
        notices
            .into_iter()
            .map(|notice| sanitize_note(&notice))
            .filter(|notice| !notice.is_empty())
            .collect()
    }
}

/// One admitted pass handed to the caller.
///
/// The caller records what it did per layer and then calls
/// [`ReloadSupervisor::finish`] for the single [`ReloadReport`].
#[derive(Debug)]
pub struct ReloadPlan {
    token: ReloadToken,
    forced: bool,
    dry_run: bool,
    boundary_idle: bool,
    due: bool,
    scan: ScanMeta,
    requesters: Vec<ReloadRequester>,
    layers: Vec<PlannedLayer>,
}

#[derive(Debug)]
struct PlannedLayer {
    layer: ReloadLayer,
    changed: Vec<PathBuf>,
    overflow: usize,
    outcome: Option<LayerOutcome>,
    detached: Vec<String>,
    reattached: Vec<String>,
    losses: Vec<ReloadLoss>,
    notes: Vec<String>,
}

impl ReloadPlan {
    /// Layers included in this pass, in fixed order.
    #[cfg(test)]
    pub fn layers(&self) -> Vec<ReloadLayer> {
        self.layers.iter().map(|line| line.layer).collect()
    }

    /// Whether the pass includes `layer`.
    pub fn contains(&self, layer: ReloadLayer) -> bool {
        self.layers.iter().any(|line| line.layer == layer)
    }

    /// Whether the caller took the force path.
    pub fn is_forced(&self) -> bool {
        self.forced
    }

    /// Record how far a layer got. Ignored for a layer not in the pass.
    pub fn record(&mut self, layer: ReloadLayer, outcome: LayerOutcome) -> bool {
        match self.line_mut(layer) {
            Some(line) => {
                line.outcome = Some(outcome);
                true
            }
            None => false,
        }
    }

    /// Record a successful reload of a layer in the pass.
    pub fn record_reload(&mut self, layer: ReloadLayer) -> bool {
        self.record(layer, LayerOutcome::Reloaded)
    }

    /// Record a borrowed note for a layer (bounded and control-character free
    /// once rendered).
    pub fn note(&mut self, layer: ReloadLayer, note: impl AsRef<str>) -> bool {
        let note = sanitize_note(note.as_ref());
        match self.line_mut(layer) {
            Some(line) if line.notes.len() < MAX_LAYER_NOTES => {
                line.notes.push(note);
                true
            }
            _ => false,
        }
    }

    /// Record names detached by this reload.
    pub fn detached(
        &mut self,
        layer: ReloadLayer,
        names: impl IntoIterator<Item = String>,
    ) -> bool {
        let names = names.into_iter().collect::<Vec<_>>();
        self.record_labels(layer, names, true)
    }

    /// Record names that will be reattached.
    pub fn reattached(
        &mut self,
        layer: ReloadLayer,
        names: impl IntoIterator<Item = String>,
    ) -> bool {
        let names = names.into_iter().collect::<Vec<_>>();
        self.record_labels(layer, names, false)
    }

    fn record_labels(&mut self, layer: ReloadLayer, names: Vec<String>, detached: bool) -> bool {
        match self.line_mut(layer) {
            Some(line) => {
                let target = if detached {
                    &mut line.detached
                } else {
                    &mut line.reattached
                };
                let mut recorded = false;
                for name in names {
                    if target.len() >= MAX_LAYER_LABELS {
                        break;
                    }
                    target.push(sanitize_label(&name));
                    recorded = true;
                }
                recorded
            }
            None => false,
        }
    }

    /// Finish the pass. Missing outcomes become skipped lines; a forced pass
    /// names every loss, and an extensions reload always names the in-flight
    /// host request it drops.
    ///
    /// Private on purpose: a plan must be completed through
    /// [`ReloadSupervisor::finish`], which validates the admission token and
    /// clears the in-flight flag. Completing a plan any other way would leave
    /// the supervisor believing a pass still owns it.
    fn into_report(self) -> ReloadReport {
        let token = self.token;
        let forced = self.forced;
        let dry_run = self.dry_run;
        let boundary_idle = self.boundary_idle;
        let due = self.due;
        let scan = self.scan;
        let requesters = self.requesters;
        let mut layers = Vec::with_capacity(self.layers.len());
        for planned in self.layers {
            let outcome = planned.outcome.unwrap_or_else(|| {
                if scan.capped_for(planned.layer) {
                    LayerOutcome::Skipped(SkipReason::WatcherCapReached)
                } else {
                    LayerOutcome::Skipped(SkipReason::NoChange)
                }
            });
            let mut losses = planned.losses;
            if forced {
                for loss in ReloadLoss::ALL {
                    if !losses.contains(&loss) {
                        losses.push(loss);
                    }
                }
            } else if matches!(outcome, LayerOutcome::Reloaded | LayerOutcome::WouldReload)
                && planned.layer == ReloadLayer::Extensions
                && !losses.contains(&ReloadLoss::ExtensionHostRequest)
            {
                losses.push(ReloadLoss::ExtensionHostRequest);
            }
            layers.push(LayerReport {
                layer: planned.layer,
                outcome,
                changed: planned.changed,
                changed_overflow: planned.overflow,
                detached: planned.detached,
                reattached: planned.reattached,
                losses,
                notes: planned.notes,
            });
        }
        ReloadReport {
            sequence: token.sequence,
            forced,
            dry_run,
            boundary_idle,
            due,
            inspected_paths: scan.inspected,
            skipped_paths: scan.skipped_total(),
            watcher_cap_reached: scan.truncated(),
            requesters,
            layers,
        }
    }

    fn line_mut(&mut self, layer: ReloadLayer) -> Option<&mut PlannedLayer> {
        self.layers.iter_mut().find(|line| line.layer == layer)
    }
}

/// The supervisor: a pure `(now, observed) → decision` state machine.
///
/// Rules, in one place:
///
/// 1. The **first** observation establishes the baseline and never reloads.
/// 2. A path whose fingerprint changed, a path that appeared, a path that
///    disappeared (when that layer was fully inspected), or a re-resolved
///    executable marks its layer **stale**. Stale layers are ordered by
///    [`ReloadLayer::ORDER`].
/// 3. A change sets a debounce deadline of `now + debounce`, clamped to
///    `first_change + `[`MAX_DEBOUNCE`]` so a burst always flushes. Further
///    changes extend the burst only up to that ceiling.
/// 4. Nothing is admitted while `boundary` is [`ReloadBoundary::Busy`] or a
///    pass is already in flight; the evidence stays queued.
/// 5. An explicit request (`/reload`, API 0.3 `session/reload`) marks the
///    resources and extensions layers stale immediately; the host layer is off
///    entirely unless `reload_host = true` armed it, and even then it still
///    requires a real executable change, because a re-exec is only ever
///    justified by evidence. `/reload --force` is the one path that takes every
///    layer now.
/// 6. `force` supersedes queued and in-flight passes (generation bump) and
///    names every loss, because it may be taken while a run owns the session.
/// 7. `dry_run` reports what a pass would do and changes nothing.
#[derive(Debug)]
pub struct ReloadSupervisor {
    settings: ReloadSettings,
    baseline: BTreeMap<PathBuf, BaselineEntry>,
    baseline_executable: Option<ExecutableFingerprint>,
    baseline_ready: bool,
    last_scan: Option<Instant>,
    scan: ScanMeta,
    stale: BTreeMap<ReloadLayer, LayerChange>,
    first_change: Option<Instant>,
    deadline: Option<Instant>,
    requesters: Vec<ReloadRequester>,
    generation: u64,
    next_sequence: u64,
    in_flight: Option<ReloadToken>,
}

impl ReloadSupervisor {
    /// Build a supervisor. Settings are clamped on construction.
    pub fn new(settings: ReloadSettings) -> Self {
        Self {
            settings: settings.sanitized(),
            baseline: BTreeMap::new(),
            baseline_executable: None,
            baseline_ready: false,
            last_scan: None,
            scan: ScanMeta::default(),
            stale: BTreeMap::new(),
            first_change: None,
            deadline: None,
            requesters: Vec::new(),
            generation: 0,
            next_sequence: 0,
            in_flight: None,
        }
    }

    /// Resolved settings.
    pub fn settings(&self) -> ReloadSettings {
        self.settings
    }

    /// Whether sampling and applying happen.
    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    /// Whether a pass currently owns an admitted plan.
    pub fn is_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Whether stale evidence is queued for the next idle boundary.
    pub fn is_queued(&self) -> bool {
        !self.stale.is_empty()
    }

    /// Debounce deadline of the current burst.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Queued layers, in fixed order. Private: the queued set is reported
    /// through [`ObserveSummary::queued_layers`].
    fn queued_in_order(&self) -> Vec<ReloadLayer> {
        self.stale.keys().copied().collect()
    }

    /// Whether the poll interval has elapsed.
    pub fn should_scan(&self, now: Instant) -> bool {
        match self.last_scan {
            Some(last) => now
                .checked_duration_since(last)
                .is_none_or(|elapsed| elapsed >= self.settings.poll_interval),
            None => true,
        }
    }

    /// Whether queued evidence is past its deadline and admissible now.
    pub fn is_due(&self, now: Instant) -> bool {
        self.is_enabled()
            && self.in_flight.is_none()
            && self.is_queued()
            && self.deadline.is_some_and(|deadline| now >= deadline)
    }

    /// Compare one bounded scan against the baseline and update staleness.
    ///
    /// Pure: no I/O, no boundary change. Safe to call every tick.
    pub fn observe(&mut self, now: Instant, scan: &Scan) -> ObserveSummary {
        self.scan = scan.meta();
        self.last_scan = Some(now);

        let mut summary = ObserveSummary {
            first_observation: !self.baseline_ready,
            inspected: scan.inspected(),
            cap_reached: scan.truncated(),
            ..ObserveSummary::default()
        };
        if !self.settings.enabled {
            summary.queued_layers = self.queued_in_order();
            return summary;
        }

        let first = !self.baseline_ready;
        let mut changed: BTreeMap<ReloadLayer, Vec<PathBuf>> = BTreeMap::new();
        let mut observed = BTreeSet::new();
        for entry in scan.entries() {
            let path = entry.path().to_path_buf();
            observed.insert(path.clone());
            let layer = entry.layer();
            let fingerprint = entry.fingerprint();
            match self.baseline.get(entry.path()) {
                Some(previous)
                    if !previous.fingerprint.differs_from(&fingerprint)
                        && previous.layer == layer => {}
                Some(_) => changed.entry(layer).or_default().push(path.clone()),
                None if !first => changed.entry(layer).or_default().push(path.clone()),
                None => {}
            }
            self.baseline
                .insert(path, BaselineEntry { layer, fingerprint });
        }

        if !first {
            let mut vanished = Vec::new();
            for (path, entry) in &self.baseline {
                if observed.contains(path) {
                    continue;
                }
                // A full enumeration of this layer's targets proves the path is
                // gone; a capped layer proves nothing, so its entries are kept
                // and no change is claimed from the gap.
                if !scan.capped_for(entry.layer) {
                    vanished.push((path.clone(), entry.layer));
                }
            }
            for (path, layer) in vanished {
                changed.entry(layer).or_default().push(path.clone());
                // Drop the entry so a vanished path is reported once instead of
                // reloading on every poll; if the path comes back it is a fresh
                // observation, which is a change again.
                self.baseline.remove(&path);
            }
        }

        if !first {
            let executable_change = match scan.executable() {
                Some(current) => {
                    let changed_now = match &self.baseline_executable {
                        Some(previous) => {
                            previous.path != current.path
                                || previous.fingerprint.differs_from(&current.fingerprint)
                        }
                        None => true,
                    };
                    self.baseline_executable = Some(current.clone());
                    if changed_now {
                        Some(current.path().to_path_buf())
                    } else {
                        None
                    }
                }
                None => None,
            };
            if let Some(path) = executable_change {
                // The host layer is only queued when the user armed it. A
                // changed binary is still fingerprinted into the baseline (above
                // and here), so a later `/reload --force` or an opt-in flip has
                // the right starting point, but a generic `reload = true` can
                // never queue an `execve` of a replaced `current_exe`.
                if self.settings.host_enabled {
                    changed.entry(ReloadLayer::Host).or_default().push(path);
                }
            }
        } else if let Some(current) = scan.executable() {
            self.baseline_executable = Some(current.clone());
        }

        summary.changed_layers = changed.keys().copied().collect();
        summary.changed_paths = changed.values().map(|paths| paths.len()).sum();
        for (layer, paths) in changed {
            for path in paths {
                self.note_stale(now, layer, path);
            }
        }

        self.baseline_ready = true;
        summary.queued_layers = self.queued_in_order();
        summary.due = self.is_due(now);
        summary
    }

    /// Record an explicit request: `/reload`, `--dev` startup, or an
    /// extension's API 0.3 `session/reload`.
    ///
    /// Resources and extensions are marked stale immediately; the host layer is
    /// deliberately not, because only a real executable change can justify
    /// replacing the process image — and only when `reload_host = true` armed
    /// that layer (or `/reload --force` takes it directly). An explicit request
    /// is not debounced, but it is still admitted only at an idle boundary.
    pub fn request(&mut self, now: Instant, requester: ReloadRequester) {
        if !self.settings.enabled {
            return;
        }
        if !self.requesters.contains(&requester) {
            self.requesters.push(requester);
        }
        self.first_change = None;
        self.deadline = Some(now);
        for layer in [ReloadLayer::Resources, ReloadLayer::Extensions] {
            self.stale.entry(layer).or_default();
        }
    }

    /// Admit at most one pass, only at an idle boundary and only when the
    /// debounce deadline has passed. Busy calls leave the evidence queued.
    /// Only stale layers are selected.
    pub fn begin(&mut self, now: Instant, boundary: ReloadBoundary) -> Option<ReloadPlan> {
        if boundary != ReloadBoundary::Idle || !self.is_due(now) {
            return None;
        }
        Some(self.admit(false, true))
    }

    /// The explicit force path: take a pass over **every** layer now, at any
    /// boundary.
    ///
    /// A forced pass supersedes anything queued or in flight (generation bump)
    /// and names every loss, because it may be taken while a run owns the
    /// session. Records already persisted survive; in-flight work does not.
    pub fn force(&mut self) -> ReloadPlan {
        self.generation = self.generation.wrapping_add(1);
        if !self.requesters.contains(&ReloadRequester::Forced) {
            self.requesters.push(ReloadRequester::Forced);
        }
        self.admit(true, false)
    }

    fn admit(&mut self, forced: bool, boundary_idle: bool) -> ReloadPlan {
        let stale = std::mem::take(&mut self.stale);
        let requesters = std::mem::take(&mut self.requesters);
        let token = ReloadToken {
            generation: self.generation,
            sequence: self.next_sequence,
        };
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.first_change = None;
        self.deadline = None;
        self.in_flight = Some(token);

        let wanted: Vec<ReloadLayer> = if forced {
            ReloadLayer::ORDER.to_vec()
        } else {
            ReloadLayer::ORDER
                .iter()
                .copied()
                .filter(|layer| stale.contains_key(layer))
                .collect()
        };
        let layers = wanted
            .into_iter()
            .map(|layer| {
                let change = stale.get(&layer).cloned().unwrap_or_default();
                PlannedLayer {
                    layer,
                    changed: change.changed,
                    overflow: change.overflow,
                    outcome: None,
                    detached: Vec::new(),
                    reattached: Vec::new(),
                    losses: Vec::new(),
                    notes: Vec::new(),
                }
            })
            .collect();

        ReloadPlan {
            token,
            forced,
            dry_run: false,
            boundary_idle,
            due: true,
            scan: self.scan,
            requesters,
            layers,
        }
    }

    /// Report what a pass would do now, changing nothing: no admission, no
    /// generation bump, no baseline update, no deadline change.
    pub fn dry_run(&self, now: Instant, boundary: ReloadBoundary) -> ReloadReport {
        let mut layers = Vec::with_capacity(ReloadLayer::ORDER.len());
        for layer in ReloadLayer::ORDER {
            match self.stale.get(&layer) {
                Some(change) => {
                    let mut losses = Vec::new();
                    if layer == ReloadLayer::Extensions {
                        losses.push(ReloadLoss::ExtensionHostRequest);
                    }
                    layers.push(LayerReport {
                        layer,
                        outcome: LayerOutcome::WouldReload,
                        changed: change.changed.clone(),
                        changed_overflow: change.overflow,
                        detached: Vec::new(),
                        reattached: Vec::new(),
                        losses,
                        notes: Vec::new(),
                    });
                }
                None => layers.push(LayerReport {
                    layer,
                    outcome: LayerOutcome::Skipped(if self.scan.capped_for(layer) {
                        SkipReason::WatcherCapReached
                    } else {
                        SkipReason::NoChange
                    }),
                    changed: Vec::new(),
                    changed_overflow: 0,
                    detached: Vec::new(),
                    reattached: Vec::new(),
                    losses: Vec::new(),
                    notes: Vec::new(),
                }),
            }
        }
        ReloadReport {
            sequence: self.next_sequence,
            forced: false,
            dry_run: true,
            boundary_idle: boundary == ReloadBoundary::Idle,
            due: self.is_due(now),
            inspected_paths: self.scan.inspected,
            skipped_paths: self.scan.skipped_total(),
            watcher_cap_reached: self.scan.truncated(),
            requesters: self.requesters.clone(),
            layers,
        }
    }

    /// Commit a pass. `None` means the plan was superseded or cancelled and
    /// must be dropped silently, exactly like a stale theme reload completion.
    pub fn finish(&mut self, plan: ReloadPlan) -> Option<ReloadReport> {
        if plan.token.generation != self.generation || self.in_flight != Some(plan.token) {
            return None;
        }
        self.in_flight = None;
        Some(plan.into_report())
    }

    fn note_stale(&mut self, now: Instant, layer: ReloadLayer, path: PathBuf) {
        // The sampler is the requester of its own passes; recording it keeps the
        // report's wording honest about why the pass happened.
        if self.requesters.is_empty() {
            self.requesters.push(ReloadRequester::Watcher);
        }
        let change = self.stale.entry(layer).or_default();
        if change.changed.len() < MAX_REPORTED_PATHS_PER_LAYER {
            change.changed.push(path);
        } else {
            change.overflow = change.overflow.saturating_add(1);
        }
        let first = *self.first_change.get_or_insert(now);
        let burst = now.checked_add(self.settings.debounce).unwrap_or(now);
        let ceiling = first.checked_add(MAX_DEBOUNCE).unwrap_or(now);
        self.deadline = Some(if burst < ceiling { burst } else { ceiling });
    }
}

fn clamp_duration(value: Duration, minimum: Duration, maximum: Duration) -> Duration {
    if value < minimum {
        minimum
    } else if value > maximum {
        maximum
    } else {
        value
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "es"
    }
}

/// Bounded path labels for one report line.
fn changed_labels_of(line: &LayerReport) -> Vec<String> {
    line.changed.iter().map(|path| path_label(path)).collect()
}

fn changed_clause(line: &LayerReport) -> String {
    if line.changed.is_empty() && line.changed_overflow == 0 {
        return String::new();
    }
    let mut labels = changed_labels_of(line);
    if line.changed_overflow > 0 {
        labels.push(format!("+{} more", line.changed_overflow));
    }
    format!(
        " ({} changed: {})",
        line.changed.len() + line.changed_overflow,
        labels.join(", ")
    )
}

fn join_labels(labels: &[String]) -> String {
    labels.join(", ")
}

/// Bounded, control-character-free path label: the last two components.
fn path_label(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let parent = path
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|parent| parent.to_string_lossy().into_owned())
        .filter(|parent| !parent.is_empty());
    match parent {
        Some(parent) => sanitize_label(&format!("{parent}/{name}")),
        None => sanitize_label(&name),
    }
}

/// Bound one label inside a note.
fn sanitize_label(value: &str) -> String {
    bounded_text(value, MAX_LABEL_BYTES)
}

/// Bound a whole note.
///
/// Every string that reaches a transcript goes through this: the supervisor
/// must not be able to inject terminal control sequences, and a caller-supplied
/// label must not be able to grow a notice without bound.
fn sanitize_note(value: &str) -> String {
    bounded_text(value, MAX_NOTE_BYTES)
}

/// Collapse control characters and whitespace, then bound the byte length.
fn bounded_text(value: &str, max_bytes: usize) -> String {
    let mut cleaned = String::with_capacity(value.len().min(max_bytes));
    let mut last_space = false;
    for character in value.chars() {
        let mapped = if character.is_control() {
            ' '
        } else {
            character
        };
        if mapped == ' ' {
            if last_space || cleaned.is_empty() {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        if cleaned.len() + mapped.len_utf8() > max_bytes {
            break;
        }
        cleaned.push(mapped);
    }
    cleaned
}

#[cfg(test)]
mod tests {
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

        fn read_dir(&self, path: &Path) -> Vec<PathBuf> {
            self.dirs.get(path).cloned().unwrap_or_default()
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
    fn baseline(
        supervisor: &mut ReloadSupervisor,
        watcher: &TestWatcher,
        source: &Fake,
        now: Instant,
    ) {
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
    fn force_names_every_loss_and_supersedes_a_queued_pass() {
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
        // A forced pass names every loss on every layer it selects.
        for layer in ReloadLayer::ORDER {
            assert_eq!(losses_of(&report, layer), ReloadLoss::ALL.to_vec());
        }
        assert_eq!(report.layers[0].outcome, LayerOutcome::Reloaded);
        assert!(report.notices()[0].contains("reload (forced)"));
        assert!(summary_mentions(&report, "4 named losses"));
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
    fn the_inspection_budget_stops_every_poll_at_its_bound_and_records_the_rest() {
        let start = base();
        // Fixture: one watched directory with ten files, so eleven candidates
        // exist (the target itself plus each enumerable entry).
        const CAP: usize = 3;
        const FILES: usize = 10;
        const CANDIDATES: usize = FILES + 1;

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
        // Rule 2: nothing is dropped silently — every candidate the layer could
        // enumerate is either inspected or counted as skipped for its layer.
        assert_eq!(scan.inspected() + scan.meta().skipped_total(), CANDIDATES);
        assert_eq!(skipped_for(&scan, ReloadLayer::Resources), CANDIDATES - CAP);
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
        assert_eq!(
            skipped_for(&wide, ReloadLayer::Resources),
            BIG_FILES + 1 - MAX_INSPECTIONS_PER_POLL
        );
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
        assert_eq!(report.skipped_paths, CANDIDATES - CAP);
        assert!(report
            .notices()
            .iter()
            .any(|notice| notice.contains("per-poll cap")));
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
    fn an_extension_reload_names_the_host_request_it_drops_and_a_host_pass_does_not() {
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
        assert!(
            losses_of(&report, ReloadLayer::Extensions).contains(&ReloadLoss::ExtensionHostRequest)
        );
        assert!(report
            .diagnostics()
            .iter()
            .any(|notice| { notice.contains(ReloadLoss::ExtensionHostRequest.label()) }));

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
            "skipped 2",
            "lost work",
        ] {
            assert!(text.contains(detail), "{text}");
        }
        for loss in ReloadLoss::ALL {
            assert!(text.contains(loss.label()), "{text}");
        }
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
}
