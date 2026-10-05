//! Steering and control inputs, their reservations, delivery and `RunControl`.

use super::*;

// Reservations follow semantic input out of the bounded ingress channel and
// into pending steering/follow-up batches, until persistence or run termination.
pub(super) const MAX_PENDING_CONTROL_INPUTS: usize = 64;

pub(super) const MAX_PENDING_CONTROL_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct ControlReservation {
    pub(super) _count: tokio::sync::OwnedSemaphorePermit,
    pub(super) _bytes: tokio::sync::OwnedSemaphorePermit,
}

pub(super) struct ReservedPayload {
    pub(super) input: UserInput,
    // None only for a prepared steering input not yet submitted.
    pub(super) reservation: Option<ControlReservation>,
}

pub(super) enum ReservedInput {
    Ready(ReservedPayload),
    Retractable(PreparedSteering),
}

impl ReservedInput {
    pub(super) fn push_pending(self, pending: &mut Vec<Self>) {
        // Recalled payloads release their permits immediately. Remove their
        // empty queue slots before admitting more, so repeated editing cannot
        // accumulate an unbounded backlog of receipt tombstones.
        pending.retain(Self::is_pending);
        if self.is_pending() {
            pending.push(self);
        }
    }

    pub(super) fn is_pending(&self) -> bool {
        match self {
            Self::Ready(_) => true,
            Self::Retractable(prepared) => prepared
                .receipt
                .payload
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some(),
        }
    }

    pub(super) fn claim(self) -> Option<ReservedPayload> {
        match self {
            Self::Ready(payload) => Some(payload),
            Self::Retractable(prepared) => prepared
                .receipt
                .payload
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
        }
    }
}

/// A single-use steering submission with a receipt available before sending.
///
/// Create with [`Self::new`], keep the receipt in the frontend, and move this
/// value into [`RunControl::steer_retractable`]. Dropping the submission (including
/// a cancelled send future) releases its input and any admission reservation.
/// Unlike the receipt, this value cannot be cloned or submitted twice.
pub struct PreparedSteering {
    pub(super) receipt: SteeringReceipt,
    pub(super) owner: Option<Arc<tokio::sync::Semaphore>>,
}

impl PreparedSteering {
    /// Prepares an input and its independently clonable recall receipt.
    /// Preparation does not reserve run capacity or start asynchronous work.
    pub fn new(input: impl Into<UserInput>) -> (Self, SteeringReceipt) {
        let receipt = SteeringReceipt {
            payload: Arc::new(Mutex::new(Some(ReservedPayload {
                input: input.into(),
                reservation: None,
            }))),
            recalled: CancellationToken::default(),
        };
        (
            Self {
                receipt: receipt.clone(),
                owner: None,
            },
            receipt,
        )
    }
}

impl Drop for PreparedSteering {
    fn drop(&mut self) {
        self.receipt
            .payload
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}

/// Clone-safe authority to recall one exact prepared steering input.
/// Identical text in different submissions has independent receipts.
#[derive(Clone)]
pub struct SteeringReceipt {
    pub(super) payload: Arc<Mutex<Option<ReservedPayload>>>,
    pub(super) recalled: CancellationToken,
}

impl std::fmt::Debug for SteeringReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteeringReceipt")
            .field("pending", &self.is_pending())
            .finish_non_exhaustive()
    }
}

