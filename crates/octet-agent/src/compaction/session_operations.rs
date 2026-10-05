//! Cancellable session-operation interception serviced by the sole Session owner.
//!
//! This is deliberately separate from bitmap compaction strategies. Process
//! implementations receive an immutable event and a private append producer;
//! only this driver ever supplies mutable persistence authority. A veto is not a
//! successful compaction, and an after-event is dispatched only after commit.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::tool::CancellationToken;
use serde::{Deserialize, Serialize};

use super::{HandoffPreparation, MAX_COMPACTION_HANDOFF_BYTES};
use crate::session::{Entry, EntryId, Session};
use crate::tools::deferred::DeferredRunStore;

/// What caused a host-owned compaction, independently of the active frontend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCompactionReason {
    /// An explicitly requested compaction.
    Manual,
    /// The configured proactive context threshold was crossed.
    Threshold,
    /// Local capacity or a provider context-overflow response required recovery.
    Overflow,
}

/// Actual prepared or committed session state, never a synthesized notification.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionOperation {
    /// Intercepts a prepared local textual compaction before any summary call.
    BeforeCompact {
        /// Actual trigger.
        reason: SessionCompactionReason,
        /// Host-selected safe retained boundary.
        first_kept: EntryId,
        /// Owned transcript input, including prior summary and file details.
        preparation: HandoffPreparation,
        /// Full source branch, oldest first.
        branch_entries: Vec<Entry>,
        /// Optional instructions from the initiating command.
        custom_instructions: Option<String>,
    },
    /// Observes one durably committed compaction entry.
    Compacted {
        /// Actual trigger.
        reason: SessionCompactionReason,
        /// The durable record, including its real ID and parent.
        entry: Entry,
        /// Whether an interceptor supplied the textual replacement.
        from_extension: bool,
    },
    /// Intercepts a checkout before any durable head mutation.
    BeforeTree {
        /// Requested entry; None selects the empty root.
        target_id: Option<EntryId>,
        /// Source head, before the operation.
        old_head: Option<EntryId>,
    },
    /// Observes the preparation boundary of one logical model iteration.
    /// Retries and auxiliary model requests do not begin another iteration.
    ModelTurnStart {
        /// Owning run identity, derived from its durable initiating user entry.
        run_id: String,
        /// Zero-based logical model iteration within this run.
        turn_index: u64,
        /// Actual host wall-clock time at this boundary, in Unix milliseconds.
        timestamp_ms: u64,
    },
    /// Observes a durable assistant and all of its settled tool results.
    /// This is not the earlier, provider-completion `AgentEvent::TurnFinished`.
    ModelTurnEnd {
        /// Same owning run identity as the corresponding start observation.
        run_id: String,
        /// Zero-based logical model iteration within this run.
        turn_index: u64,
        /// Actual host wall-clock time at this boundary, in Unix milliseconds.
        timestamp_ms: u64,
        /// Actual persisted assistant entry; never a reconstructed Pi message.
        assistant_entry: Entry,
        /// Actual paired result entries in durable commit order, including errors.
        tool_result_entries: Vec<Entry>,
    },
    /// Observes an actual durable checkout, not a frontend selection.
    Tree {
        /// Previously selected head.
        old_head: Option<EntryId>,
        /// Durably selected head.
        new_head: Option<EntryId>,
    },
}

/// A textual replacement anchored in the prepared source branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCompactionReplacement {
    /// Complete bounded replacement, retained verbatim.
    pub summary: String,
    /// Oldest source entry to retain verbatim.
    pub first_kept: EntryId,
}

/// Awaited native session decision. Cancellation never requests native fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionOperationDecision {
    /// No interception; preserve any earlier replacement. This is the only
    /// valid decision for model-turn and other post-commit observations.
    #[default]
    Continue,
    /// Explicit veto; do not commit or invoke the native summarizer.
    Cancel,
    /// Replace a local textual compaction, not a bitmap/provider checkpoint.
    ReplaceCompaction {
        /// Validated against the original prepared source branch.
        replacement: SessionCompactionReplacement,
    },
}

/// Owned callback, independent of a borrowed Session or invocation guard.
pub type SessionOperationFuture =
    Pin<Box<dyn Future<Output = Result<SessionOperationDecision, String>> + Send + 'static>>;

/// One activated callback and optional authenticated private append lane.
pub trait SessionOperationInvocation: Send {
    /// Called once before polling the callback. No Session is borrowed by it.
    fn take_future(&mut self) -> SessionOperationFuture;

    /// Readiness for the private append lane. Native read-only hooks stay pending.
    fn ready(&self) -> Pin<Box<dyn Future<Output = bool> + Send + 'static>> {
        Box::pin(std::future::pending())
    }

    /// Commit exactly one claimed append on the original Session writer.
    fn consume_next(&mut self, _session: &mut Session) -> Result<(), String> {
        Err("session operation has no append consumer".into())
    }

    /// Recheck process generation/owner even when the callback appended nothing.
    fn validate_current(&self, _session: &Session) -> Result<(), String> {
        Ok(())
    }
}

