//! Bounded, ordered child observations. These are native facts, not a second
//! agent stream. Consumers must reject a lost cursor instead of inventing events.
use super::*;

const MAX_CHILD_EVENTS: usize = 4096;
const MAX_CHILD_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHILD_EVENT: usize = 256 * 1024;
const MAX_CHILD_EVENT_BATCH: usize = 512 * 1024;

#[derive(Default)]
pub(super) struct ChildEventLog {
    sequence: u64,
    bytes: usize,
    events: VecDeque<(u64, Value, usize)>,
}

impl ChildEventLog {
    pub(super) fn push(&mut self, mut event: Value) {
        let mut size = serde_json::to_vec(&event)
            .expect("child event is JSON")
            .len();
        if size > MAX_CHILD_EVENT {
            event = json!({"kind": "observation_error", "timestamp": event["timestamp"],
                "error": "child event exceeded 262144 bytes"});
            size = serde_json::to_vec(&event)
                .expect("child event is JSON")
                .len();
        }
        self.sequence += 1;
        self.bytes += size;
        self.events.push_back((self.sequence, event, size));
        while self.events.len() > MAX_CHILD_EVENTS || self.bytes > MAX_CHILD_EVENT_BYTES {
            if let Some((_, _, bytes)) = self.events.pop_front() {
                self.bytes -= bytes;
            }
        }
    }

    fn after(&self, sequence: u64) -> Result<(Vec<Value>, u64, bool), String> {
        if sequence > self.sequence {
            return Err("child event cursor is ahead of this worker incarnation".into());
        }
        if self
            .events
            .front()
            .is_some_and(|(first, _, _)| sequence + 1 < *first)
        {
            return Err("child event cursor expired; lossless observation cannot resume".into());
        }
        let mut events = Vec::new();
        let mut next = sequence;
        let mut bytes = 0;
        for (number, event, size) in self
            .events
            .iter()
            .filter(|(number, _, _)| *number > sequence)
        {
            if events.len() >= 256 || bytes + size > MAX_CHILD_EVENT_BATCH {
                break;
            }
            bytes += size;
            next = *number;
            events.push(json!({"sequence": number, "event": event}));
        }
        Ok((events, next, next < self.sequence))
    }
}

impl DelegationManager {
    pub(super) fn record_child_event(&self, id: &str, mut event: Value) {
        event["timestamp"] = json!(timestamp_ms());
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(record) = state.records.get_mut(id) {
            if record.extension_policy.is_some() {
                record.child_events.push(event);
            }
        }
        drop(state);
        self.changed.notify_waiters();
    }

    pub(super) fn observe_child_event(&self, id: &str, event: &AgentEvent) {
        let event = match event {
            AgentEvent::TurnStarted => json!({"kind": "turn_started"}),
            AgentEvent::OutputDelta { channel, text } => json!({
                "kind": "output_delta", "channel": match channel {
                    crate::events::OutputChannel::Text => "text",
                    crate::events::OutputChannel::Reasoning => "reasoning",
                }, "text": text,
            }),
            AgentEvent::ProviderRetry { .. } | AgentEvent::CandidateRejected { .. } => {
                json!({"kind": "output_discarded"})
            }
            AgentEvent::ToolStarted { id, name, args } => json!({
                "kind": "tool_started", "id": id.0, "name": name, "arguments": args,
            }),
            AgentEvent::ToolFinished { id, result, .. } => match result {
                Ok(output) => {
                    let content = output
                        .content_parts()
                        .iter()
                        .map(|part| match part {
                            crate::tool::ToolOutputContentPart::Text(text) => {
                                Ok(json!({"type": "text", "text": text}))
                            }
                            crate::tool::ToolOutputContentPart::Media(_) => Err(()),
                        })
                        .collect::<Result<Vec<_>, _>>();
                    match content {
                        Ok(content) => json!({"kind": "tool_finished", "id": id.0,
                            "content": content, "metadata": output.metadata(), "is_error": output.is_error()}),
                        Err(()) => {
                            json!({"kind": "observation_error", "error": "child tool media observations are not yet supported"})
                        }
                    }
                }
                Err(error) => json!({"kind": "tool_finished", "id": id.0,
                    "error": error.to_string(), "is_error": true}),
            },
            AgentEvent::TurnFinished {
                message,
                stop_reason,
                turn_usage,
                turn_cost,
                ..
            } => {
                json!({"kind": "turn_finished", "message": message,
                    "stop_reason": format!("{stop_reason:?}"), "usage": turn_usage, "cost": turn_cost})
            }
            AgentEvent::RunFinished { reason, .. } => json!({
                "kind": "run_finished", "reason": match reason {
                    FinishReason::Completed => "completed",
                    FinishReason::Aborted => "interrupted",
                    FinishReason::MaxTurns => "limit_reached",
                    FinishReason::Failed(_) => "failed",
                },
            }),
            AgentEvent::OutputMedia { .. } => json!({"kind": "observation_error",
                "error": "child media observations are not yet supported"}),
            AgentEvent::CompactionStarted { .. } => json!({"kind": "observation_error",
                "error": "child compaction mirror replacement is not yet supported"}),
            AgentEvent::ProviderUsageUncertain => json!({"kind": "observation_error",
                "error": "native child provider usage is uncertain"}),
            _ => return,
        };
        self.record_child_event(id, event);
    }
}

