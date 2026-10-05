//! Budget, usage, status, and error vocabulary for the extension fleet.
//!
//! Every governed quantity the fleet exposes is declared here: the aggregate
//! [`ExtensionRuntimeBudget`], the per-runtime [`ExtensionRuntimeUsage`] charge,
//! the inspectable [`ExtensionRuntimeStatus`], the bounded public failure
//! classes, and the typed [`ExtensionResourceExhausted`] outcome that is also
//! API 0.3's stable `resource_exhausted` error.
//!
//! This is separate from [`super::manager`] so the limits and the reporting
//! shapes can be read, reviewed, and serialized without the launch, reload,
//! and session-binding machinery. Nothing in this module launches a process or
//! touches a runtime's lifecycle state.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::extension_process::{ExtensionLifecycleProfile, ExtensionProcess};
/// Aggregate process-fleet limits enforced by a runtime manager.
#[derive(Clone, Debug)]
pub struct ExtensionRuntimeBudget {
    /// Maximum simultaneously owned child processes.
    pub max_processes: usize,
    /// Maximum conservatively reserved extension file descriptors.
    pub max_file_descriptors: usize,
    /// Maximum conservatively reserved buffered protocol bytes.
    pub max_buffered_bytes: usize,
    /// Maximum launches/handshakes in progress at once.
    pub max_concurrent_startups: usize,
    /// Maximum wait for a startup permit and one launch/handshake.
    pub startup_timeout: Duration,
    /// Maximum manual reloads in one restart window per runtime.
    pub max_reloads_per_window: usize,
    /// Maximum crash restarts in one restart window per runtime.
    pub max_restarts_per_window: usize,
    /// Rolling window for reload and restart-storm governance.
    pub restart_window: Duration,
    /// Base delay before a manager-supervised crash restart retry.
    pub restart_backoff: Duration,
}

impl Default for ExtensionRuntimeBudget {
    fn default() -> Self {
        Self {
            max_processes: 64,
            max_file_descriptors: 256,
            max_buffered_bytes: 16 * 1024 * 1024 * 1024,
            max_concurrent_startups: 4,
            startup_timeout: Duration::from_secs(30),
            max_reloads_per_window: 16,
            max_restarts_per_window: 8,
            restart_window: Duration::from_secs(60),
            restart_backoff: Duration::from_millis(250),
        }
    }
}

impl ExtensionRuntimeBudget {
    pub(super) fn validate(&self) -> Result<(), ExtensionRuntimeManagerError> {
        if self.max_processes == 0
            || self.max_file_descriptors == 0
            || self.max_buffered_bytes == 0
            || self.max_concurrent_startups == 0
            || self.startup_timeout.is_zero()
            || self.max_reloads_per_window == 0
            || self.max_restarts_per_window == 0
            || self.restart_window.is_zero()
        {
            return Err(ExtensionRuntimeManagerError::InvalidBudget);
        }
        Ok(())
    }
}

/// Governed resource kind that was exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRuntimeResource {
    /// Child-process count.
    Processes,
    /// Conservatively reserved standard-stream/process descriptors.
    FileDescriptors,
    /// Conservatively reserved protocol buffering.
    BufferedBytes,
    /// Concurrent launch or handshake slots.
    StartupConcurrency,
    /// Launch or handshake wall-clock time.
    StartupTime,
    /// Explicit reload-rate budget.
    Reloads,
    /// Automatic crash-restart storm budget.
    RestartStorm,
}

/// Secret-safe runtime provenance included in visible governance outcomes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRuntimeProvenance {
    /// Manifest-selected extension name.
    pub extension: String,
    /// Content-bound runtime digest.
    pub content_digest: String,
    /// Manifest-selected lifecycle profile.
    pub lifecycle: ExtensionLifecycleProfile,
}

/// Typed, visible aggregate resource exhaustion outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionResourceExhausted {
    /// Exhausted aggregate resource.
    pub resource: ExtensionRuntimeResource,
    /// Configured hard limit.
    pub limit: u64,
    /// New reservation or event request.
    pub requested: u64,
    /// Usage retained before the rejected request.
    pub in_use: u64,
    /// Secret-safe ownership/provenance.
    pub provenance: ExtensionRuntimeProvenance,
}

impl std::fmt::Display for ExtensionResourceExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:?} budget exhausted ({} used + {} requested; limit {}) for {}",
            self.resource, self.in_use, self.requested, self.limit, self.provenance.extension
        )
    }
}

impl std::error::Error for ExtensionResourceExhausted {}

impl ExtensionResourceExhausted {
    /// API 0.3's stable JSON-RPC code for this outcome.
    pub const JSON_RPC_CODE: i64 = -32012;

    /// Returns API 0.3's stable error name.
    pub const fn api_error_name(&self) -> &'static str {
        "resource_exhausted"
    }
}

/// Public classification of a process launch/reload failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRuntimeFailure {
    /// The manifest was disabled or untrusted.
    NotEligible,
    /// Source content changed or was no longer verifiable for a shared runtime.
    StaleSource,
    /// Launching the child failed.
    Launch,
    /// Initialization/negotiation rejected the child.
    Protocol,
    /// The manager startup timer elapsed.
    StartupTimeout,
    /// A runtime was shut down while a start was pending.
    ManagerClosed,
}

