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
//!   `deferred.effect_pending` leaf; the next permitted pass replaces that
//!   unknown poll under **fresh** ids at the **same** poll number and deletes the
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

/// Identity of the model captured in a run's durable configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeferredHandleRejection {
    /// The provider reported a deferred response without a handle.
    Absent,
    /// The handle carried an empty provider token.
    EmptyId,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeferredSuspendFailureKind {
    /// The handle was absent, empty, foreign, or for the wrong api.
    MalformedHandle(DeferredHandleRejection),
    /// The provider returned an error stop reason.
    ProviderRejected,
    /// The request was aborted.
    Aborted,
}

/// A terminal refusal to suspend.
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeferredPhase {
    /// `deferred.suspended`: no poll outcome is unknown; a permitted poll
    /// increments the poll number.
    Suspended,
    /// `deferred.effect_pending`: a poll was admitted and its outcome is unknown.
    /// Recovery replaces it under fresh ids at the same poll number.
    EffectPending {
        /// Reserved response entry id of the unknown poll.
        response_id: String,
        /// Reserved usage id of the unknown poll.
        usage_id: String,
    },
}

/// The durable `deferred.suspended` / `deferred.effect_pending` leaf.
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, PartialEq)]
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
/// representation of a pass that carries no permit at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredPollPermit {
    pass_id: String,
    generation: u64,
    remaining: u32,
    consumed: bool,
}

impl DeferredPollPermit {
    /// Grants the pass one deferred poll against `generation`.
    pub fn one(pass_id: impl Into<String>, generation: u64) -> Self {
        Self {
            pass_id: pass_id.into(),
            generation,
            remaining: 1,
            consumed: false,
        }
    }

    /// A pass that carries no poll permit.
    pub fn none(pass_id: impl Into<String>, generation: u64) -> Self {
        Self {
            pass_id: pass_id.into(),
            generation,
            remaining: 0,
            consumed: false,
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
}

/// A fail-closed refusal to poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredPollRefusal {
    /// Classified cause.
    pub kind: DeferredPollRefusalKind,
    /// Diagnostic for the host.
    pub diagnostic: String,
}

/// Replacement of one unknown-outcome poll under fresh ids.
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
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