/// Generic session interceptor. Installed only where the owning driver polls it.
pub trait SessionOperationHook: Send + Sync {
    /// None means this implementation does not subscribe to this event.
    /// Returning a guard must install its append producer before callback polling.
    fn begin(
        &self,
        session: &Session,
        operation: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String>;
}

/// Payload-free operation failures; a post-commit failure cannot undo history.
#[derive(Debug, thiserror::Error)]
pub enum SessionOperationError {
    /// Host cancellation won. Already committed entries are never rolled back.
    #[error("session operation cancelled")]
    Cancelled,
    /// A callback exceeded the shared operation deadline.
    #[error("session operation hook deadline exceeded")]
    Deadline,
    /// An extension or its append service failed; no automatic fallback.
    #[error("session operation hook failed")]
    Hook,
    /// The prepared source is no longer the exact Session revision.
    #[error("session operation source changed")]
    StaleSource,
    /// A returned decision is not meaningful or safe at this boundary.
    #[error("invalid session operation decision")]
    InvalidDecision,
    /// The original descriptor could not be inspected.
    #[error("session operation persistence unavailable")]
    Persistence,
}

/// Exact writer incarnation and durable source revision, including A→B→A checkouts.
/// This is a host-only witness, never extension-issued authority.
pub struct SessionSourceRevision {
    incarnation: Weak<DeferredRunStore>,
    head: Option<EntryId>,
    entries: usize,
    bytes: u64,
}

impl SessionSourceRevision {
    /// Capture the already-open writer; never open another writable Session.
    pub fn capture(session: &Session) -> Result<Self, SessionOperationError> {
        Ok(Self {
            incarnation: Arc::downgrade(&session.deferred_run_store()),
            head: session.head(),
            entries: session.entries().len(),
            bytes: session
                .try_clone_file()
                .and_then(|file| file.metadata())
                .map_err(|_| SessionOperationError::Persistence)?
                .len(),
        })
    }

    /// Revalidate immediately before a mutation/decision is adopted.
    pub fn validate(&self, session: &Session) -> Result<(), SessionOperationError> {
        let current = Self::capture(session)?;
        if !self.incarnation.ptr_eq(&current.incarnation)
            || self.head != current.head
            || self.entries != current.entries
            || self.bytes != current.bytes
        {
            return Err(SessionOperationError::StaleSource);
        }
        Ok(())
    }
}

/// Walk the selected parent chain. Abandoned branches are never preparation input.
pub fn session_operation_branch(session: &Session) -> Result<Vec<Entry>, SessionOperationError> {
    let mut branch = Vec::new();
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let entry = session
            .entry(&id)
            .ok_or(SessionOperationError::StaleSource)?;
        cursor = entry.parent.clone();
        branch.push(entry.clone());
    }
    branch.reverse();
    Ok(branch)
}

fn validate_decision(
    operation: &SessionOperation,
    decision: &SessionOperationDecision,
) -> Result<(), SessionOperationError> {
    match (operation, decision) {
        (_, SessionOperationDecision::Continue) => Ok(()),
        (
            SessionOperation::BeforeCompact { .. } | SessionOperation::BeforeTree { .. },
            SessionOperationDecision::Cancel,
        ) => Ok(()),
        (
            SessionOperation::BeforeCompact { branch_entries, .. },
            SessionOperationDecision::ReplaceCompaction { replacement },
        ) if !replacement.summary.trim().is_empty()
            && replacement.summary.len() <= MAX_COMPACTION_HANDOFF_BYTES
            && branch_entries.iter().any(|entry| entry.id == replacement.first_kept
                && matches!(&entry.value, crate::session::EntryValue::Message(message)
                    if matches!(message, octet_ai::Message::Assistant(_)) || super::is_turn_start_user(message))) => Ok(()),
        _ => Err(SessionOperationError::InvalidDecision),
    }
}

/// Drive ordered callbacks without giving any transport task mutable Session access.
/// Each guard is dropped before the next callback, revoking unused append grants.
/// Errors, timeouts, and explicit vetoes never turn into a successful no-op event.
pub async fn run_session_operation_hooks(
    session: &mut Session,
    hooks: &[Arc<dyn SessionOperationHook>],
    operation: &SessionOperation,
    cancellation: &CancellationToken,
    timeout: Duration,
) -> Result<SessionOperationDecision, SessionOperationError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut selected = SessionOperationDecision::Continue;
    let mut revision = SessionSourceRevision::capture(session)?;
    for hook in hooks {
        if cancellation.is_cancelled() {
            return Err(SessionOperationError::Cancelled);
        }
        revision.validate(session)?;
        let Some(mut invocation) = hook
            .begin(session, operation)
            .map_err(|_| SessionOperationError::Hook)?
        else {
            continue;
        };
        let mut future = invocation.take_future();
        let decision = loop {
            let ready = invocation.ready();
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(SessionOperationError::Cancelled),
                _ = tokio::time::sleep_until(deadline) => return Err(SessionOperationError::Deadline),
                live = ready => {
                    if !live { return Err(SessionOperationError::Hook); }
                    revision.validate(session)?;
                    invocation.consume_next(session).map_err(|_| SessionOperationError::Hook)?;
                    // Only this known committed leaf may advance the prepared source.
                    revision = SessionSourceRevision::capture(session)?;
                },
                result = &mut future => break result.map_err(|_| SessionOperationError::Hook)?,
            }
        };
        invocation
            .validate_current(session)
            .map_err(|_| SessionOperationError::StaleSource)?;
        revision.validate(session)?;
        if cancellation.is_cancelled() {
            return Err(SessionOperationError::Cancelled);
        }
        validate_decision(operation, &decision)?;
        match decision {
            SessionOperationDecision::Continue => {}
            SessionOperationDecision::Cancel => return Ok(SessionOperationDecision::Cancel),
            replacement => selected = replacement,
        }
    }
    if cancellation.is_cancelled() {
        return Err(SessionOperationError::Cancelled);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests;
