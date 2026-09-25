//! Deferred provider responses: durable suspend/resume, handles, and poll permits.
//!
//! A provider may answer a generation request with "not finished yet, here is a
//! handle". Pi turns that into a durably suspended run rather than an in-process
//! wait (`packages/agent/docs/harness.md` §3.10, `docs/harness.md` §4.5, and
//! `packages/agent/src/harness/runtime/drive/deferred.ts`):
//!
//! * a valid handle parks the run at the `deferred.suspended` leaf, emitting
//!   `run_suspend{reason:"deferred",deferred,poll}`;
//! * an **invalid** handle is a terminal failure ("Provider returned an invalid
//!   deferred handle"), never a suspension;
//! * the provider is polled again only when the driving pass carries a deferred
//!   poll permit — `run_resume` is emitted and the permit is consumed at most
//!   once per pass;
//! * a poll that is admitted but whose outcome becomes unknown leaves a
//!   `deferred.effect_pending` leaf. That leaf is **not** re-polled
//!   automatically: a new billable poll may only replace the unknown one after
//!   an explicit [`DeferredResumeIntent::ReplaceUnknownPoll`] decision, and the
//!   replacement then uses **fresh** ids at the same poll number and deletes the
//!   abandoned stream frame list, so a duplicate partial response can never be
//!   merged into the run;
//! * a poll that returns another deferred response goes back to
//!   `deferred.suspended` at the same poll number (a poll is not a new request).
//!
//! This module implements the durable decision core of that lifecycle. It is
//! host-runtime independent: it does not own the event stream, the provider, or
//! the session, and it never performs I/O. Every refusal fails closed — a stale,
//! duplicate, foreign, or expired poll is reported as a refusal rather than
//! silently degrading into "still suspended", because treating it as waiting
//! would leave a run parked forever or, worse, admit a second poll for the same
//! permit.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::session_writer::SessionWriter;

/// Identity of the model captured in a run's durable configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelIdentity {
    /// Provider id.
    pub provider: String,
    /// Provider-local model id.
    pub model_id: String,
}

impl ModelIdentity {
    /// Constructs a model identity.
    pub fn new(provider: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model_id: model_id.into(),
        }
    }
}

/// Provider handle for one deferred response (pi-ai `DeferredHandle`).
///
/// `data` is opaque provider conversion material required to reconstruct the
/// final message; like `octet_ai::deferred::DeferredHandle` it is never
/// included in `Debug`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredHandle {
    /// Provider id that owns the handle.
    pub provider: String,
    /// Provider-local model id that owns the handle.
    pub model_id: String,
    /// API id that produced the response carrying this handle.
    pub api: String,
    /// Provider token: a response id, or a batch id plus row id.
    pub id: String,
    /// Absolute expiry, in milliseconds since the Unix epoch, when the provider
    /// supplies one.
    pub expires_at_ms: Option<i64>,
    /// Provider-suggested minimum delay before the next poll.
    pub poll_after_ms: Option<u64>,
    /// Provider conversion data required to reconstruct the final message.
    pub data: Option<serde_json::Value>,
}

impl std::fmt::Debug for DeferredHandle {
    /// Mirrors `octet_ai::deferred::DeferredHandle`: the opaque provider
    /// conversion data is redacted, so a durable handle can be logged or
    /// rendered in a diagnostic without leaking provider material. The two
    /// handle types stay separate on purpose (transport shape vs durable
    /// decision-core shape); only the redaction rule is shared.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeferredHandle")
            .field("provider", &self.provider)
            .field("model_id", &self.model_id)
            .field("api", &self.api)
            .field("id", &self.id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("poll_after_ms", &self.poll_after_ms)
            .field("data", &self.data.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl DeferredHandle {
    /// Builds a handle with only the required fields.
    pub fn new(
        provider: impl Into<String>,
        model_id: impl Into<String>,
        api: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model_id: model_id.into(),
            api: api.into(),
            id: id.into(),
            expires_at_ms: None,
            poll_after_ms: None,
            data: None,
        }
    }

    /// Why this handle cannot be used for `identity`/`response_api`, if any.
    ///
    /// Mirrors Pi's `deferredHandleIsValid`: the id must be non-empty, the
    /// provider and model id must equal the run's configured identity, and the
    /// api must equal the api of the response that produced the handle.
    pub fn rejection(
        &self,
        identity: &ModelIdentity,
        response_api: &str,
    ) -> Option<DeferredHandleRejection> {
        if self.id.is_empty() {
            return Some(DeferredHandleRejection::EmptyId);
        }
        if self.api.is_empty() {
            return Some(DeferredHandleRejection::EmptyApi);
        }
        if self.provider != identity.provider || self.model_id != identity.model_id {
            return Some(DeferredHandleRejection::ForeignProvider {
                configured: identity.clone(),
                handle: ModelIdentity::new(self.provider.clone(), self.model_id.clone()),
            });
        }
        if self.api != response_api {
            return Some(DeferredHandleRejection::ForeignApi {
                configured: response_api.to_owned(),
                handle: self.api.clone(),
            });
        }
        None
    }

    /// Whether the provider's absolute expiry has passed at `now_ms`.
    pub fn is_expired_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_some_and(|expiry| expiry <= now_ms)
    }
}

/// Why a provider's deferred handle cannot be trusted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferredHandleRejection {
    /// The provider reported a deferred response without a handle.
    Absent,
    /// The handle carried an empty provider token.
    EmptyId,
    /// The handle carried an empty api identifier.
    EmptyApi,
    /// Provider or model id does not match the run's durable configuration.
    ForeignProvider {
        /// Identity captured in the run configuration.
        configured: ModelIdentity,
        /// Identity the handle claims.
        handle: ModelIdentity,
    },
    /// The handle's api does not match the response that carried it.
    ForeignApi {
        /// Api id of the response that carried the handle.
        configured: String,
        /// Api id the handle claims.
        handle: String,
    },
}

impl std::fmt::Display for DeferredHandleRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => write!(f, "the response carried no deferred handle"),
            Self::EmptyId => write!(f, "the deferred handle has an empty provider id"),
            Self::EmptyApi => write!(f, "the deferred handle has an empty api id"),
            Self::ForeignProvider { configured, handle } => write!(
                f,
                "the deferred handle belongs to {}/{} but the run is configured for {}/{}",
                handle.provider, handle.model_id, configured.provider, configured.model_id
            ),
            Self::ForeignApi { configured, handle } => write!(
                f,
                "the deferred handle is for api {handle} but the response used api {configured}"
            ),
        }
    }
}

/// Normalized stop reason of one assistant or deferred-poll response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredStopReason {
    /// The provider parked the request and returned a handle.
    Deferred,
    /// The provider finished the request.
    Settled,
    /// The provider returned an error stop reason.
    Failed,
    /// The request was aborted or cancelled.
    Aborted,
}

/// The parts of one response needed to classify a suspension.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredResponseDeclaration {
    /// Normalized stop reason.
    pub stop_reason: DeferredStopReason,
    /// Api id that produced the response.
    pub api: String,
    /// The handle, when the provider supplied one.
    pub handle: Option<DeferredHandle>,
}

/// The exact user-visible diagnostic Pi uses for an untrustworthy handle.
pub const INVALID_DEFERRED_HANDLE_DIAGNOSTIC: &str = "Provider returned an invalid deferred handle";

/// Why a deferred response could not be suspended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferredSuspendFailureKind {
    /// The handle was absent, empty, foreign, or for the wrong api.
    MalformedHandle(DeferredHandleRejection),
    /// The provider returned an error stop reason.
    ProviderRejected,
    /// The request was aborted.
    Aborted,
}

/// A terminal refusal to suspend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredSuspendFailure {
    /// Classified cause.
    pub kind: DeferredSuspendFailureKind,
    /// Diagnostic for the model/host; malformed handles use Pi's exact wording
    /// plus the specific rejection so the fault is actionable.
    pub diagnostic: String,
}

