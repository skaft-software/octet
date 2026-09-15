//! Durable invocation-scoped state for tool invocations.
//!
//! Pi's `packages/agent/docs/tool-durability.md` gives an in-flight tool exactly
//! two durable, invocation-scoped values, and both are **auxiliary observation
//! data** rather than evidence that an effect settled:
//!
//! * `pi.pending.tool_output` — the latest complete *bounded* progress snapshot
//!   the tool chose to persist (`pendingToolOutput(operationId, invocationId)`,
//!   row 4.7);
//! * `pi.op.tool_memo` — a named replay memo for one invocation
//!   (`operationToolMemo(operationId, invocationId, name)`, row 4.11).
//!
//! Pi stores both in the session's bound-value family, which supports keyed
//! replace, prefix scan, and deletion fenced on the call still being
//! `effect_pending`. octet's session is an append-only JSONL log with no keyed
//! replace/scan API, and `session.rs` is not an owned path for this work, so this
//! module implements the same *contract* behind one store type that the tool
//! layer can use today and the session can back later:
//!
//! 1. **Fencing.** Every value is written through an [`InvocationHandle`] that
//!    carries the generation it was opened with. Settlement bumps the
//!    generation and deletes every value, so a late checkpoint from a settled
//!    invocation is refused instead of reviving state (Pi's "A late checkpoint
//!    after settlement returns without committing").
//! 2. **Bounded by default.** Values, names, per-invocation value counts, live
//!    invocations, and retained settled invocations all have hard caps. An
//!    over-limit write fails closed rather than growing the store; nothing here
//!    silently truncates a memo.
//! 3. **Fail closed on uncertainty.** A memo read on a settled invocation is an
//!    error, never `None`: "no value" means *run the effect*, so conflating an
//!    expired capability with an absent memo would silently re-execute a
//!    recorded step. [`InvocationHandle::replay_step`] therefore runs its effect
//!    only when the capability is live *and* the memo is genuinely absent.
//!
//! Unsettled state is process-local until a host wires the store to the session
//! log; `docs/parity/tools.md` records that gap together with the exact session
//! primitive (`setValue` with an `effect_pending` check, `scanValues` cleanup)
//! that would make it cross-process durable.
//!
//! The store is deliberately not a general key/value database: it accepts only
//! the two owner-defined address families, each with its own namespace prefix,
//! so no caller can claim a reserved key that a later operation would receive.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::tool::{PartialOutputCheckpointSink, ToolError};

/// Namespace prefix for invocation memos (Pi's `pi.op.tool_memo`).
pub const MEMO_NAMESPACE: &str = "pi.op.tool_memo";
/// Namespace prefix for durable partial tool output (Pi's `pi.pending.tool_output`).
pub const PARTIAL_OUTPUT_NAMESPACE: &str = "pi.pending.tool_output";

/// Hard cap for one stored value (one memo or one partial-output snapshot).
pub const DEFAULT_MAX_VALUE_BYTES: usize = 64 * 1024;
/// Hard cap for distinct values retained for one invocation.
pub const DEFAULT_MAX_VALUES_PER_INVOCATION: usize = 256;
/// Hard cap for simultaneously unsettled invocations.
pub const DEFAULT_MAX_LIVE_INVOCATIONS: usize = 64;
/// Hard cap for settled invocation records retained for replay verdicts.
pub const DEFAULT_MAX_SETTLED_INVOCATIONS: usize = 1024;
/// Hard cap for one operation/invocation id or one memo name.
pub const DEFAULT_MAX_NAME_BYTES: usize = 256;

/// The exact human-readable marker Pi requires on a synthesized interruption
/// result. It states both facts a client must not have to infer: the output is
/// a partial snapshot, and the external outcome is unknown.
pub const INTERRUPTED_OUTCOME_UNKNOWN_MARKER: &str = "[Tool execution was interrupted. The preceding output is the latest durable progress snapshot; newer live output may be missing, and the external outcome is unknown.]";