/// Runtime-manager operation error.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionRuntimeManagerError {
    /// The configured aggregate budget is invalid.
    #[error("invalid extension runtime budget")]
    InvalidBudget,
    /// A caller asked for an entry absent from the static catalog.
    #[error("extension runtime entry is unavailable")]
    UnknownExtension,
    /// A process is disabled, untrusted, or blocked by the caller's gate.
    #[error("extension runtime is not eligible")]
    NotEligible,
    /// A shared profile lacks a directly verified content source.
    #[error("extension runtime source cannot be content-bound for sharing")]
    UnverifiedSharedSource,
    /// A static source changed before start/reload and was retired fail-closed.
    #[error("extension runtime source changed")]
    StaleSource,
    /// A session binding was already released.
    #[error("extension runtime session binding is closed")]
    BindingClosed,
    /// The manager was shut down.
    #[error("extension runtime manager is shut down")]
    ManagerClosed,
    /// A caller supplied a config for another workspace domain.
    #[error("extension runtime configuration belongs to another workspace")]
    WorkspaceMismatch,
    /// A shared runtime requested session-specific reverse services.
    #[error("shared extension runtime cannot expose session-specific host services")]
    SharedServiceUnsupported,
    /// API 0.1 cannot carry resource-owner fences required for sharing.
    #[error("shared extension runtime requires API 0.2 or newer")]
    SharedApiUnsupported,
    /// One aggregate resource was exhausted.
    #[error(transparent)]
    ResourceExhausted(#[from] ExtensionResourceExhausted),
    /// Process start/reload failed without exposing arbitrary child diagnostics.
    #[error("extension runtime {failure:?}")]
    Failed {
        /// Bounded secret-safe failure class.
        failure: ExtensionRuntimeFailure,
        /// Host-generated launch detail (a missing or unusable executable),
        /// never child output.
        detail: Option<String>,
    },
}

/// Current externally observable manager state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionManagedRuntimeState {
    /// The static entry is eligible but has not been activated.
    Eligible,
    /// The entry is disabled or lacks explicit executable trust.
    Inactive,
    /// A launch/handshake owns a bounded startup slot.
    Starting,
    /// A resident process is ready for use.
    Ready,
    /// The manager is delaying an automatic restart.
    Backoff,
    /// A runtime was retired because its bound source changed.
    StaleSource,
    /// A governed resource prevented activation/reload.
    ResourceExhausted,
    /// Restart policy parked the runtime after a terminal failure.
    Parked,
    /// The runtime was deliberately stopped.
    Stopped,
}

/// Resource usage charged to one active runtime or the aggregate manager.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRuntimeUsage {
    /// Process count.
    pub processes: usize,
    /// Conservatively reserved file descriptors.
    pub file_descriptors: usize,
    /// Conservatively reserved buffered bytes.
    pub buffered_bytes: usize,
}

/// Inspectable status for a static entry or durable active runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRuntimeStatus {
    /// Secret-safe static ownership identity.
    pub provenance: ExtensionRuntimeProvenance,
    /// Current lifecycle state.
    pub state: ExtensionManagedRuntimeState,
    /// Number of current session bindings attached to the process.
    pub bindings: usize,
    /// Charged resource usage while active.
    pub usage: ExtensionRuntimeUsage,
    /// Most recent typed resource rejection, when one occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_exhausted: Option<ExtensionResourceExhausted>,
    /// Bounded public process failure class, without child stderr or paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionRuntimeFailure>,
}

/// Result of activating one static entry through a session binding.
#[derive(Clone)]
pub struct ExtensionRuntimeLease {
    pub(super) process: ExtensionProcess,
    pub(super) provenance: ExtensionRuntimeProvenance,
    pub(super) shared: bool,
    pub(super) one_shot: bool,
}

impl ExtensionRuntimeLease {
    /// Returns the active extension process handle.
    pub fn process(&self) -> &ExtensionProcess {
        &self.process
    }

    /// Returns the secret-safe runtime identity.
    pub fn provenance(&self) -> &ExtensionRuntimeProvenance {
        &self.provenance
    }

    /// Returns whether this activation attached to a pre-existing shared
    /// process rather than starting a new child.
    pub fn shared(&self) -> bool {
        self.shared
    }

    /// Returns whether the binding must settle this lease at the operation
    /// boundary to stop its one-shot process.
    pub fn is_one_shot(&self) -> bool {
        self.one_shot
    }
}

/// Deterministic report for eager profile activation.
#[derive(Clone)]
pub struct ExtensionRuntimeActivation {
    /// Selected manifest name.
    pub extension: String,
    /// Secret-safe identity, when catalog construction succeeded.
    pub provenance: Option<ExtensionRuntimeProvenance>,
    /// Successful process handle, if activation was admitted.
    pub process: Option<ExtensionProcess>,
    /// Whether a successful activation reused an existing shared process.
    pub shared: bool,
    /// Typed visible outcome.
    pub outcome: ExtensionRuntimeActivationOutcome,
    /// Host-generated launch detail for a failed activation, never child output.
    pub detail: Option<String>,
}

/// Typed eager activation outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionRuntimeActivationOutcome {
    /// The runtime is ready.
    Ready,
    /// The entry was not eligible under the caller's explicit activation gate.
    Inactive,
    /// A source change was rejected fail-closed.
    StaleSource,
    /// A typed aggregate budget was exhausted.
    ResourceExhausted(ExtensionResourceExhausted),
    /// A bounded startup failure occurred.
    Failed(ExtensionRuntimeFailure),
}
