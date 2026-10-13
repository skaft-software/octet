//! What one bounded sampling pass observes, and the only I/O it performs.
//!
//! The reload supervisor is a pure state machine over this module's data, so
//! every change-detection rule is unit-testable without touching a filesystem.
//! Keeping the whole observation vocabulary here also keeps the "we never read
//! file contents" guarantee in one place: the only route into the filesystem is
//! the [`MetadataSource`] trait, whose one real implementation
//! ([`SystemMetadata`]) calls `symlink_metadata`, `read_dir`, and a fresh
//! `std::env::current_exe()` per poll.
//!
//! [`WatchTarget`] is the other half of the boundary: it is what the caller
//! declares watchable, grouped per layer by the watch set in the parent module.
//! Budget accounting ([`ScanMeta`]) is reported as data too, so a capped pass
//! is visible to the state machine instead of being silently partial.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::ReloadLayer;

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
    pub(super) fn differs_from(&self, previous: &Self) -> bool {
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
    pub(super) path: PathBuf,
    pub(super) fingerprint: FileFingerprint,
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
///   boolean is the record; it refuses all change/disappearance inferences and
///   makes a layer report [`super::SkipReason::WatcherCapReached`].
/// * [`Self::skipped`] — per layer, a count: candidates the scanner had already
///   read (a target, or an entry of a directory it did read) but could not
///   inspect, including an overflow witness if it names a path. This is a
///   **lower bound**, not the total omitted paths: enumeration itself stops at
///   the budget. A capped layer can have zero known skipped paths; unread
///   contents are unknown, not empty.
///
/// The depth bound ([`super::MAX_SCAN_DEPTH`]) is *not* a cap: looking no deeper than a
/// fixed level is a rule about which files a layer consists of, so it never sets
/// either field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanMeta {
    /// Metadata inspections performed.
    pub inspected: usize,
    /// Lower bound on paths skipped by the budget, per layer; unread totals are unknown.
    pub skipped: [usize; 3],
    /// Whether the budget stopped this layer's enumeration, per layer.
    pub capped: [bool; 3],
}

impl ScanMeta {
    /// Total known skipped paths (a lower bound, not an exact omitted total).
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

    /// Lazy immediate entries of `path`, in filesystem order. Do not collect,
    /// sort, or filter errors here: the scanner bounds iteration before sorting
    /// and counts failed entries against the enumeration budget too. A missing
    /// or unreadable directory yields no entries.
    fn read_dir(&self, path: &Path) -> Box<dyn Iterator<Item = std::io::Result<PathBuf>> + '_>;

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

    fn read_dir(&self, path: &Path) -> Box<dyn Iterator<Item = std::io::Result<PathBuf>> + '_> {
        match std::fs::read_dir(path) {
            Ok(entries) => Box::new(entries.map(|entry| entry.map(|entry| entry.path()))),
            Err(_) => Box::new(std::iter::empty()),
        }
    }

    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
}

/// One watched path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchTarget {
    pub(super) layer: ReloadLayer,
    pub(super) path: PathBuf,
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
