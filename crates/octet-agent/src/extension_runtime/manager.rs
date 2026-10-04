//! The durable extension process fleet and its per-session bindings.
//!
//! [`ExtensionRuntimeManager`] owns every child process for one canonical
//! workspace plus explicit trust domain. It keeps the fleet's resource charge
//! accurate across start, reload, crash-restart, catalog replacement, and
//! shutdown, and it coalesces concurrent activations of the same
//! (domain, extension, content digest, scope) key onto one process.
//!
//! [`ExtensionSessionBinding`] is the per-session attachment. It is the only
//! way to launch a runtime, and it owns the reservation that makes a launch
//! either commit or fail closed — including when the binding is dropped
//! mid-startup.
//!
//! This is separate from [`super::catalog`] and [`super::governance`] because
//! it is the only part of the module with lifecycle, task, and cancellation
//! semantics. Catalog identity and the limits it is charged against are owned
//! elsewhere and reached through this module's state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock, Weak};
use std::time::Instant;

use futures_util::future::join_all;
use tokio::runtime::Handle;
use tokio::sync::{Mutex, Notify, Semaphore};

use super::catalog::{ExtensionRuntimeCatalog, ExtensionRuntimeCatalogEntry};
use super::governance::{
    ExtensionManagedRuntimeState, ExtensionResourceExhausted, ExtensionRuntimeActivation,
    ExtensionRuntimeActivationOutcome, ExtensionRuntimeBudget, ExtensionRuntimeFailure,
    ExtensionRuntimeLease, ExtensionRuntimeManagerError, ExtensionRuntimeProvenance,
    ExtensionRuntimeResource, ExtensionRuntimeStatus, ExtensionRuntimeUsage,
};
use super::{
    lock, read, sha256_hex, write, CanonicalWorkspace, ExtensionRuntimeDomain,
    ESTIMATED_PROCESS_FDS, SUPERVISOR_POLL,
};
use crate::extension_process::{
    DiscoveredExtension, ExtensionHealthState, ExtensionLifecycleProfile, ExtensionProcess,
    ExtensionReloadReport, ExtensionRuntimeConfig, ExtensionRuntimeError as ProcessRuntimeError,
    ExtensionRuntimeSharing, ExtensionTrust, EXTENSION_API_VERSION_0_1,
};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RuntimeScope {
    Shared,
    Binding(u64),
    OneShot(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RuntimeKey {
    domain_digest: String,
    extension: String,
    content_digest: String,
    scope: RuntimeScope,
}

struct ManagedRuntime {
    descriptor: DiscoveredExtension,
    provenance: ExtensionRuntimeProvenance,
    process: ExtensionProcess,
    usage: ExtensionRuntimeUsage,
    estimated_usage: ExtensionRuntimeUsage,
    bindings: BTreeSet<u64>,
    lifecycle: ExtensionLifecycleProfile,
    sharing: ExtensionRuntimeSharing,
    state: ExtensionManagedRuntimeState,
    reloads: VecDeque<Instant>,
    restarts: VecDeque<Instant>,
    restart_attempt: u32,
    next_restart: Option<Instant>,
    gate: Arc<Mutex<()>>,
}

struct RecentStatus {
    provenance: ExtensionRuntimeProvenance,
    state: ExtensionManagedRuntimeState,
    resource_exhausted: Option<ExtensionResourceExhausted>,
    failure: Option<ExtensionRuntimeFailure>,
}

/// Inserts one recent status under its manifest-selected extension name.
///
/// Recent status is keyed by name alone, so a status never overwrites a
/// different content identity that happens to share a display name. All eight
/// writers have to derive that key the same way, so they share this one.
fn insert_recent(state: &mut ManagerState, status: RecentStatus) {
    let extension = status.provenance.extension.clone();
    state.recent.insert(extension, status);
}

/// Records a rejected reservation as the entry's most recent public status.
///
/// A reservation can be refused by any of the aggregate limits, but only the
/// typed outcome explains which one, so a non-exhaustion error records nothing.
fn record_reservation_exhaustion(
    state: &mut ManagerState,
    provenance: &ExtensionRuntimeProvenance,
    error: &ExtensionRuntimeManagerError,
) {
    if let ExtensionRuntimeManagerError::ResourceExhausted(exhausted) = error {
        insert_recent(
            state,
            RecentStatus {
                provenance: provenance.clone(),
                state: ExtensionManagedRuntimeState::ResourceExhausted,
                resource_exhausted: Some(exhausted.clone()),
                failure: None,
            },
        );
    }
}

/// A launch reservation that is visible while a process is being initialized.
///
/// Keeping provenance and its charge here makes startup wait and budget state
/// inspectable without exposing the child command, workspace path, or stderr.
struct StartingRuntime {
    notify: Arc<Notify>,
    reservation: Arc<AtomicBool>,
    provenance: ExtensionRuntimeProvenance,
    usage: ExtensionRuntimeUsage,
}

#[derive(Default)]
struct ManagerState {
    active: BTreeMap<RuntimeKey, ManagedRuntime>,
    starting: BTreeMap<RuntimeKey, StartingRuntime>,
    usage: ExtensionRuntimeUsage,
    recent: BTreeMap<String, RecentStatus>,
}

struct ManagerInner {
    domain: ExtensionRuntimeDomain,
    budget: ExtensionRuntimeBudget,
    catalog: StdRwLock<ExtensionRuntimeCatalog>,
    state: StdMutex<ManagerState>,
    bulk_storage: StdMutex<Option<crate::BulkStorage>>,
    startup_slots: Arc<Semaphore>,
    shutdown: AtomicBool,
    monitor_started: AtomicBool,
    next_binding: AtomicU64,
    next_one_shot: AtomicU64,
}

/// One durable owner of an ordinary-host or explicit Serve-partition process fleet.
#[derive(Clone)]
pub struct ExtensionRuntimeManager {
    inner: Arc<ManagerInner>,
}

impl ExtensionRuntimeManager {
    /// Creates a manager with an empty static catalog and default governance.
    pub fn new(domain: ExtensionRuntimeDomain) -> Self {
        Self::with_budget(domain, ExtensionRuntimeBudget::default())
            .expect("default extension runtime budget is valid")
    }

    /// Creates a manager with explicit aggregate governance limits.
    pub fn with_budget(
        domain: ExtensionRuntimeDomain,
        budget: ExtensionRuntimeBudget,
    ) -> Result<Self, ExtensionRuntimeManagerError> {
        budget.validate()?;
        Ok(Self {
            inner: Arc::new(ManagerInner {
                domain,
                startup_slots: Arc::new(Semaphore::new(budget.max_concurrent_startups)),
                budget,
                catalog: StdRwLock::new(ExtensionRuntimeCatalog::default()),
                state: StdMutex::new(ManagerState::default()),
                bulk_storage: StdMutex::new(None),
                shutdown: AtomicBool::new(false),
                monitor_started: AtomicBool::new(false),
                next_binding: AtomicU64::new(1),
                next_one_shot: AtomicU64::new(1),
            }),
        })
    }

    /// Returns the shared, lazily created bulk store for this trust domain.
    ///
    /// The product explicitly offers it through each runtime configuration.
    /// Keeping it on the fleet preserves retained results across producer and
    /// foreground-binding replacement without granting another session access.
    pub fn bulk_storage(&self) -> Result<crate::BulkStorage, crate::BulkError> {
        let mut storage = lock(&self.inner.bulk_storage);
        if let Some(storage) = storage.as_ref() {
            return Ok(storage.clone());
        }
        let created = crate::BulkStorage::new()?;
        *storage = Some(created.clone());
        Ok(created)
    }

    /// Returns the immutable canonical workspace/trust domain.
    pub fn domain(&self) -> &ExtensionRuntimeDomain {
        &self.inner.domain
    }

    /// Replaces the static catalog without launching any process.
    ///
    /// Runtimes whose selected source is replaced or removed are stopped before
    /// the method returns. This preserves one durable owner and prevents an old
    /// process from being silently attached under a new content identity.
    pub async fn replace_catalog(&self, catalog: ExtensionRuntimeCatalog) {
        if self.inner.shutdown.load(Ordering::Acquire) {
            return;
        }
        let (changed, canceled_starts) = {
            let mut current = write(&self.inner.catalog);
            let mut changed = Vec::new();
            let mut canceled_starts = Vec::new();
            let mut state = lock(&self.inner.state);
            for (key, runtime) in &state.active {
                let replacement = match catalog.get(&runtime.descriptor.manifest.name) {
                    None => Some(ExtensionManagedRuntimeState::Stopped),
                    Some(entry)
                        if !Self::entry_is_eligible(entry)
                            || entry.lifecycle() != runtime.lifecycle
                            || entry.sharing() != runtime.sharing =>
                    {
                        Some(ExtensionManagedRuntimeState::Inactive)
                    }
                    Some(entry)
                        if entry.content_digest.as_str() != key.content_digest
                            || (entry.sharing() == ExtensionRuntimeSharing::Workspace
                                && !entry.source_verified) =>
                    {
                        Some(ExtensionManagedRuntimeState::StaleSource)
                    }
                    Some(_) => None,
                };
                if let Some(replacement) = replacement {
                    changed.push((key.clone(), replacement));
                }
            }
            let starting_keys = state.starting.keys().cloned().collect::<Vec<_>>();
            for key in starting_keys {
                let Some(starting) = state.starting.get(&key) else {
                    continue;
                };
                let replacement = match catalog.get(&starting.provenance.extension) {
                    None => Some(ExtensionManagedRuntimeState::Stopped),
                    Some(entry)
                        if !Self::entry_is_eligible(entry)
                            || entry.lifecycle() != starting.provenance.lifecycle
                            || !matches!(
                                (&key.scope, entry.sharing()),
                                (RuntimeScope::Shared, ExtensionRuntimeSharing::Workspace)
                                    | (
                                        RuntimeScope::Binding(_) | RuntimeScope::OneShot(_),
                                        ExtensionRuntimeSharing::Isolated,
                                    )
                            ) =>
                    {
                        Some(ExtensionManagedRuntimeState::Inactive)
                    }
                    Some(entry)
                        if entry.content_digest.as_str() != key.content_digest
                            || (entry.sharing() == ExtensionRuntimeSharing::Workspace
                                && !entry.source_verified) =>
                    {
                        Some(ExtensionManagedRuntimeState::StaleSource)
                    }
                    Some(_) => None,
                };
                if let Some(replacement) = replacement {
                    if let Some(starting) = state.starting.remove(&key) {
                        insert_recent(
                            &mut state,
                            RecentStatus {
                                provenance: starting.provenance,
                                state: replacement,
                                resource_exhausted: None,
                                // A canceled startup names the reason it was
                                // canceled, so a reload-rejected entry reports
                                // ineligibility here rather than silently
                                // looking like an ordinary stop.
                                failure: match replacement {
                                    ExtensionManagedRuntimeState::Inactive => {
                                        Some(ExtensionRuntimeFailure::NotEligible)
                                    }
                                    ExtensionManagedRuntimeState::StaleSource => {
                                        Some(ExtensionRuntimeFailure::StaleSource)
                                    }
                                    _ => None,
                                },
                            },
                        );
                        canceled_starts.push(starting.notify);
                    }
                }
            }
            *current = catalog;
            (changed, canceled_starts)
        };
        for notify in canceled_starts {
            notify.notify_waiters();
        }
        for (key, replacement) in changed {
            self.stop_key(&key, Some(replacement)).await;
        }
    }

    /// Returns a static catalog snapshot. Reading this has no activation side effects.
    pub fn catalog(&self) -> ExtensionRuntimeCatalog {
        read(&self.inner.catalog).clone()
    }

    /// Creates a session binding. The opaque owner is only hashed for runtime
    /// keys and never emitted in provenance/status.
    pub fn bind_session(
        &self,
        session_owner: impl AsRef<str>,
    ) -> Result<ExtensionSessionBinding, ExtensionRuntimeManagerError> {
        if self.inner.shutdown.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeManagerError::ManagerClosed);
        }
        let session_owner = session_owner.as_ref();
        if session_owner.is_empty() || session_owner.len() > 512 {
            return Err(ExtensionRuntimeManagerError::BindingClosed);
        }
        let id = self.inner.next_binding.fetch_add(1, Ordering::Relaxed);
        Ok(ExtensionSessionBinding {
            manager: self.clone(),
            id,
            session_digest: sha256_hex(b"octet-extension-session-binding-v1\0", session_owner),
            resource_owner: session_owner.to_owned(),
            active: Arc::new(StdMutex::new(BTreeSet::new())),
            released: Arc::new(AtomicBool::new(false)),
            release_notify: Arc::new(Notify::new()),
            // Activation fans out cloned handles. Only the final binding handle
            // may perform Drop-based cleanup; a completed activation must not
            // release the session attachment that owns the returned process.
            owners: Arc::new(()),
        })
    }

    /// Returns static and active runtime status without starting eligible entries.
    pub fn statuses(&self) -> Vec<ExtensionRuntimeStatus> {
        self.reconcile_dead_usage();
        let catalog = self.catalog();
        let state = lock(&self.inner.state);
        let mut statuses = BTreeMap::<(String, String), ExtensionRuntimeStatus>::new();
        for entry in catalog.entries() {
            let provenance = self.provenance(entry);
            let state_value = if entry.descriptor.activation.enabled
                && entry.descriptor.activation.trust == ExtensionTrust::Trusted
            {
                ExtensionManagedRuntimeState::Eligible
            } else {
                ExtensionManagedRuntimeState::Inactive
            };
            statuses.insert(
                (
                    provenance.extension.clone(),
                    provenance.content_digest.clone(),
                ),
                ExtensionRuntimeStatus {
                    provenance,
                    state: state_value,
                    bindings: 0,
                    usage: ExtensionRuntimeUsage::default(),
                    resource_exhausted: None,
                    failure: None,
                },
            );
        }
        let is_selected_identity = |provenance: &ExtensionRuntimeProvenance| {
            catalog.get(&provenance.extension).is_some_and(|entry| {
                entry.content_digest.as_str() == provenance.content_digest
                    && entry.lifecycle() == provenance.lifecycle
            })
        };
        for starting in state.starting.values() {
            // A replaced entry can still be winding down its startup future.
            // It is never presented as the newly selected catalog entry.
            if !is_selected_identity(&starting.provenance) {
                continue;
            }
            statuses.insert(
                (
                    starting.provenance.extension.clone(),
                    starting.provenance.content_digest.clone(),
                ),
                ExtensionRuntimeStatus {
                    provenance: starting.provenance.clone(),
                    state: ExtensionManagedRuntimeState::Starting,
                    bindings: 0,
                    usage: starting.usage,
                    resource_exhausted: None,
                    failure: None,
                },
            );
        }
        for runtime in state.active.values() {
            // `replace_catalog` removes changed runtimes before waiting for
            // child shutdown, but retain this filter for observers racing the
            // catalog transition.
            if !is_selected_identity(&runtime.provenance) {
                continue;
            }
            statuses.insert(
                (
                    runtime.provenance.extension.clone(),
                    runtime.provenance.content_digest.clone(),
                ),
                ExtensionRuntimeStatus {
                    provenance: runtime.provenance.clone(),
                    state: Self::observed_runtime_state(runtime),
                    bindings: runtime.bindings.len(),
                    usage: runtime.usage,
                    resource_exhausted: None,
                    failure: None,
                },
            );
        }
        for recent in state.recent.values() {
            // A status belongs only to the catalog identity that produced it.
            // Do not let a prior source's stale/exhausted status overwrite a
            // newly selected manifest with the same display name.
            if !is_selected_identity(&recent.provenance) {
                continue;
            }
            let key = (
                recent.provenance.extension.clone(),
                recent.provenance.content_digest.clone(),
            );
            if let Some(status) = statuses.get_mut(&key) {
                status.state = recent.state;
                status.resource_exhausted = recent.resource_exhausted.clone();
                status.failure = recent.failure;
            }
        }
        statuses.into_values().collect()
    }

    /// Returns aggregate resource usage currently charged to the fleet.
    pub fn usage(&self) -> ExtensionRuntimeUsage {
        self.reconcile_dead_usage();
        lock(&self.inner.state).usage
    }

    /// Reloads every currently active runtime with this manifest-selected name.
    ///
    /// Candidate-first reload uses a transient second reservation while the old
    /// process is still alive, so it fails visibly rather than oversubscribing
    /// file descriptors, process count, or protocol buffering.
    pub async fn reload(
        &self,
        extension: &str,
    ) -> Vec<Result<ExtensionReloadReport, ExtensionRuntimeManagerError>> {
        let keys = {
            let state = lock(&self.inner.state);
            state
                .active
                .iter()
                .filter(|(_, runtime)| runtime.descriptor.manifest.name == extension)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>()
        };
        join_all(keys.into_iter().map(|key| self.reload_key(key, false))).await
    }

    /// Stops every fleet process after cancelling/draining its protocol work.
    pub async fn shutdown(&self) {
        if self.inner.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        // Terminal shutdown closes pending permit acquisition immediately;
        // start reservations then release their charges and wake coalesced
        // activation callers instead of waiting for the startup timeout.
        self.inner.startup_slots.close();
        let (runtimes, starters) = {
            let mut state = lock(&self.inner.state);
            state.usage = ExtensionRuntimeUsage::default();
            let starters = std::mem::take(&mut state.starting)
                .into_values()
                .map(|starting| {
                    starting.reservation.store(false, Ordering::Release);
                    starting.notify
                })
                .collect::<Vec<_>>();
            let runtimes = std::mem::take(&mut state.active)
                .into_values()
                .map(|runtime| (runtime.process, runtime.gate))
                .collect::<Vec<_>>();
            (runtimes, starters)
        };
        for starter in starters {
            starter.notify_waiters();
        }
        let _ = join_all(runtimes.into_iter().map(|(process, gate)| async move {
            let _guard = gate.lock().await;
            let _ = process.shutdown().await;
        }))
        .await;
    }

    fn entry_is_eligible(entry: &ExtensionRuntimeCatalogEntry) -> bool {
        entry.descriptor.activation.enabled
            && entry.descriptor.activation.trust == ExtensionTrust::Trusted
    }

    fn observed_runtime_state(runtime: &ManagedRuntime) -> ExtensionManagedRuntimeState {
        if runtime.state != ExtensionManagedRuntimeState::Ready {
            return runtime.state;
        }
        if !runtime.process.is_running() {
            return ExtensionManagedRuntimeState::Backoff;
        }
        match runtime.process.health_snapshot().state {
            ExtensionHealthState::Ready | ExtensionHealthState::Degraded => {
                ExtensionManagedRuntimeState::Ready
            }
            ExtensionHealthState::Starting
            | ExtensionHealthState::Initializing
            | ExtensionHealthState::Draining => ExtensionManagedRuntimeState::Starting,
            ExtensionHealthState::Backoff => ExtensionManagedRuntimeState::Backoff,
            ExtensionHealthState::Parked => ExtensionManagedRuntimeState::Parked,
            ExtensionHealthState::Stopped | ExtensionHealthState::Crashed => {
                ExtensionManagedRuntimeState::Backoff
            }
        }
    }

    fn runtime_is_attachable(runtime: &ManagedRuntime) -> bool {
        runtime.process.is_running()
            && matches!(
                runtime.state,
                ExtensionManagedRuntimeState::Ready
                    | ExtensionManagedRuntimeState::ResourceExhausted
            )
            && matches!(
                runtime.process.health_snapshot().state,
                ExtensionHealthState::Ready | ExtensionHealthState::Degraded
            )
    }

    fn reconcile_dead_usage(&self) {
        let mut state = lock(&self.inner.state);
        let released = state
            .active
            .values_mut()
            .filter_map(|runtime| {
                (!runtime.process.is_running()).then(|| std::mem::take(&mut runtime.usage))
            })
            .collect::<Vec<_>>();
        for usage in released {
            Self::release_usage(&mut state, usage);
        }
    }

    fn release_usage(state: &mut ManagerState, usage: ExtensionRuntimeUsage) {
        state.usage.processes = state.usage.processes.saturating_sub(usage.processes);
        state.usage.file_descriptors = state
            .usage
            .file_descriptors
            .saturating_sub(usage.file_descriptors);
        state.usage.buffered_bytes = state
            .usage
            .buffered_bytes
            .saturating_sub(usage.buffered_bytes);
    }

    fn validate_catalog_identity(
        key: &RuntimeKey,
        expected: &ExtensionRuntimeCatalogEntry,
        selected: Option<&ExtensionRuntimeCatalogEntry>,
    ) -> Result<(), ExtensionRuntimeManagerError> {
        let entry = selected.ok_or(ExtensionRuntimeManagerError::StaleSource)?;
        if entry.content_digest != expected.content_digest
            || entry.content_digest.as_str() != key.content_digest
            || entry.lifecycle() != expected.lifecycle()
            || entry.sharing() != expected.sharing()
        {
            return Err(ExtensionRuntimeManagerError::StaleSource);
        }
        if !Self::entry_is_eligible(entry) {
            return Err(ExtensionRuntimeManagerError::NotEligible);
        }
        Ok(())
    }

    /// Rechecks the selected catalog and source immediately before a freshly
    /// started child becomes attachable. This closes the gap between the first
    /// preflight fingerprint and a slow launch or concurrent catalog reload.
    fn validate_current_entry(
        &self,
        key: &RuntimeKey,
        expected: &ExtensionRuntimeCatalogEntry,
    ) -> Result<(), ExtensionRuntimeManagerError> {
        let entry = read(&self.inner.catalog)
            .get(&expected.descriptor.manifest.name)
            .cloned();
        Self::validate_catalog_identity(key, expected, entry.as_ref())?;
        let entry = entry.expect("validated catalog entry is present");
        let (current_digest, source_verified) = entry
            .current_digest()
            .map_err(|_| ExtensionRuntimeManagerError::StaleSource)?;
        if current_digest != entry.content_digest
            || (entry.sharing() == ExtensionRuntimeSharing::Workspace && !source_verified)
        {
            return Err(ExtensionRuntimeManagerError::StaleSource);
        }
        Ok(())
    }

    fn provenance(&self, entry: &ExtensionRuntimeCatalogEntry) -> ExtensionRuntimeProvenance {
        ExtensionRuntimeProvenance {
            extension: entry.descriptor.manifest.name.clone(),
            content_digest: entry.content_digest.as_str().to_owned(),
            lifecycle: entry.lifecycle(),
        }
    }

    fn runtime_key(
        &self,
        entry: &ExtensionRuntimeCatalogEntry,
        binding: &ExtensionSessionBinding,
    ) -> RuntimeKey {
        let scope = match entry.sharing() {
            ExtensionRuntimeSharing::Workspace => RuntimeScope::Shared,
            ExtensionRuntimeSharing::Isolated
                if entry.lifecycle() == ExtensionLifecycleProfile::OneShot =>
            {
                RuntimeScope::OneShot(self.inner.next_one_shot.fetch_add(1, Ordering::Relaxed))
            }
            ExtensionRuntimeSharing::Isolated => RuntimeScope::Binding(binding.id),
        };
        RuntimeKey {
            domain_digest: self.inner.domain.fingerprint().to_owned(),
            extension: entry.descriptor.manifest.name.clone(),
            content_digest: entry.content_digest.as_str().to_owned(),
            scope,
        }
    }

    fn estimated_usage(config: &ExtensionRuntimeConfig) -> ExtensionRuntimeUsage {
        let queue_items = config
            .writer_queue_capacity
            .saturating_add(config.max_pending_requests)
            .saturating_add(2);
        ExtensionRuntimeUsage {
            processes: 1,
            file_descriptors: ESTIMATED_PROCESS_FDS,
            buffered_bytes: config.max_message_bytes.saturating_mul(queue_items),
        }
    }

    fn reserve(
        &self,
        state: &mut ManagerState,
        requested: ExtensionRuntimeUsage,
        provenance: &ExtensionRuntimeProvenance,
    ) -> Result<(), ExtensionRuntimeManagerError> {
        let check =
            |resource: ExtensionRuntimeResource, limit: usize, in_use: usize, request: usize| {
                if in_use.saturating_add(request) > limit {
                    Err(ExtensionRuntimeManagerError::ResourceExhausted(
                        ExtensionResourceExhausted {
                            resource,
                            limit: limit as u64,
                            requested: request as u64,
                            in_use: in_use as u64,
                            provenance: provenance.clone(),
                        },
                    ))
                } else {
                    Ok(())
                }
            };
        check(
            ExtensionRuntimeResource::Processes,
            self.inner.budget.max_processes,
            state.usage.processes,
            requested.processes,
        )?;
        check(
            ExtensionRuntimeResource::FileDescriptors,
            self.inner.budget.max_file_descriptors,
            state.usage.file_descriptors,
            requested.file_descriptors,
        )?;
        check(
            ExtensionRuntimeResource::BufferedBytes,
            self.inner.budget.max_buffered_bytes,
            state.usage.buffered_bytes,
            requested.buffered_bytes,
        )?;
        state.usage.processes = state.usage.processes.saturating_add(requested.processes);
        state.usage.file_descriptors = state
            .usage
            .file_descriptors
            .saturating_add(requested.file_descriptors);
        state.usage.buffered_bytes = state
            .usage
            .buffered_bytes
            .saturating_add(requested.buffered_bytes);
        Ok(())
    }

    fn record_recent(
        &self,
        provenance: ExtensionRuntimeProvenance,
        state_value: ExtensionManagedRuntimeState,
        resource_exhausted: Option<ExtensionResourceExhausted>,
        failure: Option<ExtensionRuntimeFailure>,
    ) {
        insert_recent(
            &mut lock(&self.inner.state),
            RecentStatus {
                provenance,
                state: state_value,
                resource_exhausted,
                failure,
            },
        );
    }

    /// Returns the typed outcome for a launch or handshake that outlived its
    /// configured startup budget.
    ///
    /// The whole budget is both the limit and the request, and nothing else is
    /// in use for wall-clock: a startup timeout is not cumulative, so this
    /// reads the same for every runtime that hits it.
    fn startup_timeout_exhaustion(
        budget: &ExtensionRuntimeBudget,
        provenance: &ExtensionRuntimeProvenance,
    ) -> ExtensionResourceExhausted {
        let budget_ms = budget.startup_timeout.as_millis() as u64;
        ExtensionResourceExhausted {
            resource: ExtensionRuntimeResource::StartupTime,
            limit: budget_ms,
            requested: budget_ms,
            in_use: 0,
            provenance: provenance.clone(),
        }
    }

    fn record_entry_validation_failure(
        &self,
        provenance: &ExtensionRuntimeProvenance,
        error: &ExtensionRuntimeManagerError,
    ) {
        match error {
            ExtensionRuntimeManagerError::NotEligible => self.record_recent(
                provenance.clone(),
                ExtensionManagedRuntimeState::Inactive,
                None,
                Some(ExtensionRuntimeFailure::NotEligible),
            ),
            ExtensionRuntimeManagerError::StaleSource => self.record_recent(
                provenance.clone(),
                ExtensionManagedRuntimeState::StaleSource,
                None,
                Some(ExtensionRuntimeFailure::StaleSource),
            ),
            _ => {}
        }
    }

    fn ensure_monitor(&self) {
        if self.inner.monitor_started.swap(true, Ordering::AcqRel)
            || self.inner.shutdown.load(Ordering::Acquire)
        {
            return;
        }
        let Ok(handle) = Handle::try_current() else {
            self.inner.monitor_started.store(false, Ordering::Release);
            return;
        };
        let manager = self.clone();
        handle.spawn(async move { manager.monitor().await });
    }

    async fn monitor(&self) {
        loop {
            if self.inner.shutdown.load(Ordering::Acquire) {
                return;
            }
            let now = Instant::now();
            let keys = {
                let state = lock(&self.inner.state);
                state
                    .active
                    .iter()
                    .filter(|(_, runtime)| {
                        runtime.lifecycle != ExtensionLifecycleProfile::OneShot
                            && !runtime.process.is_running()
                            && matches!(
                                runtime.state,
                                ExtensionManagedRuntimeState::Ready
                                    | ExtensionManagedRuntimeState::Backoff
                            )
                            && runtime.next_restart.is_none_or(|next| next <= now)
                    })
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>()
            };
            for key in keys {
                let _ = self.reload_key(key, true).await;
            }
            tokio::time::sleep(SUPERVISOR_POLL).await;
        }
    }

    async fn stop_key(&self, key: &RuntimeKey, replacement: Option<ExtensionManagedRuntimeState>) {
        let gate = lock(&self.inner.state)
            .active
            .get(key)
            .map(|runtime| Arc::clone(&runtime.gate));
        if let Some(gate) = gate {
            let _guard = gate.lock().await;
            self.stop_key_after_gate(key, replacement, &gate).await;
        }
    }

    /// Removes and shuts down a runtime while its lifecycle gate is held.
    /// Reload uses this form for validation failures after it has already
    /// acquired the same gate.
    async fn stop_key_after_gate(
        &self,
        key: &RuntimeKey,
        replacement: Option<ExtensionManagedRuntimeState>,
        expected_gate: &Arc<Mutex<()>>,
    ) {
        let runtime = {
            let mut state = lock(&self.inner.state);
            let should_remove = state
                .active
                .get(key)
                .is_some_and(|runtime| Arc::ptr_eq(&runtime.gate, expected_gate));
            let runtime = should_remove.then(|| state.active.remove(key)).flatten();
            if let Some(runtime) = &runtime {
                Self::release_usage(&mut state, runtime.usage);
                if let Some(replacement) = replacement {
                    insert_recent(
                        &mut state,
                        RecentStatus {
                            provenance: runtime.provenance.clone(),
                            state: replacement,
                            resource_exhausted: None,
                            // Unlike a canceled startup above, a retired
                            // runtime reports a failure only when its source
                            // went stale; every other stop is deliberate.
                            failure: (replacement == ExtensionManagedRuntimeState::StaleSource)
                                .then_some(ExtensionRuntimeFailure::StaleSource),
                        },
                    );
                }
            }
            runtime
        };
        if let Some(runtime) = runtime {
            let _ = runtime.process.shutdown().await;
        }
    }

    async fn detach_binding(&self, binding_id: u64, keys: BTreeSet<RuntimeKey>) {
        let mut stop = Vec::new();
        {
            let mut state = lock(&self.inner.state);
            for key in keys {
                let should_stop = state.active.get_mut(&key).is_some_and(|runtime| {
                    runtime.bindings.remove(&binding_id);
                    runtime.bindings.is_empty()
                        && runtime.sharing == ExtensionRuntimeSharing::Isolated
                        && runtime.lifecycle != ExtensionLifecycleProfile::Always
                });
                if should_stop {
                    if let Some(runtime) = state.active.remove(&key) {
                        Self::release_usage(&mut state, runtime.usage);
                        stop.push((runtime.process, runtime.gate));
                    }
                }
            }
        }
        let _ = join_all(stop.into_iter().map(|(process, gate)| async move {
            let _guard = gate.lock().await;
            let _ = process.shutdown().await;
        }))
        .await;
    }

    async fn settle_one_shots(&self, binding_id: u64, keys: BTreeSet<RuntimeKey>) {
        let targets = {
            let state = lock(&self.inner.state);
            keys.into_iter()
                .filter(|key| {
                    state.active.get(key).is_some_and(|runtime| {
                        runtime.lifecycle == ExtensionLifecycleProfile::OneShot
                            && runtime.bindings.contains(&binding_id)
                    })
                })
                .collect::<Vec<_>>()
        };
        for key in targets {
            self.stop_key(&key, Some(ExtensionManagedRuntimeState::Stopped))
                .await;
        }
    }

    async fn reload_key(
        &self,
        key: RuntimeKey,
        automatic: bool,
    ) -> Result<ExtensionReloadReport, ExtensionRuntimeManagerError> {
        self.reconcile_dead_usage();
        let gate = {
            let state = lock(&self.inner.state);
            state
                .active
                .get(&key)
                .map(|runtime| Arc::clone(&runtime.gate))
        };
        let Some(gate) = gate else {
            return Err(ExtensionRuntimeManagerError::UnknownExtension);
        };
        let _gate = gate.lock().await;
        if self.inner.shutdown.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeManagerError::ManagerClosed);
        }
        let (descriptor, provenance, process, estimated_usage) = {
            let mut state = lock(&self.inner.state);
            let Some(runtime) = state.active.get_mut(&key) else {
                return Err(ExtensionRuntimeManagerError::UnknownExtension);
            };
            if !Arc::ptr_eq(&runtime.gate, &gate) {
                return Err(ExtensionRuntimeManagerError::UnknownExtension);
            }
            let now = Instant::now();
            let history = if automatic {
                &mut runtime.restarts
            } else {
                &mut runtime.reloads
            };
            while history
                .front()
                .is_some_and(|at| now.duration_since(*at) > self.inner.budget.restart_window)
            {
                history.pop_front();
            }
            let limit = if automatic {
                self.inner.budget.max_restarts_per_window
            } else {
                self.inner.budget.max_reloads_per_window
            };
            if history.len() >= limit {
                let resource = if automatic {
                    ExtensionRuntimeResource::RestartStorm
                } else {
                    ExtensionRuntimeResource::Reloads
                };
                let exhausted = ExtensionResourceExhausted {
                    resource,
                    limit: limit as u64,
                    requested: 1,
                    in_use: history.len() as u64,
                    provenance: runtime.provenance.clone(),
                };
                let runtime_state = if automatic {
                    ExtensionManagedRuntimeState::Parked
                } else {
                    ExtensionManagedRuntimeState::ResourceExhausted
                };
                runtime.state = runtime_state;
                let recent = RecentStatus {
                    provenance: runtime.provenance.clone(),
                    state: runtime_state,
                    resource_exhausted: Some(exhausted.clone()),
                    failure: None,
                };
                // Do not retain the runtime borrow while updating the separate
                // bounded public diagnostic map.
                let _ = runtime;
                insert_recent(&mut state, recent);
                return Err(exhausted.into());
            }
            history.push_back(now);
            runtime.state = ExtensionManagedRuntimeState::Starting;
            (
                runtime.descriptor.clone(),
                runtime.provenance.clone(),
                runtime.process.clone(),
                runtime.estimated_usage,
            )
        };
        let _transition = ReloadTransition {
            manager: Arc::downgrade(&self.inner),
            key: key.clone(),
            gate: Arc::clone(&gate),
        };

        let entry = read(&self.inner.catalog)
            .get(&descriptor.manifest.name)
            .cloned();
        let Some(entry) = entry else {
            self.stop_key_after_gate(&key, Some(ExtensionManagedRuntimeState::StaleSource), &gate)
                .await;
            return Err(ExtensionRuntimeManagerError::StaleSource);
        };
        if let Err(error) = self.validate_current_entry(&key, &entry) {
            let replacement = if matches!(&error, ExtensionRuntimeManagerError::NotEligible) {
                ExtensionManagedRuntimeState::Inactive
            } else {
                ExtensionManagedRuntimeState::StaleSource
            };
            self.stop_key_after_gate(&key, Some(replacement), &gate)
                .await;
            self.record_entry_validation_failure(&provenance, &error);
            return Err(error);
        }

        let mut transient = {
            let mut state = lock(&self.inner.state);
            match self.reserve(&mut state, estimated_usage, &provenance) {
                Ok(()) => Some(UsageReservation {
                    manager: Arc::downgrade(&self.inner),
                    usage: estimated_usage,
                    armed: true,
                }),
                Err(error) => {
                    record_reservation_exhaustion(&mut state, &provenance, &error);
                    if let Some(runtime) = state.active.get_mut(&key) {
                        runtime.state = if runtime.process.is_running() {
                            ExtensionManagedRuntimeState::Ready
                        } else {
                            ExtensionManagedRuntimeState::Backoff
                        };
                    }
                    return Err(error);
                }
            }
        };

        let permit = match self.acquire_startup(&provenance).await {
            Ok(permit) => permit,
            Err(error) => {
                if let ExtensionRuntimeManagerError::ResourceExhausted(exhausted) = &error {
                    self.record_reload_exhaustion(&key, automatic, exhausted.clone());
                }
                return Err(error);
            }
        };
        let result =
            tokio::time::timeout(self.inner.budget.startup_timeout, process.reload()).await;
        drop(permit);
        match result {
            Ok(Ok(report)) => {
                if self.inner.shutdown.load(Ordering::Acquire) {
                    return Err(ExtensionRuntimeManagerError::ManagerClosed);
                }
                if let Err(error) = self.validate_current_entry(&key, &entry) {
                    let replacement = if matches!(&error, ExtensionRuntimeManagerError::NotEligible)
                    {
                        ExtensionManagedRuntimeState::Inactive
                    } else {
                        ExtensionManagedRuntimeState::StaleSource
                    };
                    self.stop_key_after_gate(&key, Some(replacement), &gate)
                        .await;
                    self.record_entry_validation_failure(&provenance, &error);
                    return Err(error);
                }
                let committed = {
                    let mut state = lock(&self.inner.state);
                    let old_usage = match state.active.get_mut(&key) {
                        Some(runtime) if Arc::ptr_eq(&runtime.gate, &gate) => {
                            let old_usage = runtime.usage;
                            runtime.usage = estimated_usage;
                            runtime.estimated_usage = estimated_usage;
                            runtime.state = ExtensionManagedRuntimeState::Ready;
                            runtime.restart_attempt = 0;
                            runtime.next_restart = None;
                            Some(old_usage)
                        }
                        _ => None,
                    };
                    if let Some(old_usage) = old_usage {
                        Self::release_usage(&mut state, old_usage);
                        state.recent.remove(&provenance.extension);
                        if let Some(reservation) = transient.as_mut() {
                            reservation.disarm();
                        }
                        true
                    } else {
                        false
                    }
                };
                if !committed {
                    let _ = process.shutdown().await;
                    return Err(if self.inner.shutdown.load(Ordering::Acquire) {
                        ExtensionRuntimeManagerError::ManagerClosed
                    } else {
                        ExtensionRuntimeManagerError::UnknownExtension
                    });
                }
                Ok(report)
            }
            Ok(Err(error)) => {
                self.record_reload_failure(&key, automatic, &provenance, &error);
                Err(ExtensionRuntimeManagerError::Failed {
                    failure: classify_process_failure(&error),
                })
            }
            Err(_) => {
                let exhausted = Self::startup_timeout_exhaustion(&self.inner.budget, &provenance);
                self.record_reload_exhaustion(&key, automatic, exhausted.clone());
                Err(exhausted.into())
            }
        }
    }

    fn record_reload_failure(
        &self,
        key: &RuntimeKey,
        automatic: bool,
        provenance: &ExtensionRuntimeProvenance,
        error: &ProcessRuntimeError,
    ) {
        let failure = classify_process_failure(error);
        let mut state = lock(&self.inner.state);
        if let Some(runtime) = state.active.get_mut(key) {
            runtime.state = if automatic {
                runtime.restart_attempt = runtime.restart_attempt.saturating_add(1);
                let multiplier = 1_u32 << runtime.restart_attempt.saturating_sub(1).min(8);
                runtime.next_restart = Some(
                    Instant::now() + self.inner.budget.restart_backoff.saturating_mul(multiplier),
                );
                ExtensionManagedRuntimeState::Backoff
            } else if runtime.process.is_running() {
                ExtensionManagedRuntimeState::Ready
            } else {
                ExtensionManagedRuntimeState::Parked
            };
        }
        insert_recent(
            &mut state,
            RecentStatus {
                provenance: provenance.clone(),
                state: if automatic {
                    ExtensionManagedRuntimeState::Backoff
                } else {
                    ExtensionManagedRuntimeState::Parked
                },
                resource_exhausted: None,
                failure: Some(failure),
            },
        );
    }

    fn record_reload_exhaustion(
        &self,
        key: &RuntimeKey,
        automatic: bool,
        exhausted: ExtensionResourceExhausted,
    ) {
        let mut state = lock(&self.inner.state);
        if let Some(runtime) = state.active.get_mut(key) {
            runtime.state = if automatic {
                runtime.restart_attempt = runtime.restart_attempt.saturating_add(1);
                runtime.next_restart = Some(Instant::now() + self.inner.budget.restart_backoff);
                ExtensionManagedRuntimeState::Backoff
            } else {
                ExtensionManagedRuntimeState::Ready
            };
        }
        insert_recent(
            &mut state,
            RecentStatus {
                provenance: exhausted.provenance.clone(),
                state: if automatic {
                    ExtensionManagedRuntimeState::Backoff
                } else {
                    ExtensionManagedRuntimeState::ResourceExhausted
                },
                resource_exhausted: Some(exhausted),
                failure: None,
            },
        );
    }

    async fn acquire_startup(
        &self,
        provenance: &ExtensionRuntimeProvenance,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, ExtensionRuntimeManagerError> {
        match tokio::time::timeout(
            self.inner.budget.startup_timeout,
            Arc::clone(&self.inner.startup_slots).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(ExtensionRuntimeManagerError::ManagerClosed),
            Err(_) => Err(ExtensionResourceExhausted {
                resource: ExtensionRuntimeResource::StartupConcurrency,
                limit: self.inner.budget.max_concurrent_startups as u64,
                requested: 1,
                in_use: self
                    .inner
                    .budget
                    .max_concurrent_startups
                    .saturating_sub(self.inner.startup_slots.available_permits())
                    as u64,
                provenance: provenance.clone(),
            }
            .into()),
        }
    }
}

