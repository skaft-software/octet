#![allow(missing_docs)]

//! Scheduling and state management for an active file-theme watcher.
//!
//! This module deliberately does not own a filesystem watcher. The `notify`
//! adapter belongs to the interactive frontend because it must be created and
//! dropped with the TUI. The adapter sends bounded [`FileChangeEvent`] values
//! to [`theme_change_channel`]. The frontend drains that channel here, then
//! starts and commits reloads only at an [`ReloadBoundary::Idle`] boundary.
//!
//! Loading is also deliberately supplied by the caller. The interactive
//! adapter must call `OctetTheme::reload` for the returned request path; that
//! keeps the existing bounded, no-follow, regular-file validation in one
//! place. Invalid or unsafe loads retain the last-good value; missing or
//! broken sources explicitly replace it with the compiled fallback.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

/// Maximum path size accepted at the watcher/reload boundary.
///
/// Theme contents have their own limit in `theme_schema`; this separate limit
/// prevents notifications from carrying arbitrarily large path values.
pub const MAX_THEME_PATH_BYTES: usize = 4096;

/// Maximum number of file notifications retained by the handoff channel.
pub const MAX_QUEUED_CHANGE_EVENTS: usize = 64;

/// Maximum number of notifications one idle-loop tick drains.
pub const MAX_DRAINED_CHANGE_EVENTS: usize = MAX_QUEUED_CHANGE_EVENTS;

/// Default save-burst debounce interval.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(200);

/// Upper bound on a configured debounce interval.
pub const MAX_DEBOUNCE: Duration = Duration::from_secs(2);

/// Runtime modes that may own a theme reload engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeReloadMode {
    /// The interactive TUI, where an idle replacement boundary exists.
    Interactive,
    /// One-shot printed output.
    Print,
    /// Plain/non-interactive output.
    Plain,
    /// The RPC/host process boundary.
    Rpc,
}

impl ThemeReloadMode {
    fn allows_reload(self) -> bool {
        matches!(self, Self::Interactive)
    }
}

/// The only boundary at which a loaded theme may be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadBoundary {
    /// The prompt is idle and no active run or modal owns the shell.
    Idle,
    /// Input, a model run, or a modal currently owns the shell.
    Busy,
}

/// File operations which can make the active path worth reloading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChangeKind {
    Create,
    Modify,
    Remove,
    Rename,
    /// Access notifications never trigger a reload.
    Access,
    /// Unknown/non-file notifications are ignored conservatively.
    Other,
}

impl FileChangeKind {
    fn is_reload_candidate(self) -> bool {
        matches!(
            self,
            Self::Create | Self::Modify | Self::Remove | Self::Rename
        )
    }
}

/// A normalized unit of work passed from a filesystem callback to the TUI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChangeEvent {
    path: PathBuf,
    kind: FileChangeKind,
}

impl FileChangeEvent {
    /// Construct an event. Use [`Self::is_bounded`] before crossing an
    /// untrusted callback/channel boundary.
    pub fn new(path: impl Into<PathBuf>, kind: FileChangeKind) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }

    /// Construct an event only when its path is within the boundary budget.
    pub fn bounded(path: impl Into<PathBuf>, kind: FileChangeKind) -> Option<Self> {
        let event = Self::new(path, kind);
        event.is_bounded().then_some(event)
    }

    /// The path reported by the watcher.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The operation reported by the watcher adapter.
    pub fn kind(&self) -> FileChangeKind {
        self.kind
    }

    /// Whether this event can be retained by the bounded handoff.
    pub fn is_bounded(&self) -> bool {
        path_len(&self.path) <= MAX_THEME_PATH_BYTES
    }
}

/// Sender half of the bounded watcher handoff.
pub type ThemeChangeSender = SyncSender<FileChangeEvent>;

/// Receiver half of the bounded watcher handoff.
pub type ThemeChangeReceiver = Receiver<FileChangeEvent>;

/// Create the bounded channel used by a `notify` callback.
pub fn theme_change_channel() -> (ThemeChangeSender, ThemeChangeReceiver) {
    mpsc::sync_channel(MAX_QUEUED_CHANGE_EVENTS)
}