/// A refusal from the durable invocation store. Every variant fails closed: the
/// caller must not proceed as if the value were absent or the write succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationError {
    /// An id or memo name violated the address grammar.
    InvalidAddress(String),
    /// The invocation's outcome is already known, so its capability is expired.
    OutcomeKnown(String),
    /// The handle was opened for a different generation of this invocation.
    Fenced {
        /// Address spelling of the invocation.
        scope: String,
        /// Generation carried by the handle.
        expected: u64,
        /// Generation currently durable for the invocation.
        actual: u64,
    },
    /// A write would exceed a hard storage bound.
    BoundExceeded(String),
    /// A stored value could not be interpreted as its namespace's value type.
    Corrupt(String),
}

impl std::fmt::Display for InvocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAddress(detail) => write!(f, "invalid invocation address: {detail}"),
            Self::OutcomeKnown(scope) => write!(
                f,
                "invocation {scope} already has a durable outcome; its capability is expired"
            ),
            Self::Fenced {
                scope,
                expected,
                actual,
            } => write!(
                f,
                "invocation {scope} moved from generation {expected} to {actual}; the handle is fenced"
            ),
            Self::BoundExceeded(detail) => write!(f, "invocation store bound exceeded: {detail}"),
            Self::Corrupt(detail) => write!(f, "invocation store value is corrupt: {detail}"),
        }
    }
}

impl std::error::Error for InvocationError {}

impl From<InvocationError> for ToolError {
    fn from(error: InvocationError) -> Self {
        ToolError::new(error.to_string())
    }
}

/// Whether the durable store still considers an invocation in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationState {
    /// The effect may still be running: memos and partial output may be written.
    EffectPending,
    /// The outcome is known (real result or synthetic interruption): the
    /// invocation must never execute again and its values are gone.
    OutcomeReady,
}

/// Whether an invocation's external outcome is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationOutcome {
    /// A real or synthetic result establishes the outcome.
    Known,
    /// The outcome is unknown: partial output proves nothing about success.
    Unknown,
}

/// Hard storage bounds for one store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreLimits {
    /// Maximum bytes of one stored value.
    pub max_value_bytes: usize,
    /// Maximum distinct values retained per invocation.
    pub max_values_per_invocation: usize,
    /// Maximum simultaneously unsettled invocations.
    pub max_live_invocations: usize,
    /// Maximum settled invocation records retained for replay verdicts.
    pub max_settled_invocations: usize,
    /// Maximum bytes of one id or memo name.
    pub max_name_bytes: usize,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_value_bytes: DEFAULT_MAX_VALUE_BYTES,
            max_values_per_invocation: DEFAULT_MAX_VALUES_PER_INVOCATION,
            max_live_invocations: DEFAULT_MAX_LIVE_INVOCATIONS,
            max_settled_invocations: DEFAULT_MAX_SETTLED_INVOCATIONS,
            max_name_bytes: DEFAULT_MAX_NAME_BYTES,
        }
    }
}

/// Operation/invocation identity of one tool call.
///
/// A memo or checkpoint is addressed by `namespace:operation:invocation[:name]`.
/// Ids and names may not contain `:` so the grammar stays unambiguous, exactly
/// like Pi's owner-defined address constructors.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InvocationScope {
    operation_id: String,
    invocation_id: String,
}

impl InvocationScope {
    /// Validates and constructs an address-legal scope.
    pub fn new(
        operation_id: impl Into<String>,
        invocation_id: impl Into<String>,
    ) -> Result<Self, InvocationError> {
        let operation_id = operation_id.into();
        let invocation_id = invocation_id.into();
        validate_segment("operation id", &operation_id, DEFAULT_MAX_NAME_BYTES)?;
        validate_segment("invocation id", &invocation_id, DEFAULT_MAX_NAME_BYTES)?;
        Ok(Self {
            operation_id,
            invocation_id,
        })
    }