/// Classification of one deferred response.
#[derive(Clone, Debug, PartialEq)]
pub enum DeferredSuspendDecision {
    /// Park the run at the `deferred.suspended` leaf.
    Suspended(Box<DeferredSuspended>),
    /// The response finished the request; the ordinary settlement path applies.
    Settled,
    /// Terminal failure; the run must not suspend and must not fabricate a result.
    Failed(Box<DeferredSuspendFailure>),
}

/// Durable phase of a suspended deferred run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum DeferredPhase {
    /// `deferred.suspended`: no poll outcome is unknown; a permitted poll
    /// increments the poll number.
    Suspended,
    /// `deferred.effect_pending`: a poll was admitted and its outcome is unknown.
    /// An explicit [`DeferredResumeIntent::ReplaceUnknownPoll`] replaces it under
    /// fresh ids at the same poll number; a plain poll is refused.
    EffectPending {
        /// Reserved response entry id of the unknown poll.
        response_id: String,
        /// Reserved usage id of the unknown poll.
        usage_id: String,
    },
}

/// The durable `deferred.suspended` / `deferred.effect_pending` leaf.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredSuspended {
    /// Owning operation.
    pub operation_id: String,
    /// Assistant entry whose response most recently carried the handle.
    pub source_entry_id: String,
    /// Model identity captured in the durable run configuration.
    pub identity: ModelIdentity,
    /// Api id of the response that produced the current handle.
    pub response_api: String,
    /// Poll number: `0` after the first suspend, incremented only when a
    /// permitted poll is admitted from [`DeferredPhase::Suspended`].
    pub poll: u64,
    /// Which deferred phase the run is parked in.
    pub phase: DeferredPhase,
    /// Provider handle to poll.
    pub handle: DeferredHandle,
    /// Durable generation of this leaf, incremented on every durable change.
    /// A poll permit minted for an older generation is stale.
    pub generation: u64,
}

impl DeferredSuspended {
    /// Observation of the parked run: what a caller without a permit may see.
    pub fn observation(&self) -> SuspendedRunObservation {
        SuspendedRunObservation {
            operation_id: self.operation_id.clone(),
            handle: self.handle.clone(),
            poll: self.poll,
            phase: self.phase.clone(),
        }
    }
}

/// Concretely observable suspension state (Pi's `SuspendedRun`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuspendedRunObservation {
    /// Owning operation.
    pub operation_id: String,
    /// Handle the provider must be polled with.
    pub handle: DeferredHandle,
    /// Current poll number.
    pub poll: u64,
    /// Current durable phase.
    pub phase: DeferredPhase,
}

/// Classifies one response as a durable suspension, ordinary settlement, or a
/// terminal failure.
///
/// `identity` is the run's durable configuration and `source_entry_id` is the
/// assistant entry that carried the response; both are recorded on the leaf so a
/// later poll can be validated against them.
pub fn suspend_deferred_response(
    identity: &ModelIdentity,
    operation_id: &str,
    source_entry_id: &str,
    declaration: DeferredResponseDeclaration,
) -> DeferredSuspendDecision {
    match declaration.stop_reason {
        DeferredStopReason::Settled => DeferredSuspendDecision::Settled,
        DeferredStopReason::Failed => {
            DeferredSuspendDecision::Failed(Box::new(DeferredSuspendFailure {
                kind: DeferredSuspendFailureKind::ProviderRejected,
                diagnostic: "provider returned an error result".to_owned(),
            }))
        }
        DeferredStopReason::Aborted => {
            DeferredSuspendDecision::Failed(Box::new(DeferredSuspendFailure {
                kind: DeferredSuspendFailureKind::Aborted,
                diagnostic: "deferred request was cancelled".to_owned(),
            }))
        }
        DeferredStopReason::Deferred => {
            let rejection = match declaration.handle.as_ref() {
                None => Some(DeferredHandleRejection::Absent),
                Some(handle) => handle.rejection(identity, &declaration.api),
            };
            match (rejection, declaration.handle) {
                (None, Some(handle)) => {
                    DeferredSuspendDecision::Suspended(Box::new(DeferredSuspended {
                        operation_id: operation_id.to_owned(),
                        source_entry_id: source_entry_id.to_owned(),
                        identity: identity.clone(),
                        response_api: declaration.api,
                        poll: 0,
                        phase: DeferredPhase::Suspended,
                        handle,
                        generation: 0,
                    }))
                }
                (Some(rejection), _) => {
                    DeferredSuspendDecision::Failed(Box::new(DeferredSuspendFailure {
                        kind: DeferredSuspendFailureKind::MalformedHandle(rejection.clone()),
                        diagnostic: format!("{INVALID_DEFERRED_HANDLE_DIAGNOSTIC}: {rejection}"),
                    }))
                }
                (None, None) => unreachable!("absent handle is always a rejection"),
            }
        }
    }
}

/// At most one deferred poll permit per driving pass.
///
/// A permit is minted for one durable leaf generation, is consumed at most once,
/// and cannot be re-minted by the tool or provider. `none` is the honest
/// representation of a pass that carries no permit at all. Only a permit minted
/// with [`Self::one_replacing_unknown`] may replace a poll whose outcome is
/// unknown: every other permit refuses instead of spending a second billable
/// poll on an effect that may already have been accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredPollPermit {
    pass_id: String,
    generation: u64,
    remaining: u32,
    consumed: bool,
    replace_unknown: bool,
}

impl DeferredPollPermit {
    /// Grants the pass one deferred poll against `generation`.
    pub fn one(pass_id: impl Into<String>, generation: u64) -> Self {
        Self {
            pass_id: pass_id.into(),
            generation,
            remaining: 1,
            consumed: false,
            replace_unknown: false,
        }
    }

    /// Grants the pass one poll and the explicit right to replace a poll whose
    /// outcome is unknown.
    ///
    /// A caller may mint this only after an explicit user decision to spend a
    /// new billable poll on the same effect; the abandoned poll's exposure is
    /// recorded by the caller that drives the replacement.
    pub fn one_replacing_unknown(pass_id: impl Into<String>, generation: u64) -> Self {
        Self {
            replace_unknown: true,
            ..Self::one(pass_id, generation)
        }
    }

    /// A pass that carries no poll permit.
    pub fn none(pass_id: impl Into<String>, generation: u64) -> Self {
        Self {
            pass_id: pass_id.into(),
            generation,
            remaining: 0,
            consumed: false,
            replace_unknown: false,
        }
    }

    /// Identifier of the driving pass.
    pub fn pass_id(&self) -> &str {
        &self.pass_id
    }

    /// Durable leaf generation the permit was minted for.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Permits still available to this pass.
    pub fn remaining(&self) -> u32 {
        self.remaining
    }

    /// Whether this permit already admitted a poll.
    pub fn is_consumed(&self) -> bool {
        self.consumed
    }
}

/// Why a poll was refused. Refusals never fall back to "waiting".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferredPollRefusalKind {
    /// The permit was minted for a different (older or newer) durable leaf.
    StalePermit {
        /// Generation carried by the permit.
        permit: u64,
        /// Generation currently durable.
        leaf: u64,
    },
    /// The same permit already admitted a poll; a second one would double-poll.
    AlreadyConsumed,
    /// The handle does not belong to this run's configuration or api.
    ForeignHandle(DeferredHandleRejection),
    /// The provider's absolute expiry has passed.
    ExpiredHandle {
        /// Expiry the provider supplied.
        expires_at_ms: i64,
    },
    /// The durable leaf owns an admitted poll whose outcome is unknown. Spending
    /// another billable poll on the same effect requires an explicit
    /// [`DeferredResumeIntent::ReplaceUnknownPoll`]; a plain permit fails
    /// closed instead of auto-replacing it.
    UnknownPollOutcome {
        /// Poll number whose outcome is unknown.
        poll: u64,
    },
}