/// Try to enqueue one watcher event without allowing a filesystem callback to
/// block the process. `false` means the event was oversized, the queue was
/// full, or the TUI has already dropped its receiver.
pub fn try_send_change(sender: &ThemeChangeSender, event: FileChangeEvent) -> bool {
    if !event.is_bounded() {
        return false;
    }
    sender.try_send(event).is_ok()
}

/// Errors raised when the active path cannot be represented as a file path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePathError {
    /// The path has no final component and therefore cannot identify a theme.
    MissingFileName,
    /// The path exceeds the watcher boundary budget.
    PathTooLong,
}

impl fmt::Display for ThemePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFileName => formatter.write_str("theme path has no file name"),
            Self::PathTooLong => formatter.write_str("theme path exceeds the 4096-byte limit"),
        }
    }
}

impl std::error::Error for ThemePathError {}

/// A non-recursive directory watch specification for the active theme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeWatch {
    directory: PathBuf,
    recursive: bool,
}

impl ThemeWatch {
    /// Directory that must be registered with `notify`.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// This is always false: watching the parent is sufficient and avoids
    /// receiving unrelated descendants.
    pub fn recursive(&self) -> bool {
        self.recursive
    }
}

/// The token returned when a due reload is admitted at an idle boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReloadToken {
    generation: u64,
    sequence: u64,
}

impl ReloadToken {
    /// Monotonic (within a process) diagnostic sequence number.
    pub fn sequence(self) -> u64 {
        self.sequence
    }
}

/// An admitted reload request and the path it must load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadRequest {
    token: ReloadToken,
    path: PathBuf,
}

impl ReloadRequest {
    /// Cancellation/commit token for this request.
    pub fn token(&self) -> ReloadToken {
        self.token
    }

    /// The active path captured at admission time.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Why a candidate theme could not be installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadFailureKind {
    /// The active file or its parent disappeared during an atomic save.
    Missing,
    /// The file exists but could not be read as a usable theme file.
    Broken,
    /// The bounded file was read but failed schema/semantic validation.
    Invalid,
    /// The path was replaced by a symlink, FIFO, or another unsafe file type.
    Unsafe,
    /// Any other loader failure.
    Other,
}

impl ReloadFailureKind {
    /// Missing/broken resources use the compiled fallback. Invalid or unsafe
    /// edits retain the currently rendered last-good theme.
    pub fn uses_compiled_fallback(self) -> bool {
        matches!(self, Self::Missing | Self::Broken)
    }
}

/// Result of validating a loader completion against the current generation.
#[derive(Debug)]
pub enum ReloadDecision<T> {
    /// Install this fully loaded candidate at the idle boundary.
    Applied(T),
    /// The source disappeared or became unavailable; install the supplied
    /// compiled fallback.
    FellBackToCompiledDefault(T),
    /// Keep the current last-good theme and report the failure category.
    RetainedLastGood { failure: ReloadFailureKind },
    /// The engine was cancelled before the worker completed.
    Cancelled,
    /// A completion did not belong to the currently admitted request.
    Stale,
}

impl<T> ReloadDecision<T> {
    /// Whether this completion produced a theme which the caller should apply.
    pub fn should_apply(&self) -> bool {
        matches!(self, Self::Applied(_) | Self::FellBackToCompiledDefault(_))
    }
}

/// Debounced, cancellable active-theme reload state.
///
/// `T` is intentionally generic so the scheduling/cancellation rules can be
/// tested without constructing the renderer. In the TUI it is `OctetTheme`.
/// The caller supplies the compiled fallback because terminal capabilities and
/// background detection belong to the existing theme loader.
#[derive(Debug)]
pub struct ThemeReloadEngine<T> {
    mode: ThemeReloadMode,
    active_path: Option<PathBuf>,
    watch_directory: Option<PathBuf>,
    debounce: Duration,
    pending: bool,
    deadline: Option<Instant>,
    generation: u64,
    next_sequence: u64,
    in_flight: Option<ReloadToken>,
    last_good: T,
    compiled_fallback: T,
}