    /// Durable operation identity.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Durable invocation identity (session-unique, unlike a provider-local call id).
    pub fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    /// Durable address of one memo.
    pub fn memo_address(&self, name: &str) -> Result<String, InvocationError> {
        validate_segment("memo name", name, DEFAULT_MAX_NAME_BYTES)?;
        Ok(format!(
            "{MEMO_NAMESPACE}:{}:{}:{name}",
            self.operation_id, self.invocation_id
        ))
    }

    /// Durable address of this invocation's partial output.
    pub fn partial_output_address(&self) -> String {
        format!(
            "{PARTIAL_OUTPUT_NAMESPACE}:{}:{}",
            self.operation_id, self.invocation_id
        )
    }

    /// Human-readable `operation:invocation` spelling used in diagnostics.
    pub fn display(&self) -> String {
        format!("{}:{}", self.operation_id, self.invocation_id)
    }
}

impl std::fmt::Display for InvocationScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

fn validate_segment(label: &str, value: &str, max_bytes: usize) -> Result<(), InvocationError> {
    if value.is_empty() {
        return Err(InvocationError::InvalidAddress(format!(
            "{label} must be non-empty"
        )));
    }
    if value.contains(':') {
        return Err(InvocationError::InvalidAddress(format!(
            "{label} must not contain ':'"
        )));
    }
    if value.len() > max_bytes {
        return Err(InvocationError::InvalidAddress(format!(
            "{label} is {} bytes (limit {max_bytes})",
            value.len()
        )));
    }
    Ok(())
}

/// What one recorded memo lookup means for a replaying tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoLookup {
    /// The step already ran in this invocation: use the recorded value.
    Memoized(serde_json::Value),
    /// The step has not been recorded *and* the invocation is still
    /// `effect_pending`, so the effect may run now.
    NotYetRecorded,
}

/// Result of settling one invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settlement {
    /// The settled invocation.
    pub scope: InvocationScope,
    /// Generation the invocation moved to; every open handle is now fenced.
    pub generation: u64,
    /// Addresses deleted by the settlement, in address order.
    pub deleted_values: Vec<String>,
}

/// A synthesized interruption result for an invocation whose outcome is unknown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterruptedInvocation {
    /// Bounded partial output plus the mandatory interruption marker.
    pub text: String,
    /// Always `true`: the model receives an error result, which describes the
    /// *delivered* result and deliberately does not assert that the external
    /// effect failed.
    pub is_error: bool,
    /// Always [`InvocationOutcome::Unknown`].
    pub outcome: InvocationOutcome,
    /// The bounded snapshot that was preserved, when one existed.
    pub partial_output: Option<String>,
}

/// Outcome of Pi's unsafe-orphan recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsafeRecovery {
    /// The synthetic interruption result staged for the model.
    pub invocation: InterruptedInvocation,
    /// Cleanup performed atomically with staging that result.
    pub settlement: Settlement,
}

/// Synthesizes Pi's interruption result from an optional bounded snapshot.
///
/// Partial output is auxiliary observation data: this function never infers
/// success or failure from the text it preserves, and always reports the
/// outcome as unknown.
pub fn synthesize_interruption(partial_output: Option<&str>) -> InterruptedInvocation {
    let mut text = match partial_output {
        Some(snapshot) if !snapshot.is_empty() => {
            let mut text = snapshot.to_owned();
            text.push('\n');
            text
        }
        _ => String::new(),
    };
    text.push_str(INTERRUPTED_OUTCOME_UNKNOWN_MARKER);
    InterruptedInvocation {
        text,
        is_error: true,
        outcome: InvocationOutcome::Unknown,
        partial_output: partial_output.map(str::to_owned),
    }
}