impl SteeringReceipt {
    /// Whether this input is still eligible for recall. This is a snapshot;
    /// only [`Self::try_retract`] establishes that recall actually won.
    pub fn is_pending(&self) -> bool {
        self.payload
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    /// Removes this input before its persistence claim, returning true only
    /// for the caller that won recall. Success guarantees no session append or
    /// delivery event for this input and releases any admission reservation.
    ///
    /// Returns false once delivery has claimed the input, even if persistence
    /// is still in progress or later fails; also returns false after an earlier
    /// recall or after the submission is dropped. Receipt clones share this
    /// same one-shot authority. No lock is held during filesystem persistence.
    pub fn try_retract(&self) -> bool {
        let payload = self
            .payload
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if payload.is_none() {
            return false;
        }
        drop(payload);
        self.recalled.cancel();
        true
    }
}

pub(super) enum Control {
    SetReasoning(ReasoningConfig),
    Steer(ReservedInput),
    FollowUp(ReservedInput),
    AppendCustom(ReservedInput),
    FinishNow(ReservedInput),
    SetSteeringMode(QueueDeliveryMode),
    SetFollowUpMode(QueueDeliveryMode),
    Abort,
}

/// Logical retained payload bytes, including part slots, media, references and
/// transcripts. Count inline data directly rather than allocating base64 JSON.
pub(super) fn control_input_bytes(input: &UserInput) -> usize {
    input
        .parts
        .iter()
        .fold(
            input
                .parts
                .len()
                .saturating_mul(std::mem::size_of::<InputPart>()),
            |total, part| {
                let bytes = match part {
                    InputPart::Text(text) => text.len(),
                    InputPart::Media(Media::Image(image)) => {
                        let source = match &image.source {
                            ImageSource::Inline(data) => data.len(),
                            ImageSource::Url(url) => url.as_str().len(),
                            ImageSource::ProviderRef(reference) => reference.id.len(),
                        };
                        source.saturating_add(
                            image
                                .media_type
                                .as_ref()
                                .map_or(0, |mime| mime.as_ref().len()),
                        )
                    }
                    InputPart::Media(Media::Audio(audio)) => {
                        let source = match &audio.payload {
                            AudioPayload::Inline(data) => data.len(),
                            AudioPayload::ProviderRef(reference) => reference.id.len(),
                            AudioPayload::InlineWithProviderRef { data, reference } => {
                                data.len().saturating_add(reference.id.len())
                            }
                        };
                        source.saturating_add(audio.transcript.as_ref().map_or(0, String::len))
                    }
                };
                total.saturating_add(bytes)
            },
        )
        .saturating_add(
            input
                .custom_messages
                .iter()
                .map(|message| {
                    serde_json::to_vec(message)
                        .expect("custom message JSON")
                        .len()
                })
                .sum::<usize>(),
        )
}

/// Clonable control handle for an active [`Run`].
///
/// Steering, follow-up and FinishNow share a 64-input / 64-MiB logical payload
/// budget, including inputs drained into pending delivery batches. Saturation
/// returns [`AgentError::ControlQueueFull`] before acceptance. Successful sends
/// remain reserved until durable delivery or run termination; cancellation
/// bypasses this queue entirely.
#[derive(Clone)]
pub struct RunControl {
    pub(super) cache_warming_mode: tokio::sync::watch::Sender<crate::cache_warmer::CacheWarmPolicy>,
    pub(super) cache_warming_status: tokio::sync::watch::Receiver<crate::CacheWarmingStatus>,
    pub(super) reasoning_model: Option<Model>,
    pub(super) ultra_observed: bool,
    pub(super) admission: Arc<std::sync::Mutex<bool>>,
    pub(super) tx: mpsc::Sender<Control>,
    pub(super) pending_count: Arc<tokio::sync::Semaphore>,
    pub(super) pending_bytes: Arc<tokio::sync::Semaphore>,
    pub(super) abort: Arc<AbortFlag>,
}

impl RunControl {
    /// Reconcile the user-owned warming mode while a run is active. This
    /// coalesces without queueing prompt data; the session owner applies it on
    /// its next poll. Persistence errors are reported by the run's event stream.
    pub fn set_cache_warming_mode(&self, mode: crate::CacheWarmMode) -> Result<(), AgentError> {
        let admitted = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admitted || self.tx.is_closed() {
            return Err(AgentError::RunEnded);
        }
        self.cache_warming_mode
            .send_modify(|policy| policy.set_mode(mode));
        Ok(())
    }

    /// Latest host-selected warming mode, including a pending active change.
    pub fn cache_warming_mode(&self) -> crate::CacheWarmMode {
        self.cache_warming_mode.borrow().mode
    }

    /// Live, payload-free diagnostics published by this run's session owner.
    pub fn cache_warming_status(&self) -> crate::CacheWarmingStatus {
        if self.cache_warming_mode() == crate::CacheWarmMode::Off {
            crate::CacheWarmingStatus::inactive("cache warming disabled")
        } else {
            self.cache_warming_status.borrow().clone()
        }
    }

    /// Queues a host-authoritative effort change without interrupting generation.
    /// The latest pending selection applies at the next response boundary;
    /// acceptance is not a provider acknowledgement.
    pub async fn set_reasoning(&self, reasoning: ReasoningConfig) -> Result<(), AgentError> {
        let model = self.reasoning_model.as_ref().ok_or_else(|| {
            AgentError::Ai(
                octet_ai::ConfigError::Parse(
                    "reasoning updates require a qualified Responses route".into(),
                )
                .into(),
            )
        })?;
        if reasoning == ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra) {
            require_ultra_observation(&reasoning, self.ultra_observed)?;
            octet_ai::responses::validate_responses_input(
                model,
                &ResponsesInput::default(),
                &reasoning,
                false,
            )?;
        } else {
            validate_reasoning_update(model, &reasoning)?;
        }
        let permit = self.tx.reserve().await.map_err(|_| AgentError::RunEnded)?;
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(Control::SetReasoning(reasoning));
        Ok(())
    }

