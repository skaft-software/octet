//! Deferred (parked) runs and their provider polling.

use super::*;

/// One provider deferred-poll attempt requested from a [`DeferredPollSource`].
#[derive(Debug)]
pub enum DeferredPollReply {
    /// The provider is still working; the returned handle parks the run again.
    StillDeferred(DeferredHandle),
    /// The provider finished the request.
    Settled(Box<octet_ai::Response>),
    /// The provider failed the poll. The diagnostic must be bounded and must
    /// not contain provider payloads, credentials, or request content. The
    /// admitted attempt may have been dispatched, so its usage is unknown.
    Failed(String),
    /// The poll was refused **before dispatch** (for example a transport permit
    /// that was stale or already consumed). Nothing was billed, no exposure is
    /// created, and the durable effect-pending leaf stays replaceable.
    Refused(String),
}

/// Provider transport for the deferred-fetch half of a suspended run.
///
/// The agent owns the durable lifecycle (permit, effect-pending intent,
/// generation fence, recovery, cancel); the source owns the provider request
/// for one admitted poll. It is called only after the poll's effect-pending
/// intent is durable and never more than once per permit. `permit` is the
/// codec's one-shot transport permit, minted by the agent for this pass and
/// consumed by the provider call; a source must not mint or reuse one.
#[async_trait::async_trait]
pub trait DeferredPollSource: Send + Sync {
    /// Performs exactly one deferred poll against the provider.
    async fn poll_deferred(
        &self,
        handle: &DeferredHandle,
        permit: octet_ai::deferred::DeferredPollPermit,
        leaf_generation: u64,
    ) -> DeferredPollReply;
}

/// A [`DeferredPollSource`] that polls one configured [`AiClient`] route.
///
/// The client owns the transport permit fence: a stale, replayed, or missing
/// permit is refused before any request is dispatched, which this source maps
/// to [`DeferredPollReply::Refused`] so a purely local refusal never creates
/// billing exposure. A transport that cannot park or poll fails closed for the
/// same reason. Once a request is dispatched, any failure is reported as
/// [`DeferredPollReply::Failed`] because its usage cannot be known.
#[derive(Clone)]
pub struct AiDeferredPollSource {
    pub(super) client: AiClient,
    pub(super) model: Model,
    pub(super) wait_ms: Option<u64>,
}

impl std::fmt::Debug for AiDeferredPollSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AiDeferredPollSource")
            .field("endpoint", &self.model.endpoint.id.0)
            .field("model", &self.model.spec.id.0)
            .field("wait_ms", &self.wait_ms)
            .finish()
    }
}

impl AiDeferredPollSource {
    /// Builds a poll source for one client/model route.
    pub fn new(client: AiClient, model: Model) -> Self {
        Self {
            client,
            model,
            wait_ms: None,
        }
    }

    /// Bounds the provider long-poll; `Some(0)` performs one status check.
    pub fn with_wait_ms(mut self, wait_ms: Option<u64>) -> Self {
        self.wait_ms = wait_ms;
        self
    }

    pub(super) fn refusal_or_failure(&self, error: AiError) -> DeferredPollReply {
        match &error {
            AiError::Deferred(refusal) => DeferredPollReply::Refused(refusal.to_string()),
            AiError::Unsupported(octet_ai::UnsupportedError::Deferred) => {
                DeferredPollReply::Refused(
                    "deferred provider responses are unsupported on this transport".to_owned(),
                )
            }
            _ => DeferredPollReply::Failed(public_error_diagnostic(
                &AgentError::Ai(error),
                &self.model.endpoint.id.0,
                &self.model.spec.id.0,
            )),
        }
    }
}

