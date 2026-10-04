//! Awaited observations of logical model turns, distinct from provider attempts.
//!
//! Keep only durable entry identities while tools settle. Async observations may
//! overlap the next response, so provider completion alone cannot end a turn.
use super::*;
use crate::compaction::{
    run_session_operation_hooks, SessionOperation, SessionOperationError, SessionOperationHook,
};

const MODEL_TURN_HOOK_TIMEOUT: Duration = Duration::from_secs(120);

struct PendingTurn {
    index: u64,
    assistant: EntryId,
    unresolved: HashSet<octet_ai::ToolCallId>,
    results: Vec<EntryId>,
    next_entry: usize,
}

pub(super) struct ModelTurnHooks<'a> {
    hooks: &'a [Arc<dyn SessionOperationHook>],
    run_id: &'a str,
    started: Option<u64>,
    pending: VecDeque<PendingTurn>,
    stopped: bool,
}

impl<'a> ModelTurnHooks<'a> {
    pub(super) fn new(hooks: &'a [Arc<dyn SessionOperationHook>], run_id: &'a str) -> Self {
        Self {
            hooks,
            run_id,
            started: None,
            pending: VecDeque::new(),
            stopped: false,
        }
    }

    /// Called before preparation, once per logical iteration even if context
    /// compaction or transport recovery re-enters the preparation loop.
    pub(super) async fn start(
        &mut self,
        session: &mut Session,
        index: u64,
        cancellation: &CancellationToken,
    ) -> Result<(), FinishReason> {
        if self.hooks.is_empty() || self.stopped || self.started == Some(index) {
            return Ok(());
        }
        self.started = Some(index);
        self.dispatch(
            session,
            SessionOperation::ModelTurnStart {
                run_id: self.run_id.to_owned(),
                turn_index: index,
                timestamp_ms: now_unix_millis(),
            },
            cancellation,
        )
        .await
    }

    /// Register only after the real assistant append succeeds. No provisional
    /// output, auxiliary completion, or synthetic failure message enters here.
    pub(super) fn committed(
        &mut self,
        session: &Session,
        index: u64,
        assistant: &EntryId,
        calls: &[ToolCall],
    ) {
        if self.hooks.is_empty() || self.stopped {
            return;
        }
        self.pending.push_back(PendingTurn {
            index,
            assistant: assistant.clone(),
            unresolved: calls.iter().map(|call| call.id.clone()).collect(),
            results: Vec::with_capacity(calls.len()),
            next_entry: session.entries().len(),
        });
    }

    /// Inspect only newly appended entries, then deliver completed turns in
    /// iteration order. This never waits for unfinished tools or changes their
    /// execution/commit order. The actual owner continues driving async tools.
    pub(super) async fn settle(
        &mut self,
        session: &mut Session,
        cancellation: &CancellationToken,
    ) -> Result<(), FinishReason> {
        if self.hooks.is_empty() || self.stopped {
            return Ok(());
        }
        for turn in &mut self.pending {
            for entry in &session.entries()[turn.next_entry..] {
                let EntryValue::Message(Message::User(message)) = &entry.value else {
                    continue;
                };
                let mut paired = false;
                for part in &message.content {
                    if let UserPart::ToolResult(result) = part {
                        paired |= turn.unresolved.remove(&result.tool_call_id);
                    }
                }
                if paired {
                    turn.results.push(entry.id.clone());
                }
            }
            turn.next_entry = session.entries().len();
        }
        while self
            .pending
            .front()
            .is_some_and(|turn| turn.unresolved.is_empty())
        {
            // Claim before awaiting. Errors/cancellation are terminal for this
            // driver; terminal cleanup must not redeliver a committed turn.
            let turn = self.pending.pop_front().expect("ready model turn");
            let operation = SessionOperation::ModelTurnEnd {
                run_id: self.run_id.to_owned(),
                turn_index: turn.index,
                timestamp_ms: now_unix_millis(),
                assistant_entry: session
                    .entry(&turn.assistant)
                    .expect("committed assistant remains in append-only session")
                    .clone(),
                tool_result_entries: turn
                    .results
                    .iter()
                    .map(|id| {
                        session
                            .entry(id)
                            .expect("committed tool result remains in append-only session")
                            .clone()
                    })
                    .collect(),
            };
            self.dispatch(session, operation, cancellation).await?;
        }
        Ok(())
    }

    async fn dispatch(
        &mut self,
        session: &mut Session,
        operation: SessionOperation,
        cancellation: &CancellationToken,
    ) -> Result<(), FinishReason> {
        let result = run_session_operation_hooks(
            session,
            self.hooks,
            &operation,
            cancellation,
            MODEL_TURN_HOOK_TIMEOUT,
        )
        .await;
        match result {
            Ok(_) => Ok(()), // The shared driver validates Continue-only observations.
            Err(error) => {
                self.stopped = true;
                if matches!(error, SessionOperationError::Cancelled) {
                    return Err(FinishReason::Aborted);
                }
                let boundary = match &operation {
                    SessionOperation::ModelTurnEnd { assistant_entry, .. } => format!(
                        "model turn hook failed after committed assistant {}; durable entries retained; callback not retried",
                        assistant_entry.id.0
                    ),
                    _ => "model turn start hook failed; durable entries retained; callback not retried".into(),
                };
                Err(FinishReason::Failed(
                    AiError::Config(octet_ai::ConfigError::Parse(format!("{boundary}: {error}")))
                        .into(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests;