/// A fail-closed refusal to poll.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredPollRefusal {
    /// Classified cause.
    pub kind: DeferredPollRefusalKind,
    /// Diagnostic for the host.
    pub diagnostic: String,
}

/// Replacement of one unknown-outcome poll under fresh ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownPollReplacement {
    /// Reserved response entry id of the abandoned poll.
    pub abandoned_response_id: String,
    /// Reserved usage id of the abandoned poll.
    pub abandoned_usage_id: String,
    /// Fresh response entry id for the replacement poll.
    pub replacement_response_id: String,
    /// Fresh usage id for the replacement poll.
    pub replacement_usage_id: String,
}

/// One admitted deferred poll.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredPollIntent {
    /// Poll number the provider is being polled for.
    pub poll: u64,
    /// Durable phase to record before the poll is performed.
    pub phase: DeferredPhase,
    /// Present when this poll replaces an unknown-outcome poll.
    pub discard_unknown_poll: Option<UnknownPollReplacement>,
    /// Whether the pass must emit `run_resume`.
    pub resume_event: bool,
    /// Handle to poll.
    pub handle: DeferredHandle,
    /// Driving pass that owns the permit.
    pub pass_id: String,
    /// Durable leaf the intent was prepared against.
    pub generation: u64,
}

/// Result of preparing a deferred poll in one pass.
#[derive(Clone, Debug, PartialEq)]
pub enum DeferredPollPreparation {
    /// The pass carries no permit: the run stays durably suspended, nothing is
    /// written, and no provider work starts.
    Waiting(Box<SuspendedRunObservation>),
    /// Exactly one poll is admitted.
    Admitted(Box<DeferredPollIntent>),
    /// Fail closed: no provider work, no durable write, no fabricated settlement.
    Refused(Box<DeferredPollRefusal>),
}

/// Decides whether this pass may poll the provider, and with which durable intent.
///
/// `next_id` supplies fresh durable ids (monotonic inside a session); it is only
/// called when a poll is admitted, so a refused or waiting pass allocates
/// nothing.
pub fn prepare_deferred_poll(
    suspended: &DeferredSuspended,
    permit: &mut DeferredPollPermit,
    now_ms: i64,
    mut next_id: impl FnMut() -> String,
) -> DeferredPollPreparation {
    if permit.consumed {
        return DeferredPollPreparation::Refused(Box::new(DeferredPollRefusal {
            kind: DeferredPollRefusalKind::AlreadyConsumed,
            diagnostic: format!(
                "pass {} already consumed its deferred poll permit for generation {}",
                permit.pass_id, permit.generation
            ),
        }));
    }
    if permit.remaining == 0 {
        return DeferredPollPreparation::Waiting(Box::new(suspended.observation()));
    }
    if permit.generation != suspended.generation {
        return DeferredPollPreparation::Refused(Box::new(DeferredPollRefusal {
            kind: DeferredPollRefusalKind::StalePermit {
                permit: permit.generation,
                leaf: suspended.generation,
            },
            diagnostic: format!(
                "deferred poll permit for generation {} is stale; the durable leaf is generation {}",
                permit.generation, suspended.generation
            ),
        }));
    }
    if let Some(rejection) = suspended
        .handle
        .rejection(&suspended.identity, &suspended.response_api)
    {
        return DeferredPollPreparation::Refused(Box::new(DeferredPollRefusal {
            kind: DeferredPollRefusalKind::ForeignHandle(rejection.clone()),
            diagnostic: format!("refusing to poll a foreign deferred handle: {rejection}"),
        }));
    }
    if let Some(expires_at_ms) = suspended.handle.expires_at_ms {
        if suspended.handle.is_expired_at(now_ms) {
            return DeferredPollPreparation::Refused(Box::new(DeferredPollRefusal {
                kind: DeferredPollRefusalKind::ExpiredHandle { expires_at_ms },
                diagnostic: format!(
                    "deferred handle expired at {expires_at_ms} ms (now {now_ms} ms)"
                ),
            }));
        }
    }
    // An unknown-outcome poll may already have been accepted and billed. It is
    // never replaced automatically: only a permit the caller minted with
    // `one_replacing_unknown` (an explicit resume decision) may spend a second
    // billable poll on the same effect.
    if matches!(suspended.phase, DeferredPhase::EffectPending { .. }) && !permit.replace_unknown {
        return DeferredPollPreparation::Refused(Box::new(DeferredPollRefusal {
            kind: DeferredPollRefusalKind::UnknownPollOutcome {
                poll: suspended.poll,
            },
            diagnostic: format!(
                "deferred poll {} has an unknown outcome; resuming it needs an explicit replacement decision",
                suspended.poll
            ),
        }));
    }

    permit.remaining -= 1;
    permit.consumed = true;

    let response_id = next_id();
    let usage_id = next_id();
    let (poll, discard_unknown_poll) = match &suspended.phase {
        DeferredPhase::Suspended => (suspended.poll.saturating_add(1), None),
        DeferredPhase::EffectPending {
            response_id: abandoned_response_id,
            usage_id: abandoned_usage_id,
        } => (
            suspended.poll,
            Some(UnknownPollReplacement {
                abandoned_response_id: abandoned_response_id.clone(),
                abandoned_usage_id: abandoned_usage_id.clone(),
                replacement_response_id: response_id.clone(),
                replacement_usage_id: usage_id.clone(),
            }),
        ),
    };
    DeferredPollPreparation::Admitted(Box::new(DeferredPollIntent {
        poll,
        phase: DeferredPhase::EffectPending {
            response_id,
            usage_id,
        },
        discard_unknown_poll,
        resume_event: true,
        handle: suspended.handle.clone(),
        pass_id: permit.pass_id.clone(),
        generation: suspended.generation,
    }))
}

/// Outcome of performing one admitted deferred poll.
#[derive(Clone, Debug, PartialEq)]
pub enum DeferredPollOutcome {
    /// The provider is still working and returned a (possibly new) handle.
    StillDeferred(DeferredHandle),
    /// The provider finished the request.
    Settled,
    /// The provider returned an error for this poll.
    Failed {
        /// Error text for the terminal diagnostic.
        message: String,
    },
}

/// What the durable leaf becomes after a poll.
#[derive(Clone, Debug, PartialEq)]
pub enum DeferredResume {
    /// Parked again at `deferred.suspended`, at the same poll number.
    Suspended(Box<DeferredSuspended>),
    /// The poll finished the request.
    Settled,
    /// Terminal failure; the run must not poll again.
    Failed(Box<DeferredSuspendFailure>),
}

impl DeferredSuspended {
    /// Applies one poll outcome to this leaf, producing the next durable state.
    ///
    /// A poll is **not** a new request: a still-deferred answer returns to
    /// `deferred.suspended` with the same poll number and a bumped generation
    /// (so the consumed permit cannot be reused), and a new invalid handle fails
    /// closed exactly like a malformed handle at suspend time.
    pub fn resume_after_poll(
        &self,
        intent: &DeferredPollIntent,
        outcome: DeferredPollOutcome,
    ) -> DeferredResume {
        let response_entry_id = match &intent.phase {
            DeferredPhase::EffectPending { response_id, .. } => response_id.clone(),
            // An admitted poll always records an effect-pending phase before it
            // runs; anything else is a programming error, and failing closed is
            // still safer than ignoring the poll.
            DeferredPhase::Suspended => self.source_entry_id.clone(),
        };
        match outcome {
            DeferredPollOutcome::Settled => DeferredResume::Settled,
            DeferredPollOutcome::Failed { message } => {
                DeferredResume::Failed(Box::new(DeferredSuspendFailure {
                    kind: DeferredSuspendFailureKind::ProviderRejected,
                    diagnostic: format!("deferred poll {} failed: {message}", intent.poll),
                }))
            }
            DeferredPollOutcome::StillDeferred(handle) => {
                let rejection = handle.rejection(&self.identity, &intent.handle.api);
                if let Some(rejection) = rejection {
                    return DeferredResume::Failed(Box::new(DeferredSuspendFailure {
                        kind: DeferredSuspendFailureKind::MalformedHandle(rejection.clone()),
                        diagnostic: format!("{INVALID_DEFERRED_HANDLE_DIAGNOSTIC}: {rejection}"),
                    }));
                }
                let api = handle.api.clone();
                DeferredResume::Suspended(Box::new(DeferredSuspended {
                    operation_id: self.operation_id.clone(),
                    source_entry_id: response_entry_id,
                    identity: self.identity.clone(),
                    response_api: api,
                    poll: intent.poll,
                    phase: DeferredPhase::Suspended,
                    handle,
                    generation: self.generation.saturating_add(1),
                }))
            }
        }
    }
}