#[async_trait::async_trait]
impl DeferredPollSource for AiDeferredPollSource {
    async fn poll_deferred(
        &self,
        handle: &DeferredHandle,
        permit: octet_ai::deferred::DeferredPollPermit,
        leaf_generation: u64,
    ) -> DeferredPollReply {
        let codec_handle = octet_ai::deferred::DeferredHandle::from(handle.clone());
        let mut stream = match self
            .client
            .fetch_deferred(
                &self.model,
                codec_handle,
                permit,
                leaf_generation,
                self.wait_ms,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => return self.refusal_or_failure(error),
        };
        loop {
            match stream.next().await {
                Some(Ok(StreamEvent::Finished(response))) => {
                    if response.stop_reason == StopReason::Deferred {
                        return match response.deferred.clone() {
                            Some(handle) => {
                                DeferredPollReply::StillDeferred(DeferredHandle::from(handle))
                            }
                            None => DeferredPollReply::Failed(
                                "provider parked the poll without a deferred handle".to_owned(),
                            ),
                        };
                    }
                    return DeferredPollReply::Settled(Box::new(response));
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => return self.refusal_or_failure(error),
                None => {
                    return DeferredPollReply::Failed(
                        "deferred poll stream ended without a terminal response".to_owned(),
                    );
                }
            }
        }
    }
}

/// Result of one deferred resume pass.
#[derive(Debug)]
pub enum DeferredRunOutcome {
    /// The durable record is terminal (settled, cancelled, or failed); nothing
    /// may poll it again.
    Finished {
        /// Durable operation identity.
        operation_id: String,
        /// Terminal state label (`settled`, `cancelled`, or `failed`).
        state: &'static str,
    },
    /// Observe-only pass: nothing was written and no provider work started.
    Waiting(SuspendedRunObservation),
    /// Fail closed: nothing was written and no provider work started.
    Refused(Box<DeferredPollRefusal>),
    /// The provider is still working; the run is parked again at
    /// `deferred.suspended` with a bumped generation.
    Suspended(SuspendedRunObservation),
    /// The poll settled; these reserved durable ids name the commit slots for
    /// the response entry and its usage record. The caller commits the response
    /// exactly once; the durable tombstone already blocks a re-poll.
    Settled {
        /// The complete provider response.
        response: Box<octet_ai::Response>,
        /// Reserved durable response entry id.
        response_id: String,
        /// Reserved durable usage id.
        usage_id: String,
    },
    /// Terminal failure; the run must not poll again.
    Failed(Box<DeferredSuspendFailure>),
    /// The admitted poll was refused before any provider work (for example a
    /// transport permit that was already consumed). Nothing was billed and the
    /// durable leaf stays replaceable for a later permitted pass.
    PollRefused(String),
}

/// The durable model identity used to validate deferred provider handles.
///
/// Octet has no separate provider registry identity in a model spec, so the
/// endpoint id is the durable provider and the model id is the provider-local
/// model identity. A handle whose provider, model id, or api does not match the
/// run's durable configuration or the response that carried it is a terminal
/// failure, never a suspension.
pub fn deferred_model_identity(model: &Model) -> ModelIdentity {
    ModelIdentity::new(model.endpoint.id.0.clone(), model.spec.id.0.clone())
}

pub(super) fn stop_reason_label(reason: DeferredStopReason) -> &'static str {
    match reason {
        DeferredStopReason::Deferred => "deferred",
        DeferredStopReason::Settled => "settled",
        DeferredStopReason::Failed => "failed",
        DeferredStopReason::Aborted => "aborted",
    }
}

pub(super) fn refusal_stop_reason(refusal: &DeferredPollRefusal) -> &'static str {
    match refusal.kind {
        crate::tools::deferred::DeferredPollRefusalKind::ExpiredHandle { .. } => "aborted",
        // An unknown outcome is refused, not terminal: the run stays parked at
        // its effect-pending leaf until an explicit replacement resume.
        crate::tools::deferred::DeferredPollRefusalKind::StalePermit { .. }
        | crate::tools::deferred::DeferredPollRefusalKind::AlreadyConsumed
        | crate::tools::deferred::DeferredPollRefusalKind::UnknownPollOutcome { .. }
        | crate::tools::deferred::DeferredPollRefusalKind::ForeignHandle(_) => "refused",
    }
}