impl<T> ThemeReloadEngine<T> {
    /// Build an engine. An active path is optional because the compiled default
    /// and all non-interactive modes must not create a watcher.
    pub fn new(
        mode: ThemeReloadMode,
        active_path: Option<PathBuf>,
        last_good: T,
        compiled_fallback: T,
        debounce: Duration,
    ) -> Result<Self, ThemePathError> {
        let active_path = active_path.map(normalize_active_path).transpose()?;
        let watch_directory = active_path.as_deref().map(parent_directory);
        Ok(Self {
            mode,
            active_path,
            watch_directory,
            debounce: clamp_debounce(debounce),
            pending: false,
            deadline: None,
            generation: 0,
            next_sequence: 0,
            in_flight: None,
            last_good,
            compiled_fallback,
        })
    }

    /// Current runtime mode.
    pub fn mode(&self) -> ThemeReloadMode {
        self.mode
    }

    /// Change runtime mode. Leaving interactive mode cancels both queued and
    /// in-flight work; returning to it does not invent a new file event.
    pub fn set_mode(&mut self, mode: ThemeReloadMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if !mode.allows_reload() {
            self.cancel();
        }
    }

    /// Set or clear the active file theme. A path change invalidates all old
    /// requests so a late worker cannot install a theme for another file.
    pub fn set_active_theme(
        &mut self,
        active_path: Option<PathBuf>,
    ) -> Result<bool, ThemePathError> {
        let active_path = active_path.map(normalize_active_path).transpose()?;
        if self.active_path == active_path {
            return Ok(false);
        }
        self.cancel();
        self.watch_directory = active_path.as_deref().map(parent_directory);
        self.active_path = active_path;
        Ok(true)
    }

    /// The active source path, if this engine owns a file theme.
    pub fn active_theme_path(&self) -> Option<&Path> {
        self.active_path.as_deref()
    }

    /// Return the non-recursive parent-directory watch required for atomic
    /// replacement and rename-based editor saves.
    pub fn watch_spec(&self) -> Option<ThemeWatch> {
        if !self.mode.allows_reload() {
            return None;
        }
        Some(ThemeWatch {
            directory: self.watch_directory.clone()?,
            recursive: false,
        })
    }

    /// Replace the retained theme after another existing shell operation has
    /// installed a new baseline.
    pub fn set_last_good(&mut self, theme: T) {
        self.last_good = theme;
    }

    /// Replace the compiled fallback used for missing/broken file recovery.
    pub fn set_compiled_fallback(&mut self, theme: T) {
        self.compiled_fallback = theme;
    }

    /// Inspect the currently retained theme without taking ownership of it.
    pub fn last_good(&self) -> &T {
        &self.last_good
    }

    /// Inspect the configured debounce interval.
    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    /// Whether a relevant event has been coalesced and is waiting for its
    /// debounce deadline.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Whether a worker currently owns an admitted request.
    pub fn is_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Current debounce deadline, if work is pending.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Observe one bounded notification. Unrelated paths, access events,
    /// non-interactive modes, and compiled-default themes are ignored.
    pub fn observe(&mut self, event: &FileChangeEvent, now: Instant) -> bool {
        if !self.mode.allows_reload() || !event.kind.is_reload_candidate() || !event.is_bounded() {
            return false;
        }
        let Some(active_path) = self.active_path.as_deref() else {
            return false;
        };
        if lexical_normalize(event.path()) != active_path {
            return false;
        }
        self.pending = true;
        self.deadline = Some(debounce_deadline(now, self.debounce));
        true
    }

    /// Convenience wrapper for adapters that already split a `notify` event's
    /// path list into one path at a time.
    pub fn observe_path(&mut self, path: &Path, kind: FileChangeKind, now: Instant) -> bool {
        self.observe(&FileChangeEvent::new(path.to_owned(), kind), now)
    }