// ── Durable suspended-run lifecycle ─────────────────────────────────────────
//
// The decision core above is host-runtime independent. This section makes it
// durable: a suspended run is a replaceable session record keyed by operation,
// every durable change moves the leaf's generation by exactly one, and only the
// pass that prepared against the current generation may write the next one.
// That single fence is what makes the poll permit one-owner: a stale, duplicate,
// foreign, or expired poll is refused before any provider work, and a crash
// between "poll admitted" and "outcome known" leaves a durable
// `deferred.effect_pending` record whose replacement requires an explicit
// [`DeferredResumeIntent::ReplaceUnknownPoll`] decision and then uses fresh
// reserved ids, so one billable poll can never be merged into the run twice.

/// Hard bounds for the durable deferred-run store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeferredRunLimits {
    /// Maximum bytes of one operation id or reserved durable id.
    pub max_operation_bytes: usize,
    /// Maximum distinct operations retained, including terminal tombstones.
    pub max_runs: usize,
    /// Maximum bytes of one provider handle's opaque `data` value.
    pub max_handle_data_bytes: usize,
}

impl Default for DeferredRunLimits {
    fn default() -> Self {
        Self {
            max_operation_bytes: 256,
            max_runs: 1024,
            max_handle_data_bytes: 64 * 1024,
        }
    }
}

/// Why a durable deferred-run store mutation was refused. Every variant fails
/// closed: the caller must not poll, settle, or overwrite on an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeferredRunError {
    /// No durable leaf exists for the operation.
    UnknownOperation(String),
    /// The caller prepared against a generation that is no longer durable.
    StaleGeneration {
        /// Operation whose leaf moved.
        operation: String,
        /// Generation the caller prepared against.
        expected: u64,
        /// Generation currently durable.
        actual: u64,
    },
    /// A write attempted to move the durable leaf backwards or sideways.
    GenerationRegression {
        /// Operation whose leaf would move.
        operation: String,
        /// Generation currently durable.
        previous: u64,
        /// Generation the write carried.
        next: u64,
    },
    /// The operation already has a terminal outcome.
    OutcomeKnown(String),
    /// The owning session closed; no further durable change is possible.
    Closed,
    /// A hard storage bound would be exceeded.
    BoundExceeded(String),
    /// A persisted record is not a valid leaf.
    Corrupt(String),
    /// The durable append failed; nothing was acknowledged.
    Persistence(String),
}

impl std::fmt::Display for DeferredRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownOperation(operation) => {
                write!(f, "no durable deferred run exists for operation {operation}")
            }
            Self::StaleGeneration {
                operation,
                expected,
                actual,
            } => write!(
                f,
                "deferred run {operation} moved from generation {expected} to {actual}; the pass is fenced"
            ),
            Self::GenerationRegression {
                operation,
                previous,
                next,
            } => write!(
                f,
                "deferred run {operation} cannot move from generation {previous} to {next}"
            ),
            Self::OutcomeKnown(operation) => write!(
                f,
                "deferred run {operation} already has a terminal outcome"
            ),
            Self::Closed => write!(f, "deferred-run store is closed"),
            Self::BoundExceeded(detail) => write!(f, "deferred-run bound exceeded: {detail}"),
            Self::Corrupt(detail) => write!(f, "deferred-run record is corrupt: {detail}"),
            Self::Persistence(detail) => write!(f, "deferred-run persistence failed: {detail}"),
        }
    }
}

impl std::error::Error for DeferredRunError {}

/// Durable state of one deferred run leaf.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeferredRunState {
    /// Parked at `deferred.suspended`; a permitted poll increments the poll
    /// number.
    Suspended {
        /// The durable leaf, including the handle to poll.
        leaf: Box<DeferredSuspended>,
    },
    /// Parked at `deferred.effect_pending`: a poll was admitted and its outcome
    /// is unknown. Only an explicit
    /// [`DeferredResumeIntent::ReplaceUnknownPoll`] pass replaces it, under
    /// fresh reserved ids at the same poll number; a plain poll is refused.
    EffectPending {
        /// The durable leaf, including the reserved response and usage ids.
        leaf: Box<DeferredSuspended>,
    },
    /// Terminal: the admitted poll settled these reserved durable ids.
    Settled {
        /// Reserved response entry id consumed by the committed response.
        response_id: String,
        /// Reserved usage id consumed by the committed usage record.
        usage_id: String,
    },
    /// Terminal: the run was cancelled before an admitted poll settled. No
    /// later pass may poll it.
    Cancelled,
    /// Terminal: the admitted poll failed; the diagnostic is bounded host
    /// metadata, never provider prose.
    Failed {
        /// Bounded host diagnostic for the terminal failure.
        diagnostic: String,
    },
}

/// One durable deferred-run record, keyed by operation id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeferredRunRecord {
    /// Operation whose provider request is parked.
    pub operation_id: String,
    /// Durable generation of this record; every durable change moves it by
    /// exactly one.
    pub generation: u64,
    /// Durable state of the leaf.
    pub state: DeferredRunState,
}

impl DeferredRunRecord {
    /// Builds a `deferred.suspended` record from a classified suspension.
    pub fn suspended(leaf: DeferredSuspended) -> Result<Self, DeferredRunError> {
        if leaf.phase != DeferredPhase::Suspended {
            return Err(DeferredRunError::Corrupt(
                "a suspended record requires the suspended phase".into(),
            ));
        }
        Ok(Self {
            operation_id: leaf.operation_id.clone(),
            generation: leaf.generation,
            state: DeferredRunState::Suspended {
                leaf: Box::new(leaf),
            },
        })
    }

    /// Builds a `deferred.effect_pending` record for an admitted poll.
    pub fn effect_pending(leaf: DeferredSuspended) -> Result<Self, DeferredRunError> {
        if !matches!(leaf.phase, DeferredPhase::EffectPending { .. }) {
            return Err(DeferredRunError::Corrupt(
                "an effect-pending record requires the effect-pending phase".into(),
            ));
        }
        Ok(Self {
            operation_id: leaf.operation_id.clone(),
            generation: leaf.generation,
            state: DeferredRunState::EffectPending {
                leaf: Box::new(leaf),
            },
        })
    }