    pub(super) fn reserve_input(&self, input: UserInput) -> Result<ReservedInput, AgentError> {
        let reservation = self.reserve_input_capacity(&input)?;
        Ok(ReservedInput::Ready(ReservedPayload {
            input,
            reservation: Some(reservation),
        }))
    }

    pub(super) fn reserve_input_capacity(
        &self,
        input: &UserInput,
    ) -> Result<ControlReservation, AgentError> {
        let bytes = control_input_bytes(input);
        if bytes > MAX_PENDING_CONTROL_BYTES {
            return Err(AgentError::ControlQueueFull);
        }
        let count = self
            .pending_count
            .clone()
            .try_acquire_owned()
            .map_err(|_| AgentError::ControlQueueFull)?;
        let bytes = self
            .pending_bytes
            .clone()
            .try_acquire_many_owned(bytes as u32)
            .map_err(|_| AgentError::ControlQueueFull)?;
        Ok(ControlReservation {
            _count: count,
            _bytes: bytes,
        })
    }

    pub(super) fn reserve_control(
        &self,
        control: UnreservedControl,
    ) -> Result<Control, AgentError> {
        if self.tx.is_closed()
            || !*self
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        {
            return Err(AgentError::RunEnded);
        }
        Ok(match control {
            UnreservedControl::Steer(input) => Control::Steer(self.reserve_input(input)?),
            UnreservedControl::FollowUp(input) => Control::FollowUp(self.reserve_input(input)?),
            UnreservedControl::FinishNow(input) => Control::FinishNow(self.reserve_input(input)?),
            UnreservedControl::SetSteeringMode(mode) => Control::SetSteeringMode(mode),
            UnreservedControl::SetFollowUpMode(mode) => Control::SetFollowUpMode(mode),
            UnreservedControl::Abort => Control::Abort,
        })
    }

    pub(super) async fn send(&self, control: UnreservedControl) -> Result<(), AgentError> {
        let control = self.reserve_control(control)?;
        let permit = self.tx.reserve().await.map_err(|_| AgentError::RunEnded)?;
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(control);
        Ok(())
    }