    /// Drain at most the bounded per-tick budget from a watcher channel.
    pub fn drain_notifications(&mut self, receiver: &ThemeChangeReceiver, now: Instant) -> usize {
        let mut drained = 0;
        while drained < MAX_DRAINED_CHANGE_EVENTS {
            match receiver.try_recv() {
                Ok(event) => {
                    self.observe(&event, now);
                    drained += 1;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        drained
    }

    /// Admit one due request only when the caller has reached an idle prompt
    /// boundary. Busy calls leave the coalesced event pending.
    pub fn begin_if_ready(
        &mut self,
        now: Instant,
        boundary: ReloadBoundary,
    ) -> Option<ReloadRequest> {
        if boundary != ReloadBoundary::Idle
            || !self.mode.allows_reload()
            || self.in_flight.is_some()
            || !self.pending
            || !self.deadline.is_some_and(|deadline| now >= deadline)
        {
            return None;
        }
        let path = self.active_path.clone()?;
        self.pending = false;
        self.deadline = None;
        let token = ReloadToken {
            generation: self.generation,
            sequence: self.next_sequence,
        };
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.in_flight = Some(token);
        Some(ReloadRequest { token, path })
    }

    /// Whether a worker completion still belongs to the admitted request.
    pub fn is_current(&self, token: ReloadToken) -> bool {
        self.generation == token.generation && self.in_flight == Some(token)
    }

    /// Commit a loader result. The loader should be `OctetTheme::reload`, and
    /// must classify its error without bypassing the existing bounded secure
    /// read. Stale/cancelled results are discarded before touching state.
    pub fn finish(
        &mut self,
        token: ReloadToken,
        result: Result<T, ReloadFailureKind>,
    ) -> ReloadDecision<T>
    where
        T: Clone,
    {
        if token.generation != self.generation {
            return ReloadDecision::Cancelled;
        }
        if self.in_flight != Some(token) {
            return ReloadDecision::Stale;
        }
        self.in_flight = None;
        match result {
            Ok(theme) => {
                self.last_good = theme.clone();
                ReloadDecision::Applied(theme)
            }
            Err(failure) if failure.uses_compiled_fallback() => {
                let fallback = self.compiled_fallback.clone();
                self.last_good = fallback.clone();
                ReloadDecision::FellBackToCompiledDefault(fallback)
            }
            Err(failure) => ReloadDecision::RetainedLastGood { failure },
        }
    }

    /// Cancel queued and in-flight work. A generation bump makes every late
    /// completion harmless, including completion during shutdown or a source
    /// change.
    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = false;
        self.deadline = None;
        self.in_flight = None;
    }
}

fn clamp_debounce(debounce: Duration) -> Duration {
    if debounce > MAX_DEBOUNCE {
        MAX_DEBOUNCE
    } else {
        debounce
    }
}

fn debounce_deadline(now: Instant, debounce: Duration) -> Instant {
    now.checked_add(debounce).unwrap_or(now)
}

fn normalize_active_path(path: PathBuf) -> Result<PathBuf, ThemePathError> {
    if path.file_name().is_none() {
        return Err(ThemePathError::MissingFileName);
    }
    if path_len(&path) > MAX_THEME_PATH_BYTES {
        return Err(ThemePathError::PathTooLong);
    }
    let path = lexical_normalize(&path);
    if path.file_name().is_none() {
        return Err(ThemePathError::MissingFileName);
    }
    if path_len(&path) > MAX_THEME_PATH_BYTES {
        return Err(ThemePathError::PathTooLong);
    }
    Ok(path)
}

fn parent_directory(path: &Path) -> PathBuf {
    path.parent()
        .map(lexical_normalize)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Lexically normalize only. In particular, this never calls `canonicalize`,
/// follows a symlink, or touches the filesystem; removed paths remain
/// comparable with notifications from an atomic replacement.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut normal_components = 0usize;
    let mut rooted = false;

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normal_components > 0 {
                    let _ = normalized.pop();
                    normal_components -= 1;
                } else if !rooted {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => {
                normalized.push(component.as_os_str());
                rooted = true;
            }
            Component::Normal(part) => {
                normalized.push(part);
                normal_components += 1;
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

#[cfg(unix)]
fn path_len(path: &Path) -> usize {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().len()
}

#[cfg(not(unix))]
fn path_len(path: &Path) -> usize {
    path.to_string_lossy().len()
}