fn classify_process_failure(error: &ProcessRuntimeError) -> ExtensionRuntimeFailure {
    match error {
        ProcessRuntimeError::Disabled(_) | ProcessRuntimeError::Untrusted(_) => {
            ExtensionRuntimeFailure::NotEligible
        }
        ProcessRuntimeError::Timeout { .. } => ExtensionRuntimeFailure::StartupTimeout,
        ProcessRuntimeError::Protocol(_) | ProcessRuntimeError::UnsupportedApiVersion { .. } => {
            ExtensionRuntimeFailure::Protocol
        }
        _ => ExtensionRuntimeFailure::Launch,
    }
}

// Restore the manager transition before its lifecycle gate unlocks if the
// reload future is dropped. Process-level cancellation owns candidate cleanup;
// a draining process must not be advertised as attachable after cancellation.
struct ReloadTransition {
    manager: Weak<ManagerInner>,
    key: RuntimeKey,
    gate: Arc<Mutex<()>>,
}

impl Drop for ReloadTransition {
    fn drop(&mut self) {
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let mut state = lock(&manager.state);
        let Some(runtime) = state.active.get_mut(&self.key) else {
            return;
        };
        if !Arc::ptr_eq(&runtime.gate, &self.gate)
            || runtime.state != ExtensionManagedRuntimeState::Starting
        {
            return;
        }
        runtime.state = if !runtime.process.is_running() {
            runtime.next_restart = Some(Instant::now() + manager.budget.restart_backoff);
            ExtensionManagedRuntimeState::Backoff
        } else if matches!(
            runtime.process.health_snapshot().state,
            ExtensionHealthState::Ready | ExtensionHealthState::Degraded
        ) {
            ExtensionManagedRuntimeState::Ready
        } else {
            ExtensionManagedRuntimeState::Parked
        };
    }
}