    /// Builds a terminal settled tombstone.
    pub fn settled(
        operation_id: impl Into<String>,
        generation: u64,
        response_id: impl Into<String>,
        usage_id: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            generation,
            state: DeferredRunState::Settled {
                response_id: response_id.into(),
                usage_id: usage_id.into(),
            },
        }
    }

    /// Builds a terminal cancellation tombstone.
    pub fn cancelled(operation_id: impl Into<String>, generation: u64) -> Self {
        Self {
            operation_id: operation_id.into(),
            generation,
            state: DeferredRunState::Cancelled,
        }
    }

    /// Builds a terminal failure tombstone.
    pub fn failed(
        operation_id: impl Into<String>,
        generation: u64,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            generation,
            state: DeferredRunState::Failed {
                diagnostic: diagnostic.into(),
            },
        }
    }

    /// The parked leaf, when this record is not terminal.
    pub fn leaf(&self) -> Option<&DeferredSuspended> {
        match &self.state {
            DeferredRunState::Suspended { leaf } | DeferredRunState::EffectPending { leaf } => {
                Some(leaf)
            }
            DeferredRunState::Settled { .. }
            | DeferredRunState::Cancelled
            | DeferredRunState::Failed { .. } => None,
        }
    }

    /// Observation of the parked leaf, when this record is not terminal.
    pub fn observation(&self) -> Option<SuspendedRunObservation> {
        self.leaf().map(DeferredSuspended::observation)
    }

    /// Whether the leaf reached a terminal state and may never poll again.
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self.state,
            DeferredRunState::Suspended { .. } | DeferredRunState::EffectPending { .. }
        )
    }

    /// Stable label for telemetry and diagnostics.
    pub fn state_label(&self) -> &'static str {
        match self.state {
            DeferredRunState::Suspended { .. } => "suspended",
            DeferredRunState::EffectPending { .. } => "effect_pending",
            DeferredRunState::Settled { .. } => "settled",
            DeferredRunState::Cancelled => "cancelled",
            DeferredRunState::Failed { .. } => "failed",
        }
    }

    /// Refuses a record that cannot describe a real deferred run.
    pub fn validate(&self, limits: &DeferredRunLimits) -> Result<(), DeferredRunError> {
        validate_deferred_segment(
            "operation id",
            &self.operation_id,
            limits.max_operation_bytes,
        )?;
        match &self.state {
            DeferredRunState::Suspended { leaf } => {
                validate_deferred_leaf(leaf, limits)?;
                if leaf.phase != DeferredPhase::Suspended || leaf.generation != self.generation {
                    return Err(DeferredRunError::Corrupt(
                        "suspended record phase or generation does not match its leaf".into(),
                    ));
                }
            }
            DeferredRunState::EffectPending { leaf } => {
                validate_deferred_leaf(leaf, limits)?;
                if !matches!(leaf.phase, DeferredPhase::EffectPending { .. })
                    || leaf.generation != self.generation
                {
                    return Err(DeferredRunError::Corrupt(
                        "effect-pending record phase or generation does not match its leaf".into(),
                    ));
                }
            }
            DeferredRunState::Settled {
                response_id,
                usage_id,
            } => {
                validate_deferred_segment("response id", response_id, limits.max_operation_bytes)?;
                validate_deferred_segment("usage id", usage_id, limits.max_operation_bytes)?;
            }
            DeferredRunState::Cancelled => {}
            DeferredRunState::Failed { diagnostic } => {
                if diagnostic.len() > limits.max_operation_bytes {
                    return Err(DeferredRunError::BoundExceeded(
                        "deferred failure diagnostic".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_deferred_segment(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), DeferredRunError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(DeferredRunError::Corrupt(format!(
            "{label} must be 1..={max_bytes} bytes"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/".contains(&byte))
    {
        return Err(DeferredRunError::Corrupt(format!(
            "{label} must be a bounded ASCII identifier"
        )));
    }
    Ok(())
}

/// Truncates one persisted diagnostic to a byte budget on a char boundary.
fn truncate_bounded(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn validate_deferred_leaf(
    leaf: &DeferredSuspended,
    limits: &DeferredRunLimits,
) -> Result<(), DeferredRunError> {
    validate_deferred_segment(
        "leaf operation id",
        &leaf.operation_id,
        limits.max_operation_bytes,
    )?;
    validate_deferred_segment(
        "source entry id",
        &leaf.source_entry_id,
        limits.max_operation_bytes,
    )?;
    validate_deferred_segment(
        "deferred handle id",
        &leaf.handle.id,
        limits.max_operation_bytes,
    )?;
    if leaf.handle.id.is_empty() {
        return Err(DeferredRunError::Corrupt(
            "deferred handle id must be non-empty".into(),
        ));
    }
    for (label, value) in [
        ("provider", leaf.handle.provider.as_str()),
        ("model id", leaf.handle.model_id.as_str()),
        ("api", leaf.handle.api.as_str()),
    ] {
        if value.is_empty() || value.len() > limits.max_operation_bytes {
            return Err(DeferredRunError::Corrupt(format!(
                "deferred handle {label} must be 1..={} bytes",
                limits.max_operation_bytes
            )));
        }
    }
    if leaf.handle.provider != leaf.identity.provider
        || leaf.handle.model_id != leaf.identity.model_id
    {
        return Err(DeferredRunError::Corrupt(
            "deferred handle does not belong to the recorded identity".into(),
        ));
    }
    if leaf.handle.api != leaf.response_api {
        return Err(DeferredRunError::Corrupt(
            "deferred handle api does not match the recorded response api".into(),
        ));
    }
    if let Some(data) = &leaf.handle.data {
        let encoded = serde_json::to_string(data)
            .map_err(|error| DeferredRunError::Corrupt(error.to_string()))?;
        if encoded.len() > limits.max_handle_data_bytes {
            return Err(DeferredRunError::BoundExceeded(
                "deferred handle data".into(),
            ));
        }
    }
    if let DeferredPhase::EffectPending {
        response_id,
        usage_id,
    } = &leaf.phase
    {
        validate_deferred_segment(
            "reserved response id",
            response_id,
            limits.max_operation_bytes,
        )?;
        validate_deferred_segment("reserved usage id", usage_id, limits.max_operation_bytes)?;
    }
    Ok(())
}

/// Durable, generation-fenced store for suspended deferred runs.
///
/// `new` is an in-memory decision fixture. Agent sessions construct it through
/// [`crate::session::Session`] so every durable change is an append followed by
/// `sync_data`; replay makes the leaf survive a crash or restart.
#[derive(Debug)]
pub struct DeferredRunStore {
    limits: DeferredRunLimits,
    state: Mutex<DeferredRunStoreState>,
    journal: Option<Arc<SessionWriter>>,
}

#[derive(Debug, Default)]
struct DeferredRunStoreState {
    records: BTreeMap<String, DeferredRunRecord>,
    terminal: VecDeque<String>,
    closed: bool,
}

impl Default for DeferredRunStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DeferredRunStore {
    /// Creates a store with the default hard bounds.
    pub fn new() -> Self {
        Self::with_limits(DeferredRunLimits::default())
    }

    /// Creates a store with explicit hard bounds.
    pub fn with_limits(limits: DeferredRunLimits) -> Self {
        Self {
            limits,
            state: Mutex::new(DeferredRunStoreState::default()),
            journal: None,
        }
    }

    pub(crate) fn with_journal(journal: Arc<SessionWriter>) -> Self {
        Self {
            journal: Some(journal),
            ..Self::new()
        }
    }

    pub(crate) fn attach_journal(mut self, journal: Arc<SessionWriter>) -> Self {
        self.journal = Some(journal);
        self
    }

    /// Hard bounds in force for this store.
    pub fn limits(&self) -> DeferredRunLimits {
        self.limits
    }

    /// Fences further durable changes; replay still exposes the records.
    pub fn close(&self) {
        self.lock_state().closed = true;
    }

    /// Durable state of one operation, if one exists.
    pub fn record(&self, operation_id: &str) -> Option<DeferredRunRecord> {
        self.lock_state().records.get(operation_id).cloned()
    }

    /// Every durable record, in stable operation order.
    pub fn records(&self) -> Vec<DeferredRunRecord> {
        self.lock_state().records.values().cloned().collect()
    }

    /// Every non-terminal record, in stable operation order.
    pub fn parked_records(&self) -> Vec<DeferredRunRecord> {
        self.lock_state()
            .records
            .values()
            .filter(|record| !record.is_terminal())
            .cloned()
            .collect()
    }

    /// Parks a run at `deferred.suspended` from one classified response.
    ///
    /// A malformed, absent, foreign, rejected, or aborted handle is a terminal
    /// decision with **no durable write**, exactly like the in-memory decision
    /// core. The operation id is the durable identity of the parked request, so
    /// two different suspensions of one session never overwrite each other.
    pub fn suspend(
        &self,
        identity: &ModelIdentity,
        operation_id: &str,
        source_entry_id: &str,
        declaration: DeferredResponseDeclaration,
    ) -> Result<DeferredSuspendDecision, DeferredRunError> {
        let decision =
            suspend_deferred_response(identity, operation_id, source_entry_id, declaration);
        let DeferredSuspendDecision::Suspended(leaf) = decision else {
            return Ok(decision);
        };
        let record = DeferredRunRecord::suspended(*leaf)?;
        self.write_new(record.clone())?;
        match record.leaf() {
            Some(leaf) => Ok(DeferredSuspendDecision::Suspended(Box::new(leaf.clone()))),
            None => Err(DeferredRunError::Corrupt(
                "suspension did not persist".into(),
            )),
        }
    }

    /// Begins exactly one resume pass against the current durable generation.
    ///
    /// With [`DeferredResumeIntent::Observe`] nothing is written and no
    /// provider work may start. With [`DeferredResumeIntent::Poll`] the pass
    /// owns exactly one permit: the admitted poll's `deferred.effect_pending`
    /// intent — including its fresh reserved durable ids — is written **before**
    /// this method returns, so a crash during the poll leaves an
    /// unknown-outcome leaf rather than an in-process wait. That leaf may then
    /// only be replaced by an explicit
    /// [`DeferredResumeIntent::ReplaceUnknownPoll`] pass under fresh ids.
    /// Refusals never write and never fall back to waiting.
    pub fn begin_pass(
        &self,
        operation_id: &str,
        pass_id: impl Into<String>,
        intent: DeferredResumeIntent,
        now_ms: i64,
    ) -> Result<DeferredResumeStart, DeferredRunError> {
        let pass_id = pass_id.into();
        validate_deferred_segment("pass id", &pass_id, self.limits.max_operation_bytes)?;
        let mut state = self.lock_state();
        if state.closed {
            return Err(DeferredRunError::Closed);
        }
        let Some(record) = state.records.get(operation_id).cloned() else {
            return Ok(DeferredResumeStart::Unknown);
        };
        if record.is_terminal() {
            return Ok(DeferredResumeStart::Finished(Box::new(record)));
        }
        let leaf = record.leaf().cloned().ok_or_else(|| {
            DeferredRunError::Corrupt("non-terminal record without a leaf".into())
        })?;
        let mut permit = match intent {
            DeferredResumeIntent::Poll => DeferredPollPermit::one(pass_id, record.generation),
            DeferredResumeIntent::Observe => DeferredPollPermit::none(pass_id, record.generation),
            DeferredResumeIntent::ReplaceUnknownPoll => {
                DeferredPollPermit::one_replacing_unknown(pass_id, record.generation)
            }
        };
        // The reserved ids are derived from the durable operation, generation,
        // and poll the pass actually prepares against, so a replacement poll at
        // a bumped generation never reuses an abandoned reservation.
        let poll = match &leaf.phase {
            DeferredPhase::Suspended => leaf.poll.saturating_add(1),
            DeferredPhase::EffectPending { .. } => leaf.poll,
        };
        let (response_id, usage_id) = reserved_poll_ids(operation_id, record.generation, poll);
        let mut reserved = [response_id, usage_id].into_iter();
        let mut next_id = move || reserved.next().unwrap_or_default();
        match prepare_deferred_poll(&leaf, &mut permit, now_ms, &mut next_id) {
            DeferredPollPreparation::Waiting(observation) => {
                Ok(DeferredResumeStart::Waiting(observation))
            }
            DeferredPollPreparation::Refused(refusal) => Ok(DeferredResumeStart::Refused(refusal)),
            DeferredPollPreparation::Admitted(intent) => {
                let generation = record.generation.checked_add(1).ok_or_else(|| {
                    DeferredRunError::BoundExceeded("deferred generations".into())
                })?;
                let next_leaf = DeferredSuspended {
                    operation_id: leaf.operation_id.clone(),
                    source_entry_id: leaf.source_entry_id.clone(),
                    identity: leaf.identity.clone(),
                    response_api: leaf.response_api.clone(),
                    poll: intent.poll,
                    phase: intent.phase.clone(),
                    handle: leaf.handle.clone(),
                    generation,
                };
                let next = DeferredRunRecord::effect_pending(next_leaf)?;
                self.write_locked(&mut state, next.clone())?;
                Ok(DeferredResumeStart::Admitted(Box::new(
                    AdmittedDeferredPoll {
                        intent: *intent,
                        effect_pending: next,
                    },
                )))
            }
        }
    }

    /// Applies one performed poll outcome to its durable effect-pending leaf.
    ///
    /// The completion is fenced on the effect-pending record the pass wrote, so
    /// only the pass that owned the permit can settle, park, or fail the leaf.
    /// A still-deferred poll returns to `deferred.suspended` at the same poll
    /// number with a bumped generation; a settled or failed poll writes a
    /// terminal tombstone so no later pass can poll it again.
    pub fn complete_pass(
        &self,
        poll: &AdmittedDeferredPoll,
        outcome: DeferredPollOutcome,
    ) -> Result<DeferredPollCompletion, DeferredRunError> {
        let mut state = self.lock_state();
        if state.closed {
            return Err(DeferredRunError::Closed);
        }
        let operation_id = poll.effect_pending.operation_id.clone();
        let record = state
            .records
            .get(&operation_id)
            .cloned()
            .ok_or_else(|| DeferredRunError::UnknownOperation(operation_id.clone()))?;
        if record != poll.effect_pending {
            return Err(DeferredRunError::StaleGeneration {
                operation: operation_id,
                expected: poll.effect_pending.generation,
                actual: record.generation,
            });
        }
        let leaf = record.leaf().cloned().ok_or_else(|| {
            DeferredRunError::Corrupt("effect-pending record lost its leaf".into())
        })?;
        let (response_id, usage_id) = match &poll.intent.phase {
            DeferredPhase::EffectPending {
                response_id,
                usage_id,
            } => (response_id.clone(), usage_id.clone()),
            DeferredPhase::Suspended => {
                return Err(DeferredRunError::Corrupt(
                    "an admitted poll is always effect pending".into(),
                ))
            }
        };
        match leaf.resume_after_poll(&poll.intent, outcome) {
            DeferredResume::Suspended(next_leaf) => {
                let observation = next_leaf.observation();
                let next = DeferredRunRecord::suspended(*next_leaf)?;
                self.write_locked(&mut state, next)?;
                Ok(DeferredPollCompletion::Suspended(observation))
            }
            DeferredResume::Settled => {
                let generation = record.generation.checked_add(1).ok_or_else(|| {
                    DeferredRunError::BoundExceeded("deferred generations".into())
                })?;
                let next =
                    DeferredRunRecord::settled(&operation_id, generation, &response_id, &usage_id);
                self.write_locked(&mut state, next)?;
                Ok(DeferredPollCompletion::Settled {
                    response_id,
                    usage_id,
                })
            }
            DeferredResume::Failed(failure) => {
                let generation = record.generation.checked_add(1).ok_or_else(|| {
                    DeferredRunError::BoundExceeded("deferred generations".into())
                })?;
                // The tombstone must be bounded so a verbose provider message
                // can never prevent the terminal write; an unwritten tombstone
                // would leave the poll replaceable and could bill it twice.
                let diagnostic =
                    truncate_bounded(&failure.diagnostic, self.limits.max_operation_bytes);
                let next = DeferredRunRecord::failed(&operation_id, generation, diagnostic);
                self.write_locked(&mut state, next)?;
                Ok(DeferredPollCompletion::Failed(failure))
            }
        }
    }

    /// Cancels one parked deferred run, fenced on its current generation.
    ///
    /// The tombstone makes cancellation durable: a later resume, including
    /// after a restart, reports a terminal record instead of polling. When the
    /// cancelled leaf was `deferred.effect_pending`, the abandoned poll's
    /// outcome is unknown and the caller must record that exposure rather than
    /// inventing usage or re-polling.
    pub fn cancel(
        &self,
        operation_id: &str,
        expected_generation: u64,
    ) -> Result<DeferredRunCancellation, DeferredRunError> {
        let mut state = self.lock_state();
        if state.closed {
            return Err(DeferredRunError::Closed);
        }
        let record = state
            .records
            .get(operation_id)
            .cloned()
            .ok_or_else(|| DeferredRunError::UnknownOperation(operation_id.to_owned()))?;
        if record.is_terminal() {
            return Err(DeferredRunError::OutcomeKnown(operation_id.to_owned()));
        }
        if record.generation != expected_generation {
            return Err(DeferredRunError::StaleGeneration {
                operation: operation_id.to_owned(),
                expected: expected_generation,
                actual: record.generation,
            });
        }
        let generation = record
            .generation
            .checked_add(1)
            .ok_or_else(|| DeferredRunError::BoundExceeded("deferred generations".into()))?;
        let cancelled = DeferredRunRecord::cancelled(operation_id, generation);
        self.write_locked(&mut state, cancelled.clone())?;
        Ok(DeferredRunCancellation {
            previous: record,
            cancelled,
        })
    }

    pub(crate) fn restore(&self, record: DeferredRunRecord) -> Result<(), DeferredRunError> {
        record.validate(&self.limits)?;
        let mut state = self.lock_state();
        if state.closed {
            return Err(DeferredRunError::Closed);
        }
        if let Some(existing) = state.records.get(&record.operation_id) {
            if existing.is_terminal() {
                return Err(DeferredRunError::Corrupt(
                    "a terminal deferred record may not be followed".into(),
                ));
            }
            if record.generation <= existing.generation {
                return Err(DeferredRunError::GenerationRegression {
                    operation: record.operation_id.clone(),
                    previous: existing.generation,
                    next: record.generation,
                });
            }
        }
        self.insert_locked(&mut state, record)
    }

    fn write_new(&self, record: DeferredRunRecord) -> Result<(), DeferredRunError> {
        record.validate(&self.limits)?;
        let mut state = self.lock_state();
        if state.closed {
            return Err(DeferredRunError::Closed);
        }
        if record.generation != 0 {
            return Err(DeferredRunError::GenerationRegression {
                operation: record.operation_id.clone(),
                previous: 0,
                next: record.generation,
            });
        }
        if let Some(existing) = state.records.get(&record.operation_id) {
            return Err(if existing.is_terminal() {
                DeferredRunError::OutcomeKnown(record.operation_id.clone())
            } else {
                DeferredRunError::StaleGeneration {
                    operation: record.operation_id.clone(),
                    expected: 0,
                    actual: existing.generation,
                }
            });
        }
        self.ensure_capacity(&state, &record.operation_id)?;
        // The durable append precedes the in-memory change: a failed append
        // leaves the previous leaf authoritative instead of a state a restart
        // could not observe.
        self.persist(&record)?;
        self.insert_locked(&mut state, record)
    }

    fn write_locked(
        &self,
        state: &mut DeferredRunStoreState,
        record: DeferredRunRecord,
    ) -> Result<(), DeferredRunError> {
        record.validate(&self.limits)?;
        let existing = state.records.get(&record.operation_id);
        match existing {
            Some(existing) if existing.is_terminal() => {
                return Err(DeferredRunError::OutcomeKnown(record.operation_id.clone()));
            }
            Some(existing) => {
                let expected_next = existing.generation.checked_add(1).ok_or_else(|| {
                    DeferredRunError::BoundExceeded("deferred generations".into())
                })?;
                if record.generation != expected_next {
                    return Err(DeferredRunError::GenerationRegression {
                        operation: record.operation_id.clone(),
                        previous: existing.generation,
                        next: record.generation,
                    });
                }
            }
            None => {
                if record.generation != 0 {
                    return Err(DeferredRunError::GenerationRegression {
                        operation: record.operation_id.clone(),
                        previous: 0,
                        next: record.generation,
                    });
                }
            }
        }
        // Only an accepted transition is durably appended; refusals never
        // write bytes.
        self.ensure_capacity(state, &record.operation_id)?;
        self.persist(&record)?;
        self.insert_locked(state, record)
    }

    /// Refuses a new operation before any durable bytes are written when the
    /// bound is full of non-evictable live leaves.
    fn ensure_capacity(
        &self,
        state: &DeferredRunStoreState,
        operation_id: &str,
    ) -> Result<(), DeferredRunError> {
        if state.records.contains_key(operation_id) || state.records.len() < self.limits.max_runs {
            return Ok(());
        }
        if state
            .terminal
            .iter()
            .any(|candidate| state.records.contains_key(candidate))
        {
            return Ok(());
        }
        Err(DeferredRunError::BoundExceeded(format!(
            "{} deferred runs retained (limit {})",
            state.records.len(),
            self.limits.max_runs
        )))
    }

    fn insert_locked(
        &self,
        state: &mut DeferredRunStoreState,
        record: DeferredRunRecord,
    ) -> Result<(), DeferredRunError> {
        let operation_id = record.operation_id.clone();
        let terminal = record.is_terminal();
        // A terminal record frees its live slot but stays as a tombstone so a
        // restart cannot revive the operation; tombstones are evicted oldest
        // first once the hard bound is reached.
        if !state.records.contains_key(&operation_id) && state.records.len() >= self.limits.max_runs
        {
            let mut evicted = false;
            while let Some(oldest) = state.terminal.pop_front() {
                if state.records.remove(&oldest).is_some() {
                    evicted = true;
                    break;
                }
            }
            if !evicted {
                return Err(DeferredRunError::BoundExceeded(format!(
                    "{} deferred runs retained (limit {})",
                    state.records.len(),
                    self.limits.max_runs
                )));
            }
        }
        // Replacing a live record keeps its position in the terminal order
        // untouched; a terminal write moves it to the back of the queue.
        if terminal {
            state.terminal.push_back(operation_id.clone());
        }
        state.records.insert(operation_id, record);
        Ok(())
    }

    fn persist(&self, record: &DeferredRunRecord) -> Result<(), DeferredRunError> {
        let Some(journal) = &self.journal else {
            return Ok(());
        };
        let value = crate::session::SessionRecord::DeferredRun {
            record: record.clone(),
        };
        let mut bytes = serde_json::to_vec(&value)
            .map_err(|error| DeferredRunError::Corrupt(error.to_string()))?;
        bytes.push(b'\n');
        journal
            .persist(&bytes)
            .map_err(|error| DeferredRunError::Persistence(error.to_string()))
    }

    fn lock_state(&self) -> MutexGuard<'_, DeferredRunStoreState> {
        // A poisoned lock only means another caller panicked; the store is
        // plain data, so keep serving durability instead of disabling it.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// How one resume pass may advance a parked deferred run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredResumeIntent {
    /// The pass owns exactly one poll permit for the current generation. A leaf
    /// whose poll outcome is unknown is refused, not replaced.
    Poll,
    /// The pass observes only: no permit, no durable write, no provider work.
    Observe,
    /// The pass owns one poll permit **and** explicitly replaces a leaf whose
    /// poll outcome is unknown with a new billable poll (fresh reserved ids at
    /// the same poll number). This is the only intent that may spend a second
    /// billable request for an effect that may already have been accepted, so a
    /// caller may use it only after an explicit user resume decision.
    ReplaceUnknownPoll,
}

/// Result of beginning one resume pass.
#[derive(Clone, Debug, PartialEq)]
pub enum DeferredResumeStart {
    /// No durable record exists for the operation.
    Unknown,
    /// The record is terminal; nothing may poll again.
    Finished(Box<DeferredRunRecord>),
    /// Observe-only pass: the run stays durably suspended and nothing is
    /// written.
    Waiting(Box<SuspendedRunObservation>),
    /// Exactly one poll is admitted and its effect-pending intent is durable.
    Admitted(Box<AdmittedDeferredPoll>),
    /// Fail closed: no durable write and no provider work.
    Refused(Box<DeferredPollRefusal>),
}

/// One admitted poll plus the durable effect-pending record that fences it.
#[derive(Clone, Debug, PartialEq)]
pub struct AdmittedDeferredPoll {
    /// Durable intent recorded before the provider call.
    pub intent: DeferredPollIntent,
    /// The effect-pending record written before the provider call.
    pub effect_pending: DeferredRunRecord,
}

/// What a performed poll did to the durable leaf.
#[derive(Clone, Debug, PartialEq)]
pub enum DeferredPollCompletion {
    /// The provider is still working; the run is parked again at
    /// `deferred.suspended` with a bumped generation.
    Suspended(SuspendedRunObservation),
    /// The provider settled; these reserved durable ids name the commit slots.
    Settled {
        /// Reserved response entry id.
        response_id: String,
        /// Reserved usage id.
        usage_id: String,
    },
    /// The provider failed the poll; the run is terminal.
    Failed(Box<DeferredSuspendFailure>),
}

/// One durable cancellation: the record that was cancelled and its tombstone.
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredRunCancellation {
    /// The parked record that was cancelled.
    pub previous: DeferredRunRecord,
    /// The terminal tombstone now durable.
    pub cancelled: DeferredRunRecord,
}

impl DeferredRunCancellation {
    /// Whether the cancelled leaf had an admitted poll whose outcome is
    /// unknown, so the caller must record exposure instead of usage.
    pub fn abandoned_unknown_poll(&self) -> bool {
        matches!(self.previous.state, DeferredRunState::EffectPending { .. })
    }
}

/// Fresh reserved durable ids for one admitted poll.
///
/// Ids are derived from the operation, the durable generation, and the poll
/// number, so a crashed pass and its replacement never share an id: recovery at
/// a bumped generation reserves a different pair, and the abandoned pair is
/// exactly what the replacement reports for deletion. Deterministic ids also
/// make a replayed generation map back to the same reservation instead of
/// minting a second one.
pub fn reserved_poll_ids(operation_id: &str, generation: u64, poll: u64) -> (String, String) {
    (
        reserved_poll_id("response", operation_id, generation, poll),
        reserved_poll_id("usage", operation_id, generation, poll),
    )
}

fn reserved_poll_id(kind: &str, operation_id: &str, generation: u64, poll: u64) -> String {
    let digest = Sha256::digest(operation_id.as_bytes());
    let mut prefix = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        prefix.push_str(&format!("{byte:02x}"));
    }
    format!("deferred-{kind}-{prefix}-g{generation}-p{poll}")
}

impl From<octet_ai::deferred::DeferredHandle> for DeferredHandle {
    /// Adopts one codec-issued provider handle into the durable decision core.
    ///
    /// The transport owns the handle shape; the durable leaf records the exact
    /// provider token, expiry, suggested poll delay, and opaque conversion data
    /// so a restart can poll the same effect with the same identity.
    fn from(handle: octet_ai::deferred::DeferredHandle) -> Self {
        Self {
            provider: handle.provider,
            model_id: handle.model_id,
            api: handle.api,
            id: handle.id,
            expires_at_ms: handle.expires_at_ms,
            poll_after_ms: handle.poll_after_ms,
            data: handle.data,
        }
    }
}

impl From<DeferredHandle> for octet_ai::deferred::DeferredHandle {
    /// Returns one durable handle to the codec for a provider poll.
    fn from(handle: DeferredHandle) -> Self {
        Self {
            provider: handle.provider,
            model_id: handle.model_id,
            api: handle.api,
            id: handle.id,
            expires_at_ms: handle.expires_at_ms,
            poll_after_ms: handle.poll_after_ms,
            data: handle.data,
        }
    }
}

impl DeferredResponseDeclaration {
    /// Classifies one completed codec response as a deferred declaration.
    ///
    /// The api id is the one the transport attached to the handle: the codec
    /// response carries no separate api string, so the recorded api is the
    /// transport's own declaration and every later poll must agree with it. A
    /// `deferred` stop reason without a handle stays a malformed handle, never
    /// a suspension.
    pub fn from_response(response: &octet_ai::Response) -> Self {
        let stop_reason = match response.stop_reason {
            octet_ai::StopReason::Deferred => DeferredStopReason::Deferred,
            octet_ai::StopReason::Refusal => DeferredStopReason::Failed,
            _ => DeferredStopReason::Settled,
        };
        let handle = response.deferred.clone().map(DeferredHandle::from);
        let api = handle
            .as_ref()
            .map(|handle| handle.api.clone())
            .unwrap_or_default();
        Self {
            stop_reason,
            api,
            handle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ModelIdentity {
        ModelIdentity::new("test-provider", "test-model")
    }

    fn handle() -> DeferredHandle {
        DeferredHandle::new("test-provider", "test-model", "test-api", "handle-1")
    }

    fn effect_pending_leaf() -> DeferredSuspended {
        DeferredSuspended {
            operation_id: "op-1".to_owned(),
            source_entry_id: "entry-1".to_owned(),
            identity: identity(),
            response_api: "test-api".to_owned(),
            poll: 3,
            phase: DeferredPhase::EffectPending {
                response_id: "abandoned-response".to_owned(),
                usage_id: "abandoned-usage".to_owned(),
            },
            handle: handle(),
            generation: 7,
        }
    }

    #[test]
    fn handle_debug_redacts_conversion_data() {
        let mut handle = handle();
        handle.data = Some(serde_json::json!({
            "provider_token": "secret-provider-material",
        }));
        let debug = format!("{handle:?}");
        assert!(
            debug.contains("[REDACTED]"),
            "the conversion data slot must be marked redacted: {debug}"
        );
        assert!(
            !debug.contains("secret-provider-material"),
            "provider conversion data must never reach Debug: {debug}"
        );
        // The typed identity stays debuggable: redaction covers `data` only.
        assert!(debug.contains("handle-1"));
    }

    #[test]
    fn a_plain_permit_never_replaces_an_unknown_outcome_poll() {
        let leaf = effect_pending_leaf();
        let mut permit = DeferredPollPermit::one("pass-1", leaf.generation);
        let preparation = prepare_deferred_poll(&leaf, &mut permit, 0, || "fresh".to_owned());
        match preparation {
            DeferredPollPreparation::Refused(refusal) => {
                assert_eq!(
                    refusal.kind,
                    DeferredPollRefusalKind::UnknownPollOutcome { poll: 3 }
                );
                assert!(refusal.diagnostic.contains("unknown outcome"));
            }
            other => panic!("an unknown outcome must be refused, got {other:?}"),
        }
        assert_eq!(permit.remaining(), 1, "a refusal spends no permit");
        assert!(!permit.is_consumed());
        assert!(matches!(
            prepare_deferred_poll(
                &leaf,
                &mut DeferredPollPermit::none("pass-2", leaf.generation),
                0,
                || "fresh".to_owned()
            ),
            DeferredPollPreparation::Waiting(_)
        ));
    }

    #[test]
    fn an_explicit_replacement_resumes_the_unknown_outcome_under_fresh_ids() {
        let leaf = effect_pending_leaf();
        let mut permit = DeferredPollPermit::one_replacing_unknown("pass-2", leaf.generation);
        let preparation = prepare_deferred_poll(&leaf, &mut permit, 0, || "fresh".to_owned());
        let DeferredPollPreparation::Admitted(intent) = preparation else {
            panic!("an explicit replacement must be admitted, got {preparation:?}");
        };
        assert_eq!(intent.poll, 3, "a poll is not a new request");
        let DeferredPhase::EffectPending {
            response_id,
            usage_id,
        } = &intent.phase
        else {
            panic!("an admitted replacement is effect pending");
        };
        assert_eq!(response_id, "fresh");
        assert_eq!(usage_id, "fresh");
        let replacement = intent
            .discard_unknown_poll
            .expect("the abandoned reservation must be reported");
        assert_eq!(replacement.abandoned_response_id, "abandoned-response");
        assert_eq!(replacement.abandoned_usage_id, "abandoned-usage");
    }
}