impl ExtensionDelegationService {
    /// Owner-authenticated, lossless cursor reads. No polling task, transcript
    /// path, or extension-selected authority is introduced. Bind this only to
    /// the separately negotiated `agent_session_events_v1` service.
    pub(crate) async fn events(
        &self,
        resource_owner: &str,
        target: &str,
        after_sequence: u64,
        timeout: Duration,
        cancellation: &crate::CancellationToken,
    ) -> Result<Value, String> {
        if timeout > Duration::from_secs(25) {
            return Err("child event wait must not exceed 25000 milliseconds".into());
        }
        let manager = self.manager()?;
        self.owner_identity(&manager, resource_owner)?;
        let target = self.resolve_owned_target(&manager, resource_owner, target)?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = manager.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let snapshot = {
                let state = manager.state.lock().unwrap_or_else(|p| p.into_inner());
                let record = state.records.get(&target).ok_or("child session retired")?;
                let (events, next, more) = record.child_events.after(after_sequence)?;
                json!({"agent_id": target, "session_id": record.resource_owner, "events": events, "next_sequence": next,
                    "has_more": more, "status": record.status})
            };
            if !snapshot["events"]
                .as_array()
                .expect("events array")
                .is_empty()
                || !matches!(
                    snapshot["status"]["state"].as_str(),
                    Some("pending" | "running")
                )
                || tokio::time::Instant::now() >= deadline
            {
                return Ok(snapshot);
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err("child event wait cancelled".into()),
                _ = tokio::time::sleep_until(deadline) => return Ok(snapshot),
                _ = &mut changed => {},
            }
        }
    }

    /// Dispose one owned child tree, rather than aborting an unrelated sibling
    /// or merely ending the current model turn. Settlement stays host-owned.
    pub(crate) fn stop(&self, resource_owner: &str, target: &str) -> Result<Value, String> {
        let manager = self.manager()?;
        self.owner_identity(&manager, resource_owner)?;
        let target = self.resolve_owned_target(&manager, resource_owner, target)?;
        manager.request_shutdown_agent_trees(&BTreeSet::from([target.clone()]));
        Ok(json!({"agent_id": target, "shutdown_requested": true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_are_lossless_bounded_and_non_consuming() {
        let mut log = ChildEventLog::default();
        log.push(json!({"kind": "run_started"}));
        log.push(json!({"kind": "output_delta", "text": "hello"}));
        assert_eq!(log.after(0).unwrap().0.len(), 2);
        assert_eq!(log.after(0).unwrap().1, 2);
        assert_eq!(log.after(1).unwrap().0[0]["sequence"], 2);
        assert!(log.after(3).is_err());
        for _ in 0..MAX_CHILD_EVENTS {
            log.push(json!({"kind": "turn_started"}));
        }
        assert!(log.after(0).unwrap_err().contains("expired"));
        let (events, next, more) = log.after(2).unwrap();
        assert_eq!(events.len(), 256);
        assert_eq!(next, 258);
        assert!(more);
    }

    #[test]
    fn oversized_observation_is_an_explicit_failure_not_a_truncated_event() {
        let mut log = ChildEventLog::default();
        log.push(json!({"kind": "output_delta", "text": "x".repeat(MAX_CHILD_EVENT)}));
        assert_eq!(
            log.after(0).unwrap().0[0]["event"]["kind"],
            "observation_error"
        );
    }
}