struct UsageReservation {
    manager: Weak<ManagerInner>,
    usage: ExtensionRuntimeUsage,
    armed: bool,
}

impl UsageReservation {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for UsageReservation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let mut state = lock(&manager.state);
        state.usage.processes = state.usage.processes.saturating_sub(self.usage.processes);
        state.usage.file_descriptors = state
            .usage
            .file_descriptors
            .saturating_sub(self.usage.file_descriptors);
        state.usage.buffered_bytes = state
            .usage
            .buffered_bytes
            .saturating_sub(self.usage.buffered_bytes);
    }
}

struct StartReservation {
    manager: Weak<ManagerInner>,
    key: RuntimeKey,
    reservation: Arc<AtomicBool>,
    usage: ExtensionRuntimeUsage,
    notify: Arc<Notify>,
    armed: bool,
}

impl StartReservation {
    fn is_current(&self, state: &ManagerState) -> bool {
        state
            .starting
            .get(&self.key)
            .is_some_and(|starting| Arc::ptr_eq(&starting.reservation, &self.reservation))
    }

    fn disarm(&mut self) {
        self.armed = false;
        self.reservation.store(false, Ordering::Release);
    }
}

impl Drop for StartReservation {
    fn drop(&mut self) {
        if !self.armed || !self.reservation.swap(false, Ordering::AcqRel) {
            return;
        }
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let mut state = lock(&manager.state);
        if state
            .starting
            .get(&self.key)
            .is_some_and(|starting| Arc::ptr_eq(&starting.reservation, &self.reservation))
        {
            state.starting.remove(&self.key);
        }
        ExtensionRuntimeManager::release_usage(&mut state, self.usage);
        self.notify.notify_waiters();
    }
}

