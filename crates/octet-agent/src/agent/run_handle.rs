//! The `Run` stream handle a prompt returns, with its output and context estimate.

use super::*;

/// Aggregate result of [`Agent::complete`].
#[derive(Debug)]
pub struct RunOutput {
    /// Concatenated visible text from all turns.
    pub text: String,
    /// Completed generated media from committed turns, in event order.
    pub media: Vec<Media>,
    /// Total token usage across the run.
    pub usage: Usage,
    /// Known microdollar subtotal for this run, not a full bill when
    /// `Session::has_unpriced_usage` or `Session::has_uncertain_usage` is true.
    pub cost_microdollars: u64,
    /// Session entry ID after the run.
    pub head: EntryId,
    /// How the run ended (never [`FinishReason::Failed`]; failures are
    /// returned as `Err` instead).
    pub reason: FinishReason,
}

/// Conservative estimate of the model-visible input for the next request.
///
/// `structural_tokens` comes from octet's request serializer. When available,
/// `provider_tokens` is the latest tokenizer measurement for the same route
/// and model after the latest compaction, plus structurally estimated trailing
/// messages. `input_tokens` is the larger of those two values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestContextEstimate {
    /// Structural estimate of the complete provider request.
    pub structural_tokens: u64,
    /// Provider-authoritative prefix measurement reconciled to the current head.
    pub provider_tokens: Option<u64>,
    /// Conservative input estimate used by autonomous capacity checks.
    pub input_tokens: u64,
}

/// A streaming agent run: the event stream plus a clonable control handle.
///
/// The run is driven by the caller — poll it with [`Run::next`] (or as a
/// [`Stream`]), typically inside `tokio::select!` alongside user input.
/// Dropping the run cancels the in-flight model stream and any running tool
/// (child processes included).
pub struct Run<'a> {
    pub(super) stream: Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'a>>,
    pub(super) control: RunControl,
    pub(super) lifecycle: Arc<RunLifecycle>,
    pub(super) context: Arc<ContextTracker>,
    pub(super) delegation: Option<DelegationBinding>,
}

impl Run<'_> {
    /// Open an extension-negotiated child session by its opaque presentation
    /// reference while this run is active.
    ///
    /// Mirrors [`Agent::open_delegated_session_reference`] so live UI (for
    /// example the mid-run `/subagents` transcript drill-in) can read a worker
    /// transcript read-only without owning the root session. The delegation
    /// manager state lock is only taken to resolve the reference to a path.
    pub fn open_delegated_session_reference(
        &self,
        extension_principal: &str,
        reference: &str,
    ) -> Result<Option<Session>, AgentError> {
        let Some(binding) = self.delegation.as_ref() else {
            return Ok(None);
        };
        binding.open_session_reference(extension_principal, reference)
    }

    /// Returns a clonable handle for sending control messages while the run's
    /// event stream is being consumed.
    pub fn control(&self) -> RunControl {
        self.control.clone()
    }

    /// Returns an owned snapshot of incrementally tracked response,
    /// tool-boundary, and provider token-usage state.
    /// Session-owned delegation handle, when delegation is enabled.
    ///
    /// Host-side callers use it to resolve a worker's launchable interactive
    /// handle (`agent-session:<sha256>` -> session-owned transcript) without
    /// widening what an extension can see.
    pub fn session_delegation(&self) -> Option<SessionDelegationHandle> {
        self.delegation
            .as_ref()
            .map(DelegationBinding::session_handle)
    }

    /// Returns an owned snapshot of incrementally tracked response,
    /// tool-boundary, and provider token-usage state.
    pub fn context_snapshot(&self) -> ContextSnapshot {
        self.context.snapshot()
    }

    /// Consumes the run and returns its settled context snapshot.
    ///
    /// An unfinished run is first marked as dropped, matching the normal
    /// cancellation semantics of [`Drop`]. A run that already delivered its
    /// terminal event retains that terminal state.
    pub fn into_context_snapshot(self) -> ContextSnapshot {
        let context = Arc::clone(&self.context);
        drop(self);
        context.snapshot()
    }

    /// Returns the next event, or `None` after the terminal
    /// [`AgentEvent::RunFinished`] has been delivered.
    pub async fn next(&mut self) -> Option<AgentEvent> {
        self.stream.next().await
    }
}

impl Drop for Run<'_> {
    fn drop(&mut self) {
        // Serialize retained-policy changes with releasing this run's ownership.
        // Checking channel closure alone leaves a setter/drop race.
        *self
            .control
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        if !self.lifecycle.finished.load(Ordering::Acquire) {
            self.lifecycle.dropped.store(true, Ordering::Release);
            self.context.run_dropped();
            if let Some(delegation) = &self.delegation {
                // A dropped run ends the turn, not the fleet. Workers are
                // owned by the session and stay reattachable.
                delegation.detach_run();
            }
        }
    }
}

impl Stream for Run<'_> {
    type Item = AgentEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.stream.as_mut().poll_next(cx)
    }
}