#[derive(Clone, Debug)]
struct InvocationRecord {
    generation: u64,
    state: InvocationState,
    values: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
struct StoreState {
    invocations: HashMap<InvocationScope, InvocationRecord>,
    /// Settlement order for bounded retention of replay verdicts.
    settled: VecDeque<InvocationScope>,
}

/// Process-durable store for invocation-scoped memos and partial output.
#[derive(Debug)]
pub struct DurableInvocationStore {
    limits: StoreLimits,
    state: Mutex<StoreState>,
}

impl Default for DurableInvocationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DurableInvocationStore {
    /// Creates a store with the default hard bounds.
    pub fn new() -> Self {
        Self::with_limits(StoreLimits::default())
    }

    /// Creates a store with explicit hard bounds. Every bound is enforced on
    /// write; a zero bound simply rejects that dimension's first write.
    pub fn with_limits(limits: StoreLimits) -> Self {
        Self {
            limits,
            state: Mutex::new(StoreState::default()),
        }
    }

    /// Hard bounds in force for this store.
    pub fn limits(&self) -> StoreLimits {
        self.limits
    }

    /// Opens the invocation capability and requires it to be `effect_pending`.
    ///
    /// A settled invocation reports [`InvocationError::OutcomeKnown`], which is
    /// the fail-closed signal a replayed tool needs: the outcome exists, so the
    /// tool must neither re-run nor read memos.
    pub fn open(
        self: &Arc<Self>,
        scope: InvocationScope,
    ) -> Result<InvocationHandle, InvocationError> {
        let mut state = self.lock_state();
        if let Some(record) = state.invocations.get(&scope) {
            if record.state != InvocationState::EffectPending {
                return Err(InvocationError::OutcomeKnown(scope.display()));
            }
            return Ok(InvocationHandle {
                store: Arc::clone(self),
                scope,
                generation: record.generation,
            });
        }
        let live = state
            .invocations
            .values()
            .filter(|record| record.state == InvocationState::EffectPending)
            .count();
        if live >= self.limits.max_live_invocations {
            return Err(InvocationError::BoundExceeded(format!(
                "{} unsettled invocations retained (limit {})",
                live, self.limits.max_live_invocations
            )));
        }
        state.invocations.insert(
            scope.clone(),
            InvocationRecord {
                generation: 0,
                state: InvocationState::EffectPending,
                values: BTreeMap::new(),
            },
        );
        Ok(InvocationHandle {
            store: Arc::clone(self),
            scope,
            generation: 0,
        })
    }

    /// Durable state of one invocation, if it is known.
    pub fn state(&self, scope: &InvocationScope) -> Option<InvocationState> {
        self.lock_state()
            .invocations
            .get(scope)
            .map(|record| record.state)
    }