    /// Queue a context-only custom message for the safe end-of-turn boundary.
    /// This admission never requests another model call.
    pub fn try_append_custom(
        &self,
        message: crate::session::CustomMessage,
    ) -> Result<(), AgentError> {
        let input = self.reserve_input(UserInput::from_custom(message))?;
        let permit = self.tx.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => AgentError::ControlQueueFull,
            mpsc::error::TrySendError::Closed(_) => AgentError::RunEnded,
        })?;
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(Control::AppendCustom(input));
        Ok(())
    }

    pub(super) fn try_send(&self, control: UnreservedControl) -> Result<(), AgentError> {
        let control = self.reserve_control(control)?;
        let permit = self.tx.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => AgentError::ControlQueueFull,
            mpsc::error::TrySendError::Closed(_) => AgentError::RunEnded,
        })?;
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(control);
        Ok(())
    }

    /// Injects input into the conversation at the next model-turn boundary of
    /// the active run (persisted to the session when applied).
    pub async fn steer(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.send(UnreservedControl::Steer(input.into())).await
    }

    /// Reserves steering capacity synchronously and returns a submission plus
    /// its receipt before asynchronous sending starts. A frontend can retain
    /// its draft on admission failure. Send through this control (or a clone);
    /// dropping or recalling the prepared value immediately frees capacity.
    pub fn prepare_steer(
        &self,
        input: impl Into<UserInput>,
    ) -> Result<(PreparedSteering, SteeringReceipt), AgentError> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission || self.tx.is_closed() {
            return Err(AgentError::RunEnded);
        }
        let input = input.into();
        let reservation = self.reserve_input_capacity(&input)?;
        let (mut prepared, receipt) = PreparedSteering::new(input);
        prepared
            .receipt
            .payload
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
            .expect("new prepared input")
            .reservation = Some(reservation);
        prepared.owner = Some(self.pending_count.clone());
        Ok((prepared, receipt))
    }

    /// Submits a prepared, retractable steering input at the next safe boundary.
    ///
    /// The receipt can recall local intent, an in-flight send, or accepted
    /// pending input. A recalled submission completes successfully as a no-op;
    /// `Ok(())` is admission, not durable delivery. Normal control budgets and
    /// run-end admission fencing still apply. Cancelling this future before
    /// admission drops its input and releases its reservations. Submitting an
    /// input reserved by a different run returns [`AgentError::RunEnded`].
    pub async fn steer_retractable(&self, prepared: PreparedSteering) -> Result<(), AgentError> {
        if prepared
            .owner
            .as_ref()
            .is_some_and(|owner| !Arc::ptr_eq(owner, &self.pending_count))
        {
            return Err(AgentError::RunEnded);
        }
        if !prepared.receipt.is_pending() {
            return Ok(());
        }
        if self.tx.is_closed()
            || !*self
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        {
            return Err(AgentError::RunEnded);
        }
        {
            let mut payload = prepared
                .receipt
                .payload
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(payload) = payload.as_mut() else {
                return Ok(());
            };
            if payload.reservation.is_none() {
                payload.reservation = Some(self.reserve_input_capacity(&payload.input)?);
            }
        }
        let permit = tokio::select! {
            biased;
            _ = prepared.receipt.recalled.cancelled() => return Ok(()),
            permit = self.tx.reserve() => permit.map_err(|_| AgentError::RunEnded)?,
        };
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(Control::Steer(ReservedInput::Retractable(prepared)));
        Ok(())
    }

    /// Attempts to enqueue steering without allowing a producer to wait behind
    /// the run's bounded control queue.
    pub(crate) fn try_steer(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.try_send(UnreservedControl::Steer(input.into()))
    }

    /// Queues input for after the current run settles: when the model completes
    /// a turn without tool calls, the run continues with this input instead of
    /// finishing.
    pub async fn follow_up(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.send(UnreservedControl::FollowUp(input.into())).await
    }

    /// Requests a final answer at the next safe turn boundary. The supplied
    /// input is persisted like steering, but subsequent requests in this run
    /// expose no tools.
    pub async fn finish_now(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.send(UnreservedControl::FinishNow(input.into())).await
    }

    /// Attempts to enqueue a follow-up without allowing a producer to wait
    /// behind the run's bounded control queue.
    pub fn try_follow_up(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.try_send(UnreservedControl::FollowUp(input.into()))
    }

    /// Changes how pending steering messages are delivered.
    pub async fn set_steering_mode(&self, mode: QueueDeliveryMode) -> Result<(), AgentError> {
        self.send(UnreservedControl::SetSteeringMode(mode)).await
    }

    /// Changes how pending follow-up messages are delivered.
    pub async fn set_follow_up_mode(&self, mode: QueueDeliveryMode) -> Result<(), AgentError> {
        self.send(UnreservedControl::SetFollowUpMode(mode)).await
    }

    /// Aborts the run at the next safe boundary: the in-flight model stream is
    /// dropped (cancelling the request) or the running tool is cancelled (child
    /// processes killed). All already-completed session entries are preserved
    /// and the run finishes with exactly one
    /// [`AgentEvent::RunFinished`]`{ reason: FinishReason::Aborted }`.
    pub fn abort(&self) {
        self.abort.set();
    }
}

/// Level-triggered abort signal: reliable regardless of channel capacity and
/// observable both by polling (`is_set`) and awaiting (`wait`).
#[derive(Default)]
pub(super) struct AbortFlag {
    pub(super) set: AtomicBool,
    pub(super) notify: tokio::sync::Notify,
    pub(super) cancellation: CancellationToken,
}

impl AbortFlag {
    pub(super) fn set(&self) {
        self.set.store(true, Ordering::Release);
        self.cancellation.cancel();
        self.notify.notify_waiters();
    }

    pub(super) fn is_set(&self) -> bool {
        self.set.load(Ordering::Acquire) || self.cancellation.is_cancelled()
    }

    pub(super) async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_set() {
                return;
            }
            tokio::select! {
                _ = notified => {},
                _ = self.cancellation.cancelled() => return,
            }
        }
    }
}

pub(super) async fn append_context_inputs(
    pending: &mut Vec<ReservedInput>,
    session: &mut Session,
    model: &Model,
) -> Result<(), AgentError> {
    for queued in std::mem::take(pending) {
        if let Some(ReservedPayload { input, reservation }) = queued.claim() {
            let input = prepare_user_images(input, model, None).await?;
            input.append_to(session, None)?;
            drop(reservation);
        }
    }
    Ok(())
}

/// Which queue a batch of control inputs came from; only the announced event
/// differs between steering and follow-up delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ControlDeliveryKind {
    Steering,
    FollowUp,
}

impl ControlDeliveryKind {
    pub(super) fn delivered_event(self, messages: Vec<String>) -> AgentEvent {
        match self {
            Self::Steering => AgentEvent::SteeringDelivered { messages },
            Self::FollowUp => AgentEvent::FollowUpDelivered { messages },
        }
    }
}