/// Session-scoped attachment to a durable runtime manager.
#[derive(Clone)]
pub struct ExtensionSessionBinding {
    manager: ExtensionRuntimeManager,
    id: u64,
    session_digest: String,
    resource_owner: String,
    active: Arc<StdMutex<BTreeSet<RuntimeKey>>>,
    released: Arc<AtomicBool>,
    release_notify: Arc<Notify>,
    owners: Arc<()>,
}

impl ExtensionSessionBinding {
    /// Returns the path-free session-binding fingerprint.
    pub fn session_fingerprint(&self) -> &str {
        &self.session_digest
    }

    /// Explicitly activates one static catalog entry.
    pub async fn activate(
        &self,
        extension: &str,
        config: ExtensionRuntimeConfig,
    ) -> Result<ExtensionRuntimeLease, ExtensionRuntimeManagerError> {
        // Register before checking `released` so release also wakes callers
        // waiting on another startup or a reload gate, without a lost wakeup.
        let released = Arc::clone(&self.release_notify).notified_owned();
        tokio::select! {
            biased;
            _ = released => Err(ExtensionRuntimeManagerError::BindingClosed),
            result = self.activate_inner(extension, config) => result,
        }
    }

    async fn activate_inner(
        &self,
        extension: &str,
        mut config: ExtensionRuntimeConfig,
    ) -> Result<ExtensionRuntimeLease, ExtensionRuntimeManagerError> {
        if self.released.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeManagerError::BindingClosed);
        }
        if self.manager.inner.shutdown.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeManagerError::ManagerClosed);
        }
        let entry = read(&self.manager.inner.catalog)
            .get(extension)
            .cloned()
            .ok_or(ExtensionRuntimeManagerError::UnknownExtension)?;
        let provenance = self.manager.provenance(&entry);
        let key = self.manager.runtime_key(&entry, self);
        if !ExtensionRuntimeManager::entry_is_eligible(&entry) {
            self.manager.record_recent(
                provenance,
                ExtensionManagedRuntimeState::Inactive,
                None,
                Some(ExtensionRuntimeFailure::NotEligible),
            );
            return Err(ExtensionRuntimeManagerError::NotEligible);
        }
        if entry.sharing() == ExtensionRuntimeSharing::Workspace {
            if entry.descriptor.manifest.api_version == EXTENSION_API_VERSION_0_1 {
                return Err(ExtensionRuntimeManagerError::SharedApiUnsupported);
            }
            if !entry.source_verified {
                self.manager
                    .stop_key(&key, Some(ExtensionManagedRuntimeState::StaleSource))
                    .await;
                self.manager.record_recent(
                    provenance,
                    ExtensionManagedRuntimeState::StaleSource,
                    None,
                    Some(ExtensionRuntimeFailure::StaleSource),
                );
                return Err(ExtensionRuntimeManagerError::UnverifiedSharedSource);
            }
            if config.agent_sessions
                || config.session_lifecycle.is_some()
                || config.event_bus.is_some()
                || config.approvals
                || config.secret_broker.is_some()
            {
                return Err(ExtensionRuntimeManagerError::SharedServiceUnsupported);
            }
            // Shared process initialization must not cache the first binding's
            // session/model/skill snapshot. Every operation still carries its
            // owner-scoped execution context.
            config.host_state = Default::default();
        }
        let configured_workspace = CanonicalWorkspace::new(&config.workspace)
            .map_err(|_| ExtensionRuntimeManagerError::WorkspaceMismatch)?;
        if configured_workspace != *self.manager.inner.domain.workspace() {
            return Err(ExtensionRuntimeManagerError::WorkspaceMismatch);
        }
        config.workspace = self.manager.inner.domain.workspace().path().to_owned();
        config.supervise = false;

        let (current_digest, source_verified) = entry
            .current_digest()
            .map_err(|_| ExtensionRuntimeManagerError::StaleSource)?;
        if current_digest != entry.content_digest
            || (entry.sharing() == ExtensionRuntimeSharing::Workspace && !source_verified)
        {
            self.manager
                .stop_key(&key, Some(ExtensionManagedRuntimeState::StaleSource))
                .await;
            self.manager.record_recent(
                provenance,
                ExtensionManagedRuntimeState::StaleSource,
                None,
                Some(ExtensionRuntimeFailure::StaleSource),
            );
            return Err(ExtensionRuntimeManagerError::StaleSource);
        }

        loop {
            self.manager.reconcile_dead_usage();
            let (wait, reservation, restart, gate) = {
                // Match catalog replacement's lock order and keep eligibility
                // stable through admission, including after any awaited gate.
                let catalog = read(&self.manager.inner.catalog);
                ExtensionRuntimeManager::validate_catalog_identity(
                    &key,
                    &entry,
                    catalog.get(extension),
                )?;
                let mut state = lock(&self.manager.inner.state);
                let mut active = lock(&self.active);
                if self.released.load(Ordering::Acquire) {
                    return Err(ExtensionRuntimeManagerError::BindingClosed);
                }
                if self.manager.inner.shutdown.load(Ordering::Acquire) {
                    return Err(ExtensionRuntimeManagerError::ManagerClosed);
                }
                if let Some(runtime) = state.active.get_mut(&key) {
                    if ExtensionRuntimeManager::runtime_is_attachable(runtime) {
                        runtime.bindings.insert(self.id);
                        active.insert(key.clone());
                        return Ok(ExtensionRuntimeLease {
                            process: runtime.process.clone(),
                            provenance: runtime.provenance.clone(),
                            shared: runtime.sharing == ExtensionRuntimeSharing::Workspace,
                            one_shot: runtime.lifecycle == ExtensionLifecycleProfile::OneShot,
                        });
                    }
                    if !runtime.process.is_running()
                        && runtime.lifecycle != ExtensionLifecycleProfile::OneShot
                        && matches!(
                            runtime.state,
                            ExtensionManagedRuntimeState::Ready
                                | ExtensionManagedRuntimeState::Backoff
                        )
                    {
                        (None, None, true, None)
                    } else if runtime.state == ExtensionManagedRuntimeState::Parked {
                        return Err(ExtensionRuntimeManagerError::Failed {
                            failure: ExtensionRuntimeFailure::Launch,
                        });
                    } else {
                        // Reload holds this gate while the old generation is
                        // draining. Never attach a lease to that generation.
                        (None, None, false, Some(Arc::clone(&runtime.gate)))
                    }
                } else if let Some(wait) = state.starting.get(&key) {
                    // Create the waiter before releasing the state lock: a
                    // completed startup's notify_waiters stores no later permit.
                    (
                        Some(Arc::clone(&wait.notify).notified_owned()),
                        None,
                        false,
                        None,
                    )
                } else {
                    let usage = ExtensionRuntimeManager::estimated_usage(&config);
                    match self.manager.reserve(&mut state, usage, &provenance) {
                        Ok(()) => {
                            let notify = Arc::new(Notify::new());
                            let reservation_token = Arc::new(AtomicBool::new(true));
                            state.starting.insert(
                                key.clone(),
                                StartingRuntime {
                                    notify: Arc::clone(&notify),
                                    reservation: Arc::clone(&reservation_token),
                                    provenance: provenance.clone(),
                                    usage,
                                },
                            );
                            (
                                None,
                                Some(StartReservation {
                                    manager: Arc::downgrade(&self.manager.inner),
                                    key: key.clone(),
                                    reservation: reservation_token,
                                    usage,
                                    notify,
                                    armed: true,
                                }),
                                false,
                                None,
                            )
                        }
                        Err(error) => {
                            record_reservation_exhaustion(&mut state, &provenance, &error);
                            return Err(error);
                        }
                    }
                }
            };
            if restart {
                self.manager.reload_key(key.clone(), true).await?;
                continue;
            }
            if let Some(gate) = gate {
                let _guard = gate.lock().await;
                tokio::time::sleep(SUPERVISOR_POLL).await;
                continue;
            }
            if let Some(reservation) = reservation {
                // Catalog replacement and shutdown also wake the startup owner,
                // not only callers coalesced behind it. Check identity after
                // registering the waiter so a removed/reselected key cannot
                // commit an earlier reservation into the new generation.
                let invalidated = Arc::clone(&reservation.notify).notified_owned();
                {
                    let state = lock(&self.manager.inner.state);
                    // Shutdown removes starting reservations under this lock.
                    // Check closure and currency together so shutdown cannot
                    // remove our reservation between the two checks and be
                    // misreported as a catalog/source change.
                    if self.manager.inner.shutdown.load(Ordering::Acquire) {
                        return Err(ExtensionRuntimeManagerError::ManagerClosed);
                    }
                    if !reservation.is_current(&state) {
                        return Err(ExtensionRuntimeManagerError::StaleSource);
                    }
                }
                return tokio::select! {
                    biased;
                    _ = invalidated => Err(if self.manager.inner.shutdown.load(Ordering::Acquire) {
                        ExtensionRuntimeManagerError::ManagerClosed
                    } else {
                        ExtensionRuntimeManagerError::StaleSource
                    }),
                    result = self.start_new(entry, provenance, key, config, reservation) => result,
                };
            }
            let wait = wait.expect("a non-starting activation waits for its owner");
            wait.await;
            if self.released.load(Ordering::Acquire) {
                return Err(ExtensionRuntimeManagerError::BindingClosed);
            }
        }
    }

    async fn start_new(
        &self,
        entry: ExtensionRuntimeCatalogEntry,
        provenance: ExtensionRuntimeProvenance,
        key: RuntimeKey,
        config: ExtensionRuntimeConfig,
        mut reservation: StartReservation,
    ) -> Result<ExtensionRuntimeLease, ExtensionRuntimeManagerError> {
        self.manager.ensure_monitor();
        let permit = match self.manager.acquire_startup(&provenance).await {
            Ok(permit) => permit,
            Err(error) => {
                if let ExtensionRuntimeManagerError::ResourceExhausted(exhausted) = &error {
                    self.manager.record_recent(
                        provenance.clone(),
                        ExtensionManagedRuntimeState::ResourceExhausted,
                        Some(exhausted.clone()),
                        None,
                    );
                }
                return Err(error);
            }
        };
        let started = tokio::time::timeout(
            self.manager.inner.budget.startup_timeout,
            ExtensionProcess::start(entry.descriptor.clone(), config),
        )
        .await;
        drop(permit);
        let process = match started {
            Ok(Ok(process)) if !self.manager.inner.shutdown.load(Ordering::Acquire) => process,
            Ok(Ok(process)) => {
                drop(reservation);
                let _ = process.shutdown().await;
                return Err(ExtensionRuntimeManagerError::ManagerClosed);
            }
            Ok(Err(error)) => {
                let failure = classify_process_failure(&error);
                self.manager.record_recent(
                    provenance,
                    ExtensionManagedRuntimeState::Parked,
                    None,
                    Some(failure),
                );
                return Err(ExtensionRuntimeManagerError::Failed { failure });
            }
            Err(_) => {
                let exhausted = ExtensionRuntimeManager::startup_timeout_exhaustion(
                    &self.manager.inner.budget,
                    &provenance,
                );
                self.manager.record_recent(
                    provenance,
                    ExtensionManagedRuntimeState::ResourceExhausted,
                    Some(exhausted.clone()),
                    None,
                );
                return Err(exhausted.into());
            }
        };
        if let Err(error) = self.manager.validate_current_entry(&key, &entry) {
            let _ = process.shutdown().await;
            self.manager
                .record_entry_validation_failure(&provenance, &error);
            return Err(error);
        }
        // The start reservation owns the exact caller configuration charge,
        // including queue and pending-request bounds.
        let usage = reservation.usage;
        let lifecycle = entry.lifecycle();
        let sharing = entry.sharing();
        let one_shot = lifecycle == ExtensionLifecycleProfile::OneShot;
        // Hold the catalog read lock through the state transition. Catalog
        // replacement takes the same catalog-then-state order, so it cannot
        // select a new entry between post-start validation and this commit.
        let committed = {
            let catalog = read(&self.manager.inner.catalog);
            match ExtensionRuntimeManager::validate_catalog_identity(
                &key,
                &entry,
                catalog.get(&entry.descriptor.manifest.name),
            ) {
                Err(error) => Err(error),
                Ok(()) => {
                    let mut state = lock(&self.manager.inner.state);
                    let mut active = lock(&self.active);
                    if self.manager.inner.shutdown.load(Ordering::Acquire) {
                        Err(ExtensionRuntimeManagerError::ManagerClosed)
                    } else if self.released.load(Ordering::Acquire) {
                        Err(ExtensionRuntimeManagerError::BindingClosed)
                    } else if !reservation.is_current(&state) {
                        Err(ExtensionRuntimeManagerError::StaleSource)
                    } else {
                        state.starting.remove(&key);
                        state.recent.remove(&provenance.extension);
                        active.insert(key.clone());
                        // Transfer the charge under the same state lock as the
                        // runtime insertion; shutdown cannot release it twice.
                        reservation.disarm();
                        state.active.insert(
                            key.clone(),
                            ManagedRuntime {
                                descriptor: entry.descriptor,
                                provenance: provenance.clone(),
                                process: process.clone(),
                                usage,
                                estimated_usage: usage,
                                bindings: BTreeSet::from([self.id]),
                                lifecycle,
                                sharing,
                                state: ExtensionManagedRuntimeState::Ready,
                                reloads: VecDeque::new(),
                                restarts: VecDeque::new(),
                                restart_attempt: 0,
                                next_restart: None,
                                gate: Arc::new(Mutex::new(())),
                            },
                        );
                        Ok(())
                    }
                }
            }
        };
        if let Err(error) = committed {
            let _ = process.shutdown().await;
            self.manager
                .record_entry_validation_failure(&provenance, &error);
            return Err(error);
        }
        reservation.notify.notify_waiters();
        if self.manager.inner.shutdown.load(Ordering::Acquire) {
            lock(&self.active).remove(&key);
            return Err(ExtensionRuntimeManagerError::ManagerClosed);
        }
        if self.released.load(Ordering::Acquire) {
            process.retire_resource_owner(&self.resource_owner);
            let keys = {
                let mut active = lock(&self.active);
                std::mem::take(&mut *active)
            };
            self.manager.detach_binding(self.id, keys).await;
            return Err(ExtensionRuntimeManagerError::BindingClosed);
        }
        Ok(ExtensionRuntimeLease {
            process,
            provenance,
            shared: false,
            one_shot,
        })
    }

    /// Activates only eager lifecycle profiles from an explicit eligible-name set.
    ///
    /// The caller retains policy ownership (safe mode, process gate, and
    /// enable/trust diagnostics); the manager retains resource accounting.
    pub async fn activate_eager<F>(
        &self,
        eligible_names: impl IntoIterator<Item = String>,
        mut config_for: F,
    ) -> Vec<ExtensionRuntimeActivation>
    where
        F: FnMut(&ExtensionRuntimeCatalogEntry) -> ExtensionRuntimeConfig,
    {
        let eligible = eligible_names.into_iter().collect::<BTreeSet<_>>();
        let entries = self
            .manager
            .catalog()
            .entries()
            .filter(|entry| {
                eligible.contains(&entry.descriptor.manifest.name)
                    && matches!(
                        entry.lifecycle(),
                        ExtensionLifecycleProfile::LegacyResident
                            | ExtensionLifecycleProfile::Session
                            | ExtensionLifecycleProfile::WorkspaceService
                            | ExtensionLifecycleProfile::Always
                            | ExtensionLifecycleProfile::PiAggregate
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        let requests = entries
            .into_iter()
            .map(|entry| {
                let name = entry.descriptor.manifest.name.clone();
                let provenance = self.manager.provenance(&entry);
                let config = config_for(&entry);
                let binding = self.clone();
                async move {
                    match binding.activate(&name, config).await {
                        Ok(lease) => ExtensionRuntimeActivation {
                            extension: name,
                            provenance: Some(lease.provenance.clone()),
                            process: Some(lease.process),
                            shared: lease.shared,
                            outcome: ExtensionRuntimeActivationOutcome::Ready,
                        },
                        Err(error) => ExtensionRuntimeActivation {
                            extension: name,
                            provenance: Some(provenance),
                            process: None,
                            shared: false,
                            outcome: activation_outcome(error),
                        },
                    }
                }
            })
            .collect::<Vec<_>>();
        join_all(requests).await
    }

    /// Returns active process handles attached to this binding in deterministic order.
    pub fn processes(&self) -> Vec<ExtensionProcess> {
        let keys = lock(&self.active).clone();
        let state = lock(&self.manager.inner.state);
        keys.into_iter()
            .filter_map(|key| {
                state
                    .active
                    .get(&key)
                    .map(|runtime| runtime.process.clone())
            })
            .collect()
    }

    /// Stops one-shot processes owned by this binding after their operation settles.
    pub async fn settle_one_shots(&self) {
        let keys = lock(&self.active).clone();
        self.manager.settle_one_shots(self.id, keys).await;
        let active = lock(&self.manager.inner.state)
            .active
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        lock(&self.active).retain(|key| active.contains(key));
    }

    fn retire_resources(&self) {
        // This synchronous fence must not depend on a Tokio runtime, UI grant,
        // subscribed hook, successful native disposal, or later fleet shutdown.
        for process in self.processes() {
            process.retire_resource_owner(&self.resource_owner);
        }
    }

    /// Releases the session binding without shutting down workspace-shared or
    /// always-owned runtimes. Call [`ExtensionRuntimeManager::shutdown`] when
    /// the host itself is ending.
    pub async fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.retire_resources();
        self.release_notify.notify_waiters();
        let keys = std::mem::take(&mut *lock(&self.active));
        self.manager.detach_binding(self.id, keys).await;
    }
}

impl Drop for ExtensionSessionBinding {
    fn drop(&mut self) {
        // `activate_eager` and callers may clone a binding for concurrent work.
        // A temporary clone cannot tear down the attachment owned by the
        // original binding.
        if Arc::strong_count(&self.owners) != 1 || self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.retire_resources();
        let keys = std::mem::take(&mut *lock(&self.active));
        if keys.is_empty() {
            return;
        }
        if let Ok(handle) = Handle::try_current() {
            let manager = self.manager.clone();
            let id = self.id;
            handle.spawn(async move { manager.detach_binding(id, keys).await });
        }
    }
}

fn activation_outcome(error: ExtensionRuntimeManagerError) -> ExtensionRuntimeActivationOutcome {
    match error {
        ExtensionRuntimeManagerError::NotEligible => ExtensionRuntimeActivationOutcome::Inactive,
        ExtensionRuntimeManagerError::StaleSource
        | ExtensionRuntimeManagerError::UnverifiedSharedSource => {
            ExtensionRuntimeActivationOutcome::StaleSource
        }
        ExtensionRuntimeManagerError::ResourceExhausted(exhausted) => {
            ExtensionRuntimeActivationOutcome::ResourceExhausted(exhausted)
        }
        ExtensionRuntimeManagerError::Failed { failure } => {
            ExtensionRuntimeActivationOutcome::Failed(failure)
        }
        ExtensionRuntimeManagerError::ManagerClosed => {
            ExtensionRuntimeActivationOutcome::Failed(ExtensionRuntimeFailure::ManagerClosed)
        }
        _ => ExtensionRuntimeActivationOutcome::Failed(ExtensionRuntimeFailure::Launch),
    }
}

#[cfg(test)]
mod tests;