    /// Durable values of one invocation, as `(address, value)` pairs. Intended
    /// for tests, telemetry, and host inspection; tools use a handle.
    pub fn stored_values(&self, scope: &InvocationScope) -> Vec<(String, String)> {
        self.lock_state()
            .invocations
            .get(scope)
            .map(|record| {
                record
                    .values
                    .iter()
                    .map(|(address, value)| (address.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Total bytes currently retained across all live values.
    pub fn retained_value_bytes(&self) -> usize {
        self.lock_state()
            .invocations
            .values()
            .map(|record| record.values.values().map(String::len).sum::<usize>())
            .sum()
    }

    /// Recovers an orphaned `effect_pending` invocation whose outcome is unknown
    /// (Pi's unsafe recovery): preserve the bounded snapshot, append the
    /// mandatory marker, and delete partial output and memos atomically.
    ///
    /// Never infers success from partial output, and refuses a second recovery
    /// because the first one already made the outcome known.
    pub fn recover_unsafe_orphan(
        self: &Arc<Self>,
        scope: InvocationScope,
    ) -> Result<UnsafeRecovery, InvocationError> {
        let handle = self.open(scope)?;
        let partial_output = handle.partial_output()?;
        let settlement = handle.settle()?;
        Ok(UnsafeRecovery {
            invocation: synthesize_interruption(partial_output.as_deref()),
            settlement,
        })
    }

    fn lock_state(&self) -> MutexGuard<'_, StoreState> {
        // A poisoned lock only means some other invocation panicked; the store
        // is plain data, so keep serving it rather than disabling durability.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn settle(&self, scope: &InvocationScope) -> Result<Settlement, InvocationError> {
        let mut state = self.lock_state();
        let record = state
            .invocations
            .get_mut(scope)
            .ok_or_else(|| InvocationError::OutcomeKnown(scope.display()))?;
        if record.state != InvocationState::EffectPending {
            return Err(InvocationError::OutcomeKnown(scope.display()));
        }
        let deleted_values: Vec<String> = record.values.keys().cloned().collect();
        record.values.clear();
        record.state = InvocationState::OutcomeReady;
        record.generation = record.generation.saturating_add(1);
        let settlement = Settlement {
            scope: scope.clone(),
            generation: record.generation,
            deleted_values,
        };
        state.settled.push_back(scope.clone());
        while state.settled.len() > self.limits.max_settled_invocations {
            if let Some(evicted) = state.settled.pop_front() {
                state.invocations.remove(&evicted);
            }
        }
        Ok(settlement)
    }
}

/// Capability for one `effect_pending` invocation.
///
/// Every method fences on the invocation still being `effect_pending` at the
/// generation the handle was opened with, so a late callback from a settled or
/// replayed invocation is refused instead of reviving state.
#[derive(Clone, Debug)]
pub struct InvocationHandle {
    store: Arc<DurableInvocationStore>,
    scope: InvocationScope,
    generation: u64,
}

impl InvocationHandle {
    /// The invocation this capability addresses.
    pub fn scope(&self) -> &InvocationScope {
        &self.scope
    }

    /// Generation the capability was opened with.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Records one memo. The name must be non-empty and contain no `:`.
    pub fn set_memo(&self, name: &str, value: serde_json::Value) -> Result<(), InvocationError> {
        let address = self.scope.memo_address(name)?;
        let encoded = serde_json::to_string(&value)
            .map_err(|error| InvocationError::Corrupt(error.to_string()))?;
        self.replace_value(&address, encoded)
    }

    /// Reads one memo. `Ok(None)` means "not recorded for a live invocation";
    /// a settled invocation reports [`InvocationError::OutcomeKnown`] instead.
    pub fn get_memo(&self, name: &str) -> Result<Option<serde_json::Value>, InvocationError> {
        let address = self.scope.memo_address(name)?;
        let Some(encoded) = self.read_value(&address)? else {
            return Ok(None);
        };
        let value = serde_json::from_str(&encoded)
            .map_err(|error| InvocationError::Corrupt(error.to_string()))?;
        Ok(Some(value))
    }

    /// Replay decision for one memoized step (Pi's `step.do`).
    ///
    /// Only [`MemoLookup::NotYetRecorded`] authorizes running the effect, and it
    /// is returned solely for a live `effect_pending` invocation.
    pub fn replay_lookup(&self, name: &str) -> Result<MemoLookup, InvocationError> {
        match self.get_memo(name)? {
            Some(value) => Ok(MemoLookup::Memoized(value)),
            None => Ok(MemoLookup::NotYetRecorded),
        }
    }

    /// Runs `effect` at most once per invocation and records its value.
    ///
    /// Mirrors Pi's `step.do`: a recorded step returns its memo, an unrecorded
    /// step runs now and commits the memo. On a settled invocation this fails
    /// before `effect` is called, so replay can never mistake an expired
    /// capability for "not yet executed".
    pub fn replay_step<T>(
        &self,
        name: &str,
        effect: impl FnOnce() -> T,
    ) -> Result<T, InvocationError>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        match self.replay_lookup(name)? {
            MemoLookup::Memoized(value) => serde_json::from_value(value)
                .map_err(|error| InvocationError::Corrupt(error.to_string())),
            MemoLookup::NotYetRecorded => {
                let produced = effect();
                let value = serde_json::to_value(&produced)
                    .map_err(|error| InvocationError::Corrupt(error.to_string()))?;
                self.set_memo(name, value)?;
                Ok(produced)
            }
        }
    }

    /// Deletes one memo, exactly like `setMemo(name, undefined)` in Pi.
    pub fn clear_memo(&self, name: &str) -> Result<(), InvocationError> {
        let address = self.scope.memo_address(name)?;
        self.delete_value(&address)
    }

    /// Replaces this invocation's durable partial-output snapshot.
    ///
    /// The store keeps exactly one value per invocation, so this is a
    /// replacement rather than an append: retained progress state stays bounded
    /// by one snapshot no matter how long the command runs.
    pub fn replace_partial_output(&self, snapshot: &str) -> Result<(), InvocationError> {
        self.replace_value(&self.scope.partial_output_address(), snapshot.to_owned())
    }

    /// The durable partial-output snapshot, if the tool ever requested one.
    pub fn partial_output(&self) -> Result<Option<String>, InvocationError> {
        self.read_value(&self.scope.partial_output_address())
    }

    /// Settles the invocation: fences every handle and deletes all values.
    pub fn settle(&self) -> Result<Settlement, InvocationError> {
        // Fence on the caller's own generation before mutating, so a handle
        // from a previous generation cannot settle a replayed invocation.
        {
            let state = self.store.lock_state();
            let record = state
                .invocations
                .get(&self.scope)
                .ok_or_else(|| InvocationError::OutcomeKnown(self.scope.display()))?;
            if record.generation != self.generation {
                return Err(InvocationError::Fenced {
                    scope: self.scope.display(),
                    expected: self.generation,
                    actual: record.generation,
                });
            }
        }
        self.store.settle(&self.scope)
    }

    fn replace_value(&self, address: &str, value: String) -> Result<(), InvocationError> {
        if value.len() > self.store.limits.max_value_bytes {
            return Err(InvocationError::BoundExceeded(format!(
                "{address} is {} bytes (limit {})",
                value.len(),
                self.store.limits.max_value_bytes
            )));
        }
        self.with_live_record(|record| {
            let replacing = record.values.contains_key(address);
            if !replacing && record.values.len() >= self.store.limits.max_values_per_invocation {
                return Err(InvocationError::BoundExceeded(format!(
                    "{} holds {} values (limit {})",
                    self.scope.display(),
                    record.values.len(),
                    self.store.limits.max_values_per_invocation
                )));
            }
            record.values.insert(address.to_owned(), value);
            Ok(())
        })
    }

    fn read_value(&self, address: &str) -> Result<Option<String>, InvocationError> {
        self.with_live_record(|record| Ok(record.values.get(address).cloned()))
    }

    fn delete_value(&self, address: &str) -> Result<(), InvocationError> {
        self.with_live_record(|record| {
            record.values.remove(address);
            Ok(())
        })
    }

    fn with_live_record<T>(
        &self,
        apply: impl FnOnce(&mut InvocationRecord) -> Result<T, InvocationError>,
    ) -> Result<T, InvocationError> {
        let mut state = self.store.lock_state();
        let record =
            state
                .invocations
                .get_mut(&self.scope)
                .ok_or_else(|| InvocationError::Fenced {
                    scope: self.scope.display(),
                    expected: self.generation,
                    actual: u64::MAX,
                })?;
        // Check settlement before the generation: a late callback after the
        // outcome became known must be told the capability expired, which is the
        // diagnostic that explains why its memo is gone.
        if record.state != InvocationState::EffectPending {
            return Err(InvocationError::OutcomeKnown(self.scope.display()));
        }
        if record.generation != self.generation {
            return Err(InvocationError::Fenced {
                scope: self.scope.display(),
                expected: self.generation,
                actual: record.generation,
            });
        }
        apply(record)
    }
}

impl PartialOutputCheckpointSink for InvocationHandle {
    fn checkpoint_partial_output(&self, snapshot: &str) -> Result<(), ToolError> {
        self.replace_partial_output(snapshot)
            .map_err(ToolError::from)
    }
}