/// Outcome of appending one queued control-input batch (steering or
/// follow-up) to the session.
pub(super) enum ControlDelivery {
    /// Every input was appended; announce them with this event when present.
    Completed { event: Option<AgentEvent> },
    /// Persistence failed. Any prefix that did reach the session is announced
    /// by `event`; the run must then end with `finish`.
    Interrupted {
        event: Option<AgentEvent>,
        finish: FinishReason,
    },
}

/// Snapshot of everything `observe_context_tracker` needs to re-observe the
/// tracker after control inputs change the session.
pub(super) struct ContextObservation<'a> {
    pub(super) tracker: &'a ContextTracker,
    pub(super) model: &'a Model,
    pub(super) system: &'a str,
    pub(super) tools: &'a [ToolDef],
}

impl ContextObservation<'_> {
    pub(super) fn observe(&self, session: &Session) -> Result<ContextBreakdown, SessionError> {
        observe_context_tracker(self.tracker, session, self.model, self.system, self.tools)
    }
}

pub(super) async fn next_delegation_snapshot(
    receiver: &mut watch::Receiver<Option<DelegationTelemetrySnapshot>>,
) -> Option<DelegationTelemetrySnapshot> {
    receiver.changed().await.ok()?;
    receiver.borrow_and_update().clone()
}

/// Append a batch of already-queued control inputs as durable user messages
/// and report what was delivered.
///
/// When enabled, bounded evidence is recorded for the terminal gate before
/// its append is attempted. Frontend delivery summaries remain complete.
/// On append failure the context tracker is still observed (its error ignored)
/// so observers see the partial delivery before the run ends.
pub(super) async fn deliver_control_inputs(
    queued: Vec<ReservedInput>,
    kind: ControlDeliveryKind,
    session: &mut Session,
    metadata: &EntryMetadata,
    terminal_gate_evidence: &mut Option<TerminalGateEvidence>,
    observation: &ContextObservation<'_>,
    abort: Option<&AbortFlag>,
) -> ControlDelivery {
    let mut delivered = Vec::with_capacity(queued.len());
    for queued in queued {
        // Linearize recall against delivery BEFORE any evidence or durable
        // write. The payload and permits leave receipt ownership together;
        // recall cannot succeed after this claim, including during fsync.
        let Some(ReservedPayload { input, reservation }) = queued.claim() else {
            continue;
        };
        let input = match prepare_user_images(input, observation.model, abort).await {
            Ok(input) => input,
            Err(error) => {
                let event = (!delivered.is_empty())
                    .then(|| kind.delivered_event(std::mem::take(&mut delivered)));
                return ControlDelivery::Interrupted {
                    event,
                    finish: if matches!(error, AgentError::Cancelled) {
                        FinishReason::Aborted
                    } else {
                        FinishReason::Failed(error)
                    },
                };
            }
        };
        let summary = input.text_summary();
        if let Some(evidence) = terminal_gate_evidence {
            evidence.record_request(&summary);
        }
        let display = input.display_summary();
        if let Err(e) = input.append_to(session, Some(metadata.clone())) {
            let event = (!delivered.is_empty())
                .then(|| kind.delivered_event(std::mem::take(&mut delivered)));
            let _ = observation.observe(session);
            return ControlDelivery::Interrupted {
                event,
                finish: FinishReason::Failed(e.into()),
            };
        }
        delivered.push(display);
        // Never free admission capacity merely because ingress was drained.
        // Both permits remain live through the successful durable append.
        drop(reservation);
    }
    if !delivered.is_empty() {
        if let Err(error) = observation.observe(session) {
            return ControlDelivery::Interrupted {
                event: Some(kind.delivered_event(delivered)),
                finish: FinishReason::Failed(error.into()),
            };
        }
        return ControlDelivery::Completed {
            event: Some(kind.delivered_event(delivered)),
        };
    }
    ControlDelivery::Completed { event: None }
}

/// Observe only newly committed entries; resume/history never emits fresh message events.
pub(super) fn committed_custom_message_events(
    session: &Session,
    cursor: &mut usize,
) -> Vec<AgentEvent> {
    let events = session.entries()[*cursor..]
        .iter()
        .filter_map(|entry| {
            let message = entry.metadata.as_ref()?.custom_message.as_ref()?;
            Some(AgentEvent::CustomMessageCommitted {
                entry_id: entry.id.clone(),
                message: message.clone(),
                timestamp_unix_ms: entry
                    .timestamp_unix_ms
                    .expect("fresh durable entry timestamp"),
            })
        })
        .collect();
    *cursor = session.entries().len();
    events
}