pub(super) fn bounded_deferred_label(value: &str) -> String {
    const LIMIT: usize = 256;
    if value.len() <= LIMIT {
        return value.to_owned();
    }
    let mut end = LIMIT;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Whether one provider turn reported any billable token bucket.
///
/// A parked request reports no billed work; recording a zero-token unpriced
/// operation would falsely block hard cost ceilings.
pub(super) fn usage_is_billed(usage: &Usage) -> bool {
    usage.input_tokens > 0
        || usage.cache_read_tokens > 0
        || usage.cache_write_tokens > 0
        || usage.output_tokens > 0
        || usage.reasoning_tokens > 0
        || usage.total_tokens > 0
}

impl Agent {
    /// The durable model identity used to validate deferred provider handles.
    ///
    /// Octet has no separate provider registry identity in a model spec, so the
    /// endpoint id is the durable provider and the model id is the provider-local
    /// model identity. A handle whose provider, model id, or api does not match
    /// the run's durable configuration is a terminal failure, never a
    /// suspension.
    pub fn deferred_model_identity(&self) -> ModelIdentity {
        deferred_model_identity(&self.model)
    }

    /// Every durable deferred-run record in this session, including terminal
    /// tombstones. These are auxiliary lifecycle records: never model-visible
    /// context and never usage accounting.
    pub fn deferred_runs(&self) -> Vec<DeferredRunRecord> {
        self.session.deferred_runs()
    }

    /// Every non-terminal deferred run that may still be resumed.
    pub fn parked_deferred_runs(&self) -> Vec<DeferredRunRecord> {
        self.session.parked_deferred_runs()
    }

    /// Durable state of one deferred run, if any.
    pub fn deferred_run(&self, operation_id: &str) -> Option<DeferredRunRecord> {
        self.session.deferred_run(operation_id)
    }

    /// Parks one deferred provider response at the `deferred.suspended` leaf.
    ///
    /// Row 4.12: the decision core validates the handle against the run's
    /// durable identity and the api of the response that carried it. A valid
    /// handle becomes one durable, replaceable session record and emits
    /// `run_suspend` to registered observers; anything else is a terminal
    /// [`DeferredSuspendDecision::Failed`] with **no durable write** and no
    /// claim that the request settled.
    pub fn suspend_deferred_run(
        &mut self,
        operation_id: &str,
        source_entry_id: &str,
        declaration: DeferredResponseDeclaration,
    ) -> Result<DeferredSuspendDecision, AgentError> {
        let identity = self.deferred_model_identity();
        let stop_reason = stop_reason_label(declaration.stop_reason);
        let store = self.session.deferred_run_store();
        let decision = store.suspend(&identity, operation_id, source_entry_id, declaration)?;
        match &decision {
            DeferredSuspendDecision::Suspended(leaf) => {
                self.observe_deferred_boundary(
                    &leaf.operation_id,
                    "deferred",
                    "suspended",
                    leaf.poll,
                    leaf.generation,
                    false,
                );
                self.emit_run_suspend(leaf);
            }
            DeferredSuspendDecision::Settled => {
                self.observe_deferred_boundary(operation_id, "settled", "settled", 0, 0, false);
            }
            DeferredSuspendDecision::Failed(_) => {
                self.observe_deferred_boundary(operation_id, stop_reason, "failed", 0, 0, false);
            }
        }
        Ok(decision)
    }

    /// Cancels one parked deferred run, fenced on its current generation.
    ///
    /// A cancelled leaf can never be polled, including after a restart. When
    /// the cancelled leaf had an admitted poll whose outcome is unknown, the
    /// abandoned attempt is recorded as sticky session exposure; no usage or
    /// cost is invented and no second poll is made.
    pub fn cancel_deferred_run(
        &mut self,
        operation_id: &str,
        expected_generation: u64,
    ) -> Result<DeferredRunCancellation, AgentError> {
        let store = self.session.deferred_run_store();
        let cancellation = store.cancel(operation_id, expected_generation)?;
        self.observe_deferred_boundary(
            operation_id,
            "aborted",
            "cancelled",
            cancellation
                .previous
                .leaf()
                .map(|leaf| leaf.poll)
                .unwrap_or_default(),
            cancellation.cancelled.generation,
            false,
        );
        if cancellation.abandoned_unknown_poll() {
            self.record_deferred_exposure()?;
        }
        Ok(cancellation)
    }

    /// Resumes one parked deferred run with exactly one poll permit.
    ///
    /// `intent` selects whether this pass owns a permit ([`DeferredResumeIntent::Poll`])
    /// or merely observes. A leaf whose admitted poll outcome is unknown
    /// (`deferred.effect_pending`) is refused by [`DeferredResumeIntent::Poll`];
    /// only [`DeferredResumeIntent::ReplaceUnknownPoll`], an explicit user
    /// resume decision, may replace it with a new billable poll under fresh
    /// reserved ids. An admitted poll's `deferred.effect_pending` intent
    /// is durable before the provider is called and `run_resume` is emitted
    /// before the poll; the permit is consumed exactly once and a second call
    /// with the same pass or an older generation is refused without provider
    /// work. An admitted poll that fails records the accepted attempt as
    /// exposure, because its usage cannot be known and re-polling would be a
    /// second billable request for the same effect.
    pub async fn resume_deferred_run(
        &mut self,
        operation_id: &str,
        pass_id: impl Into<String>,
        intent: DeferredResumeIntent,
        source: &dyn DeferredPollSource,
    ) -> Result<DeferredRunOutcome, AgentError> {
        let pass_id = pass_id.into();
        let store = self.session.deferred_run_store();
        let now_ms = i64::try_from(now_unix_millis()).unwrap_or(i64::MAX);
        let start = store.begin_pass(operation_id, pass_id.clone(), intent, now_ms)?;
        match start {
            DeferredResumeStart::Unknown => {
                Err(DeferredRunError::UnknownOperation(operation_id.to_owned()).into())
            }
            DeferredResumeStart::Finished(record) => Ok(DeferredRunOutcome::Finished {
                operation_id: record.operation_id.clone(),
                state: record.state_label(),
            }),
            DeferredResumeStart::Waiting(observation) => {
                self.observe_deferred_boundary(
                    operation_id,
                    "deferred",
                    "suspended",
                    observation.poll,
                    0,
                    false,
                );
                Ok(DeferredRunOutcome::Waiting(*observation))
            }
            DeferredResumeStart::Refused(refusal) => {
                let stop_reason = refusal_stop_reason(&refusal);
                self.observe_deferred_boundary(operation_id, stop_reason, "failed", 0, 0, false);
                Ok(DeferredRunOutcome::Refused(refusal))
            }
            DeferredResumeStart::Admitted(poll) => {
                self.drive_admitted_deferred_poll(pass_id, *poll, source)
                    .await
            }
        }
    }

    pub(super) async fn drive_admitted_deferred_poll(
        &mut self,
        pass_id: String,
        poll: AdmittedDeferredPoll,
        source: &dyn DeferredPollSource,
    ) -> Result<DeferredRunOutcome, AgentError> {
        let operation_id = poll.effect_pending.operation_id.clone();
        let generation = poll.intent.generation;
        let poll_number = poll.intent.poll;
        let recovery = poll.intent.discard_unknown_poll.is_some();
        // An abandoned unknown-outcome poll may already be accepted and billed;
        // its exposure is sticky and is never cleared by a later success.
        if recovery {
            self.record_deferred_exposure()?;
        }
        let resume = DeferredRunResumed {
            operation_id: operation_id.clone(),
            pass_id: pass_id.clone(),
            poll: poll_number,
            generation,
            recovery,
        };
        for observer in &self.extensions.observers {
            observer.on_run_resume(&resume);
        }
        self.observe_deferred_boundary(
            &operation_id,
            "deferred",
            "effect_pending",
            poll_number,
            generation,
            recovery,
        );
        // The transport permit is minted for the same unique pass and leaf
        // generation as the durable permit, and is consumed by the provider
        // call before any request is dispatched.
        let transport_permit = octet_ai::deferred::DeferredPollPermit::one(pass_id, generation);
        let reply = source
            .poll_deferred(&poll.intent.handle, transport_permit, generation)
            .await;
        let (outcome, settled) = match reply {
            DeferredPollReply::StillDeferred(handle) => {
                (DeferredPollOutcome::StillDeferred(handle), None)
            }
            DeferredPollReply::Settled(response) => (DeferredPollOutcome::Settled, Some(response)),
            DeferredPollReply::Failed(message) => (
                DeferredPollOutcome::Failed {
                    message: bounded_deferred_label(&message),
                },
                None,
            ),
            DeferredPollReply::Refused(message) => {
                // A refusal before dispatch is not a provider failure: nothing
                // was billed, no exposure is created, and the effect-pending
                // leaf stays replaceable for a later permitted pass.
                self.observe_deferred_boundary(
                    &operation_id,
                    "refused",
                    "effect_pending",
                    poll_number,
                    generation,
                    recovery,
                );
                return Ok(DeferredRunOutcome::PollRefused(bounded_deferred_label(
                    &message,
                )));
            }
        };
        let completion = self
            .session
            .deferred_run_store()
            .complete_pass(&poll, outcome)?;
        match completion {
            DeferredPollCompletion::Suspended(observation) => {
                self.observe_deferred_boundary(
                    &operation_id,
                    "deferred",
                    "suspended",
                    observation.poll,
                    0,
                    recovery,
                );
                Ok(DeferredRunOutcome::Suspended(observation))
            }
            DeferredPollCompletion::Settled {
                response_id,
                usage_id,
            } => {
                let Some(response) = settled else {
                    return Err(DeferredRunError::Corrupt(
                        "a settled poll requires the provider response".into(),
                    )
                    .into());
                };
                self.observe_deferred_boundary(
                    &operation_id,
                    "settled",
                    "settled",
                    poll_number,
                    generation,
                    recovery,
                );
                Ok(DeferredRunOutcome::Settled {
                    response,
                    response_id,
                    usage_id,
                })
            }
            DeferredPollCompletion::Failed(failure) => {
                // The admitted poll may have been accepted and billed; its
                // usage is unknown, so record exposure rather than a fabricated
                // cost or a second poll.
                self.record_deferred_exposure()?;
                self.observe_deferred_boundary(
                    &operation_id,
                    "failed",
                    "failed",
                    poll_number,
                    generation,
                    recovery,
                );
                Ok(DeferredRunOutcome::Failed(failure))
            }
        }
    }

    pub(super) fn emit_run_suspend(&self, leaf: &crate::tools::deferred::DeferredSuspended) {
        let suspension = DeferredRunSuspended {
            operation_id: leaf.operation_id.clone(),
            source_entry_id: leaf.source_entry_id.clone(),
            stop_reason: crate::tools::deferred::DeferredStopReason::Deferred,
            handle: leaf.handle.clone(),
            poll: leaf.poll,
            generation: leaf.generation,
        };
        for observer in &self.extensions.observers {
            observer.on_run_suspend(&suspension);
        }
    }

    /// One sticky exposure record for an accepted deferred attempt whose usage
    /// cannot be known. Never invents usage or cost, and never clears earlier
    /// exposure.
    pub(super) fn record_deferred_exposure(&mut self) -> Result<(), AgentError> {
        if self.session.has_uncertain_usage() {
            return Ok(());
        }
        self.session.record_usage_uncertainty(
            self.model.endpoint.id.clone(),
            self.model.spec.id.clone(),
            "deferred_poll",
        )?;
        Ok(())
    }

    pub(super) fn observe_deferred_boundary(
        &self,
        operation_id: &str,
        stop_reason: &str,
        phase: &str,
        poll: u64,
        generation: u64,
        recovery: bool,
    ) {
        let _guard = self
            .telemetry
            .begin_typed::<DeferredRunSpan>(DeferredRunAttributes {
                operation_id: bounded_deferred_label(operation_id),
                stop_reason: stop_reason.to_owned(),
                phase: phase.to_owned(),
                poll,
                generation,
                recovery,
                diagnostics: 0,
            });
    }
}
