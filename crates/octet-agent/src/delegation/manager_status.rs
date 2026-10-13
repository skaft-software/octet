//! DelegationManager worker status, usage accounting and failure handling.

use super::*;

impl DelegationManager {
    pub(super) fn update_agent_tool_started(
        &self,
        id: &str,
        call_id: &str,
        name: String,
        args_summary: String,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.tool_call_count = record.tool_call_count.saturating_add(1);
        record.active_tools.insert(call_id.to_owned(), name.clone());
        record.recent_tools.push_back(ChildToolActivity {
            name,
            args_summary,
            started_at_ms: timestamp_ms() as u64,
            finished_at_ms: None,
            error: false,
            call_id: call_id.to_owned(),
        });
        while record.recent_tools.len() > MAX_CHILD_TOOL_ACTIVITY {
            record.recent_tools.pop_front();
        }
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    pub(super) fn update_agent_tool_finished(&self, id: &str, call_id: &str, is_error: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.active_tools.remove(call_id);
        if let Some(entry) = record
            .recent_tools
            .iter_mut()
            .rev()
            .find(|entry| entry.call_id == call_id && entry.finished_at_ms.is_none())
        {
            entry.finished_at_ms = Some(timestamp_ms() as u64);
            entry.error = is_error;
        }
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    pub(super) fn mark_agent_usage_uncertain(&self, id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.usage_uncertain = true;
        record.usage_exposure = None;
        record.cost_microdollars = None;
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    pub(super) fn update_agent_streamed_output(
        &self,
        id: &str,
        bytes: usize,
        last_update: &mut Option<Instant>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        let previous = record.streamed_output_bytes.div_ceil(4);
        record.streamed_output_bytes = record.streamed_output_bytes.saturating_add(bytes as u64);
        let changed = record.streamed_output_bytes.div_ceil(4) != previous;
        drop(state);
        let now = Instant::now();
        if changed
            && last_update
                .is_none_or(|last| now.duration_since(last) >= STREAMED_OUTPUT_UPDATE_INTERVAL)
        {
            *last_update = Some(now);
            self.publish_telemetry(None, None);
        }
    }

    pub(super) fn clear_agent_streamed_output(&self, id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        let changed = record.streamed_output_bytes != 0;
        record.streamed_output_bytes = 0;
        drop(state);
        if changed {
            self.publish_telemetry(None, None);
        }
    }

    pub(super) fn update_agent_usage(
        &self,
        id: &str,
        usage: Usage,
        cost_microdollars: Option<u64>,
        completed_turn: bool,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        if completed_turn {
            record.turn_count = record.turn_count.saturating_add(1);
        }
        record.usage = usage;
        record.streamed_output_bytes = 0;
        if !record.usage_uncertain && cost_microdollars.is_some() {
            record.cost_microdollars = cost_microdollars;
        }
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    pub(super) fn update_agent_session_accounting(
        &self,
        id: &str,
        session: &Session,
        priced: bool,
    ) {
        let mut usage = Usage::default();
        let mut aggregate_cost = priced.then_some(Cost::default());
        for record in session.usage_records() {
            add_delegated_usage(&mut usage, &record.usage);
            match (aggregate_cost.as_mut(), record.cost) {
                (Some(total), Some(cost)) => add_delegated_cost(total, cost),
                (Some(_), None) => aggregate_cost = None,
                (None, _) => {}
            }
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.usage = usage;
        record.streamed_output_bytes = 0;
        record.cost = aggregate_cost;
        record.usage_uncertain = session.has_uncertain_usage();
        record.usage_exposure = record
            .usage_uncertain
            .then(|| session.usage_uncertainty_exposure())
            .flatten();
        record.cost_microdollars = if record.usage_uncertain {
            None
        } else {
            aggregate_cost.map(|cost| cost.total)
        };
        record.active_tools.clear();
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    /// Marks a worker task dead the moment `run_worker` returns, whatever the
    /// exit path was. A session-owned record without a live task is what a
    /// launchable interactive handle requires.
    pub(super) fn mark_worker_stopped(&self, id: &str, generation: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(id) {
            if record.worker_generation == generation {
                record.live_task = false;
            }
        }
        drop(state);
        self.changed.notify_waiters();
    }

    /// The join supervisor alone calls this after an abnormal task exit. A
    /// normal worker has already committed a terminal status, so this cannot
    /// create a duplicate terminal notification.
    pub(super) fn mark_worker_aborted(
        &self,
        id: &str,
        claim: Option<&DurableFleetClaim>,
        generation: u64,
        cause: &str,
    ) {
        // Claim, running-state check, and terminal mutation share this one
        // lock. An old supervisor therefore cannot settle a newer reattachment.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.persistence_error.is_some() {
            return;
        }
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        if record.claim.as_ref() != claim
            || record.worker_generation != generation
            || !record.status.is_running()
        {
            return;
        }
        record.live_task = false;
        // The crashed receiver is gone; an explicit follow-up must reopen this
        // session rather than publishing into the dead process-local channel.
        // Its delivery attempts are gone too, but their durable payloads remain
        // available for the replacement worker after session reconciliation.
        record.inflight_message_ids.clear();
        record.reserved_messages = QueueUsage::default();
        record.detached = true;
        record.status = DelegatedAgentStatus::Failed {
            error: bounded_text(&format!("delegated {cause}; worker settled by supervisor")),
        };
        record.completed_at_ms = Some(u64::try_from(timestamp_ms()).unwrap_or(u64::MAX));
        let parent_id = record.parent_id.clone();
        let message = MailboxMessage {
            kind: "task_status",
            from: id.to_owned(),
            task_name: Some(record.task_name.clone()),
            message: status_message(&record.identity.path, &record.status),
            evictable: true,
            continued: false,
            leased: false,
        };
        push_mailbox_locked(&mut state, &parent_id, message);
        self.persist_durable_fleet_locked(&mut state);
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    pub(super) fn worker_is_detached(&self, id: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get(id)
            .is_some_and(|record| record.detached)
    }

    /// Parks a live worker whose owning session disappeared.
    ///
    /// The worker stops, but its durable child session and accounting are kept.
    /// Unsettled work becomes [`DelegatedAgentStatus::Detached`]; settled
    /// outcomes retain their terminal evidence independently of attachment.
    /// Returns `false` when the owning session is still attached, so the caller
    /// keeps its normal terminal settlement.
    pub(super) fn park_released_worker(
        &self,
        id: &str,
        commands: Option<mpsc::Receiver<WorkerCommand>>,
    ) -> bool {
        if !self.session_owner_released() {
            return false;
        }
        let diagnostic = bounded_text(
            "the owning session was released while this worker was live; its durable session, task, and accounting are retained for reattachment by the next session owner",
        );
        // Same decide → append → commit ordering as every other durable status
        // transition, so the parked state and its lifecycle notification agree.
        let _journal_order = self
            .journal_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let status = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(record) = state.records.get(id) else {
                return false;
            };
            if record.status.is_running() || matches!(record.status, DelegatedAgentStatus::Idle) {
                DelegatedAgentStatus::Detached
            } else {
                // Attachment is independent of the settled task outcome.
                // Retain its output/error and completion time after teardown.
                record.status.clone()
            }
        };
        let encoded = serde_json::to_vec(&ProvenanceEvent::AgentStatus {
            timestamp_ms: timestamp_ms(),
            agent_id: id,
            status: &status,
        });
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(error) => {
                // `fail_persistence_locked` reports an io failure (the journal's
                // error type). A detached-status event that cannot be encoded is
                // the same class of problem for the caller: persistence is
                // broken, so surface it through the one path rather than a second
                // reporting channel.
                let error = io::Error::new(io::ErrorKind::InvalidData, error);
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return true;
            }
        };
        if let Err(error) = self.journal.append_encoded(&encoded) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.fail_persistence_locked(&mut state, &error);
            return true;
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(record) = state.records.get_mut(id) {
                record.status = status;
                record.detached = true;
                record.live_task = false;
                record
                    .completed_at_ms
                    .get_or_insert_with(|| u64::try_from(timestamp_ms()).unwrap_or(u64::MAX));
                record.durable_diagnostic.get_or_insert(diagnostic);
                if let Some(commands) = commands {
                    record.detached_commands = Some(commands);
                }
            }
            self.persist_durable_fleet_locked(&mut state);
        }
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
        true
    }

    pub(super) fn set_status(
        &self,
        id: &str,
        status: DelegatedAgentStatus,
        notify_parent: bool,
    ) -> bool {
        // The journal_order guard makes this operation's decide → append →
        // commit window mutually exclusive with every other provenance-
        // journaled operation, so journal record order always matches state
        // mutation order. The state lock is dropped across the journal's
        // durable `sync_data` so unrelated state operations are not stalled
        // by disk latency.
        let _journal_order = self
            .journal_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.persistence_error.is_some() || !state.records.contains_key(id) {
                return false;
            }
            let record = state.records.get(id).expect("checked child exists");
            if matches!(&status, DelegatedAgentStatus::Shutdown) && !record.status.is_running() {
                return true;
            }
            if record.interrupt_requested
                && matches!(
                    status,
                    DelegatedAgentStatus::Pending | DelegatedAgentStatus::Running
                )
            {
                return true;
            }
            if record.status == status {
                if matches!(status, DelegatedAgentStatus::Interrupted) {
                    state
                        .records
                        .get_mut(id)
                        .expect("checked child exists")
                        .interrupt_requested = false;
                }
                return true;
            }
        }
        let transition_ms = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
        let encoded = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Re-check under the journal_order guard: a concurrent owning-run
            // reset or persistence failure may have invalidated this record
            // after the fast-path pass above.
            if state.persistence_error.is_some() || !state.records.contains_key(id) {
                return false;
            }
            serde_json::to_vec(&ProvenanceEvent::AgentStatus {
                timestamp_ms: u128::from(transition_ms),
                agent_id: id,
                status: &status,
            })
            .map_err(io::Error::other)
        };
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(error) => {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return false;
            }
        };
        if let Err(error) = self.journal.append_encoded(&encoded) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.fail_persistence_locked(&mut state, &error);
            return false;
        }
        let notification = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(record) = state.records.get_mut(id) else {
                // An owning-run reset removed the record mid-operation; it
                // already owns its journal record and must not be resurrected.
                return true;
            };
            record.status = status;
            if matches!(
                record.status,
                DelegatedAgentStatus::Interrupted
                    | DelegatedAgentStatus::Shutdown
                    | DelegatedAgentStatus::TimedOut
            ) {
                record.pending_initial_task = None;
            }
            if matches!(record.status, DelegatedAgentStatus::Running)
                && record.started_at_ms.is_none()
            {
                record.started_at_ms = Some(transition_ms);
            }
            if !record.status.is_running() && record.completed_at_ms.is_none() {
                record.completed_at_ms = Some(transition_ms);
            }
            if matches!(record.status, DelegatedAgentStatus::Interrupted) {
                record.interrupt_requested = false;
            }
            if matches!(record.status, DelegatedAgentStatus::Shutdown) {
                record.pending_messages.clear();
                record.inflight_message_ids.clear();
                record.reserved_messages = QueueUsage::default();
                record.queued_follow_ups = QueueUsage::default();
            }
            (notify_parent && !record.status.is_running()).then(|| {
                (
                    record.parent_id.clone(),
                    MailboxMessage {
                        kind: "task_status",
                        from: id.to_owned(),
                        task_name: Some(record.task_name.clone()),
                        message: status_message(&record.identity.path, &record.status),
                        evictable: true,
                        continued: false,
                        leased: false,
                    },
                )
            })
        };
        if let Some((parent_id, message)) = notification {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            push_mailbox_locked(&mut state, &parent_id, message);
        }
        {
            // Refresh the durable session-owned roster after every durable
            // status transition so the fleet survives the owning run and a
            // process restart.
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.persist_durable_fleet_locked(&mut state);
        }
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
        true
    }

    pub(super) fn fail_worker_start(&self, id: &str, error: String) {
        self.set_status(id, DelegatedAgentStatus::Failed { error }, true);
    }

    pub(super) fn set_pending_if_needed(&self, id: &str) -> bool {
        self.set_status(id, DelegatedAgentStatus::Pending, false)
    }

    pub(super) fn ensure_owner_active_locked(
        &self,
        state: &ManagerState,
        owner: &AgentIdentity,
    ) -> Result<(), String> {
        if let Some(error) = &state.persistence_error {
            return Err(format!("delegation persistence is unavailable: {error}"));
        }
        if state.shutting_down {
            return Err("delegation team is shutting down".into());
        }
        if owner.id == ROOT_AGENT_ID {
            return (state.root_active && owner.path == ROOT_AGENT_PATH && owner.depth == 0)
                .then_some(())
                .ok_or_else(|| "root delegation owner is not active".to_owned());
        }
        let record = state
            .records
            .get(&owner.id)
            .ok_or_else(|| "delegation owner is no longer available".to_owned())?;
        if record.identity.path != owner.path || record.identity.depth != owner.depth {
            return Err("delegation owner identity does not match team state".into());
        }
        if !matches!(record.status, DelegatedAgentStatus::Running)
            || record.interrupt_requested
            || record.shutdown.is_cancelled()
        {
            return Err(format!(
                "delegation owner is not running: {}",
                record.identity.path
            ));
        }
        Ok(())
    }

    pub(super) fn fail_persistence_locked(&self, state: &mut ManagerState, error: &io::Error) {
        if state.persistence_error.is_some() {
            return;
        }
        let diagnostic = bounded_text(&format!(
            "delegation provenance persistence failed: {error}"
        ));
        state.persistence_error = Some(diagnostic.clone());
        state.shutting_down = true;
        state.root_active = false;
        let mut notifications = Vec::new();
        for record in state.records.values_mut() {
            record.shutdown.cancel();
            let _ = record.command_tx.try_send(WorkerCommand::shutdown());
            record.pending_messages.clear();
            record.inflight_message_ids.clear();
            record.reserved_messages = QueueUsage::default();
            record.queued_follow_ups = QueueUsage::default();
            if record.status.is_running() {
                record.status = DelegatedAgentStatus::Failed {
                    error: diagnostic.clone(),
                };
                notifications.push((
                    record.parent_id.clone(),
                    MailboxMessage {
                        kind: "task_status",
                        from: record.identity.id.clone(),
                        task_name: Some(record.task_name.clone()),
                        message: status_message(&record.identity.path, &record.status),
                        evictable: true,
                        continued: false,
                        leased: false,
                    },
                ));
            }
        }
        for (parent_id, message) in notifications {
            push_mailbox_locked(state, &parent_id, message);
        }
        self.changed.notify_waiters();
    }

    pub(super) fn restored_tasks(&self, id: &str) -> VecDeque<QueuedTask> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get(id)
            .map(|record| {
                record
                    .pending_initial_task
                    .iter()
                    .cloned()
                    .map(QueuedTask::Initial)
                    .chain(
                        record
                            .pending_follow_ups
                            .iter()
                            .cloned()
                            .map(QueuedTask::follow_up),
                    )
                    .collect()
            })
            .unwrap_or_default()
    }
}
