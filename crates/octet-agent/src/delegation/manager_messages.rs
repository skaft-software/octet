//! DelegationManager messages: pending input, follow-ups, waits, interrupts and shutdown.

use super::*;

impl DelegationManager {
    pub(super) fn pending_message_count(&self, id: &str) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.records.get(id).map_or(0, |record| {
            record
                .pending_messages
                .len()
                .saturating_add(record.reserved_messages.messages)
                .saturating_sub(record.inflight_message_ids.len())
        })
    }

    pub(super) fn interrupt_requested(&self, id: &str) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .records
            .get(id)
            .is_some_and(|record| record.interrupt_requested)
    }

    pub(super) async fn acquire_follow_up_permit(
        &self,
        id: &str,
        shutdown: &crate::CancellationToken,
        deadline: Option<tokio::time::Instant>,
    ) -> PermitWait {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if shutdown.is_cancelled() {
                return PermitWait::Shutdown;
            }
            {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let Some(record) = state.records.get(id) else {
                    return PermitWait::Shutdown;
                };
                if state.persistence_error.is_some()
                    || state.shutting_down
                    || record.shutdown.is_cancelled()
                {
                    return PermitWait::Shutdown;
                }
                if record.interrupt_requested {
                    return PermitWait::Interrupted;
                }
            }
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return PermitWait::Shutdown,
                _ = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if deadline.is_some() => return PermitWait::TimedOut,
                _ = &mut notified => {}
                permit = self.current_permits().acquire_owned() => {
                    return match permit {
                        Ok(permit) => PermitWait::Acquired(permit),
                        Err(_) => PermitWait::Shutdown,
                    };
                }
            }
        }
    }

    pub(super) fn drain_interrupted_commands(
        &self,
        target: &str,
        commands: &mut mpsc::Receiver<WorkerCommand>,
        queued_tasks: &mut VecDeque<QueuedTask>,
    ) -> bool {
        let mut messages = Vec::new();
        let mut saw_shutdown = false;
        while let Ok(command) = commands.try_recv() {
            match command.kind {
                WorkerCommandKind::Message(message) => messages.push(message),
                WorkerCommandKind::FollowUp => {
                    *queued_tasks = self.restored_tasks(target);
                    queued_tasks.retain(|task| matches!(task, QueuedTask::FollowUp(_)));
                }
                WorkerCommandKind::Shutdown => saw_shutdown = true,
            }
        }
        for message in messages {
            self.queue_reserved_message(target, message);
        }
        saw_shutdown
    }

    pub(super) fn take_pending_messages(&self, target: &str) -> Vec<DirectedMessage> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .records
            .get_mut(target)
            .map(|record| {
                let messages = record
                    .pending_messages
                    .iter()
                    .filter(|message| !record.inflight_message_ids.contains(&message.delivery_id))
                    .cloned()
                    .collect::<Vec<_>>();
                for message in &messages {
                    record
                        .inflight_message_ids
                        .insert(message.delivery_id.clone());
                    record
                        .reserved_messages
                        .add(directed_message_bytes(message));
                }
                messages
            })
            .unwrap_or_default()
    }

    pub(super) fn release_prompt_message_reservations(
        &self,
        target: &str,
        messages: &[DirectedMessage],
    ) {
        if messages.is_empty() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            for message in messages {
                record.inflight_message_ids.remove(&message.delivery_id);
                record.reserved_messages.remove(QueueUsage {
                    messages: 1,
                    bytes: directed_message_bytes(message),
                });
                record
                    .pending_messages
                    .retain(|pending| pending.delivery_id != message.delivery_id);
            }
            self.persist_durable_fleet_locked(&mut state);
        }
    }

    pub(super) fn restore_pending_messages(&self, target: &str, messages: Vec<DirectedMessage>) {
        if messages.is_empty() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            // The durable FIFO never gave up ownership during prompt delivery.
            // Only the process-local attempt/reservation needs to be released.
            for message in messages {
                record.inflight_message_ids.remove(&message.delivery_id);
                record.reserved_messages.remove(QueueUsage {
                    messages: 1,
                    bytes: directed_message_bytes(&message),
                });
            }
        }
        drop(state);
        self.changed.notify_waiters();
    }

    pub(super) fn queue_reserved_message(&self, target: &str, message: DirectedMessage) {
        let usage = QueueUsage {
            messages: 1,
            bytes: directed_message_bytes(&message),
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            record.inflight_message_ids.remove(&message.delivery_id);
            record.reserved_messages.remove(usage);
            // This command only woke the worker; the accepted payload already
            // occupies its durable position in pending_messages.
        }
        drop(state);
        self.changed.notify_waiters();
    }

    pub(super) fn release_message_reservation(&self, target: &str, message: &DirectedMessage) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            record.inflight_message_ids.remove(&message.delivery_id);
            record.reserved_messages.remove(QueueUsage {
                messages: 1,
                bytes: directed_message_bytes(message),
            });
            record
                .pending_messages
                .retain(|pending| pending.delivery_id != message.delivery_id);
            self.persist_durable_fleet_locked(&mut state);
        }
    }

    /// Drops one follow-up from the durable queue only after the child agent
    /// reports `FollowUpDelivered`, which is emitted after its session append.
    pub(super) fn acknowledge_follow_up_delivery(&self, target: &str, follow_up: &QueuedFollowUp) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            record
                .pending_follow_ups
                .retain(|pending| pending.delivery_id != follow_up.delivery_id);
            self.persist_durable_fleet_locked(&mut state);
        }
    }

    pub(super) fn persist_task_result(
        &self,
        target: &str,
        task: &QueuedTask,
        result: &TaskRestore,
        delivered: bool,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(target) else {
            return;
        };
        let clear = delivered || matches!(result, TaskRestore::DeadLettered { .. });
        match task {
            QueuedTask::Initial(task) => {
                if let Some(pending) = record
                    .pending_initial_task
                    .as_mut()
                    .filter(|pending| pending.delivery_id == task.delivery_id)
                {
                    if clear {
                        record.pending_initial_task = None;
                    } else if let TaskRestore::Restored { attempts } = result {
                        pending.attempts = *attempts;
                    }
                }
            }
            QueuedTask::FollowUp(task) => {
                if clear {
                    record
                        .pending_follow_ups
                        .retain(|pending| pending.delivery_id != task.delivery_id);
                    if !delivered {
                        record.queued_follow_ups.remove(task.usage());
                    }
                } else if let TaskRestore::Restored { attempts } = result {
                    if let Some(pending) = record
                        .pending_follow_ups
                        .iter_mut()
                        .find(|pending| pending.delivery_id == task.delivery_id)
                    {
                        pending.attempts = *attempts;
                    }
                }
            }
        }
        if let TaskRestore::DeadLettered { attempts } = result {
            record.durable_diagnostic = Some(format!(
                "delegated task dead-lettered after {attempts} undelivered attempts"
            ));
        }
        self.persist_durable_fleet_locked(&mut state);
    }

    pub(super) fn release_follow_up_usage(&self, target: &str, usage: QueueUsage) {
        if usage.messages == 0 {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            record.queued_follow_ups.remove(usage);
        }
    }

    pub(super) fn resolve_id_locked(state: &ManagerState, target: &str) -> Option<String> {
        if target == ROOT_AGENT_ID || target == ROOT_AGENT_PATH {
            return Some(ROOT_AGENT_ID.into());
        }
        if state.records.contains_key(target) {
            return Some(target.to_owned());
        }
        state
            .records
            .iter()
            .find_map(|(id, record)| (record.identity.path == target).then(|| id.clone()))
    }

    pub(super) async fn send_message(
        &self,
        owner: &AgentIdentity,
        target: &str,
        message: String,
    ) -> Result<Value, String> {
        validate_durable_text("message", &message)?;
        let candidate = DirectedMessage {
            delivery_id: new_delivery_id()?,
            from: owner.id.clone(),
            message,
        };
        let (target_id, delivery) = {
            // The journal_order guard spans decide → append → commit so that
            // journal record order matches state mutation order, while the
            // state lock is dropped across the journal's durable `sync_data`.
            let _journal_order = self
                .journal_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_fleet_lease()?;
            let (target_id, delivery, encoded, command_permit) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.ensure_owner_active_locked(&state, owner)?;
                let target_id = Self::resolve_id_locked(&state, target)
                    .ok_or_else(|| format!("unknown delegation target: {target}"))?;

                let mut command_permit = None;
                let delivery = if target_id == ROOT_AGENT_ID {
                    let mailbox_message = MailboxMessage {
                        kind: "message",
                        from: candidate.from.clone(),
                        task_name: None,
                        message: candidate.message.clone(),
                        evictable: false,
                        continued: false,
                        leased: false,
                    };
                    if !mailbox_can_accept_after_evicting_automatic(
                        &state.root_mailbox,
                        &mailbox_message,
                    ) {
                        return Err("root delegation mailbox is full".into());
                    }
                    "queued"
                } else {
                    let record = state
                        .records
                        .get(&target_id)
                        .expect("resolved child exists");
                    if matches!(record.status, DelegatedAgentStatus::Shutdown)
                        || record.shutdown.is_cancelled()
                    {
                        return Err(format!("target is shut down: {}", record.identity.path));
                    }
                    if record.interrupt_requested {
                        return Err(format!(
                            "target is being interrupted: {}",
                            record.identity.path
                        ));
                    }
                    if !record_can_accept_pending_message(record, &candidate) {
                        return Err(format!(
                            "target pending-message queue is full: {}",
                            record.identity.path
                        ));
                    }
                    if matches!(record.status, DelegatedAgentStatus::Running) {
                        command_permit = Some(
                            record
                                .command_tx
                                .clone()
                                .try_reserve_owned()
                                .map_err(command_queue_error)?,
                        );
                        "steering"
                    } else {
                        "queued"
                    }
                };

                let encoded = serde_json::to_vec(&ProvenanceEvent::Message {
                    timestamp_ms: timestamp_ms(),
                    from: &owner.id,
                    to: &target_id,
                    kind: "message",
                    message: &candidate.message,
                })
                .map_err(io::Error::other);
                let encoded = match encoded {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let message = format!("could not persist message provenance: {error}");
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                };
                (target_id, delivery, encoded, command_permit)
            };
            if let Err(error) = self.journal.append_encoded(&encoded) {
                let message = format!("could not persist message provenance: {error}");
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(message);
            }
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if target_id == ROOT_AGENT_ID {
                    push_mailbox_bounded(
                        &mut state.root_mailbox,
                        MailboxMessage {
                            kind: "message",
                            from: candidate.from,
                            task_name: None,
                            message: candidate.message,
                            evictable: false,
                            continued: false,
                            leased: false,
                        },
                    );
                } else {
                    let Some(record) = state.records.get_mut(&target_id) else {
                        // An owning-run reset removed the record mid-operation;
                        // do not resurrect it after its provenance record.
                        return Err("delegation team is shutting down".into());
                    };
                    record.pending_messages.push_back(candidate.clone());
                    if let Some(permit) = command_permit {
                        record
                            .inflight_message_ids
                            .insert(candidate.delivery_id.clone());
                        record
                            .reserved_messages
                            .add(directed_message_bytes(&candidate));
                        permit.send(WorkerCommand::message(candidate));
                    }
                }
                self.persist_durable_fleet_locked(&mut state);
                if let Some(error) = state.persistence_error.clone() {
                    return Err(format!("could not persist queued message: {error}"));
                }
            }
            (target_id, delivery)
        };
        self.changed.notify_waiters();
        Ok(json!({"delivered_to": target_id, "delivery": delivery}))
    }

    pub(super) async fn follow_up(
        self: &Arc<Self>,
        owner: &AgentIdentity,
        request: FollowUpRequest,
    ) -> Result<Value, String> {
        validate_durable_text("follow-up", &request.message)?;
        let follow_up = QueuedFollowUp {
            delivery_id: new_delivery_id()?,
            from: owner.id.clone(),
            message: request.message,
            attempts: 0,
        };
        let (target_id, target_path, running_now, resume) = {
            // The journal_order guard spans decide → append → commit so that
            // journal record order matches state mutation order, while the
            // state lock is dropped across the journal's durable syncs.
            let _journal_order = self
                .journal_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_fleet_lease()?;
            let (
                target_id,
                target_path,
                running_now,
                follow_up,
                encoded_message,
                encoded_status,
                command_permit,
                resume,
            ) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.ensure_owner_active_locked(&state, owner)?;
                let target_id = Self::resolve_id_locked(&state, &request.target)
                    .ok_or_else(|| format!("unknown delegation target: {}", request.target))?;
                if target_id == ROOT_AGENT_ID {
                    return Err("followup_task cannot target the root agent".into());
                }
                let record = state
                    .records
                    .get(&target_id)
                    .expect("resolved child exists");
                if matches!(record.status, DelegatedAgentStatus::Shutdown)
                    || record.shutdown.is_cancelled()
                {
                    return Err(format!("target is shut down: {}", record.identity.path));
                }
                if record.interrupt_requested {
                    return Err(format!(
                        "target is being interrupted: {}",
                        record.identity.path
                    ));
                }
                if !record_can_accept_follow_up(record, &follow_up) {
                    return Err(format!(
                        "target follow-up queue is full: {}",
                        record.identity.path
                    ));
                }
                // A suspended worker (a record restored from the durable
                // roster, or parked at the approval boundary / session release)
                // has no live task in this process. An explicit follow-up is the
                // decision and the new task: the worker is started again under
                // a fresh fleet claim, or the refusal names its reason.
                let suspended =
                    !record.live_task && (record.detached || record.detached_commands.is_some());
                let running_now = !suspended && record.status.is_running();
                let target_path = record.identity.path.clone();
                let session_path = record.session_path.clone();
                let extension_policy = record.extension_policy.clone();
                let shutdown = record.shutdown.clone();
                let claim = if suspended {
                    Some(self.fresh_claim_for(record)?)
                } else {
                    None
                };
                let (permit, mut session, mut resumed_commands) = if suspended {
                    let permit = self.current_permits().try_acquire_owned().map_err(|_| {
                        "delegation concurrency limit reached; no free execution slot is available to resume this worker"
                            .to_owned()
                    })?;
                    let session = self.reopen_child_session(&session_path).map_err(|error| {
                        format!(
                            "worker could not be resumed: its child session could not be reopened: {error}"
                        )
                    })?;
                    Self::discard_inputs_delivered_to_session(
                        state
                            .records
                            .get_mut(&target_id)
                            .expect("resolved child exists"),
                        &session,
                    )
                    .map_err(|error| format!("worker could not be resumed: {error}"))?;
                    let commands = match state
                        .records
                        .get_mut(&target_id)
                        .expect("resolved child exists")
                        .detached_commands
                        .take()
                    {
                        Some(commands) => commands,
                        None => {
                            let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
                            state
                                .records
                                .get_mut(&target_id)
                                .expect("resolved child exists")
                                .command_tx = command_tx;
                            command_rx
                        }
                    };
                    (Some(permit), Some(session), Some(commands))
                } else {
                    (None, None, None)
                };
                let command_permit = state.records[&target_id]
                    .command_tx
                    .clone()
                    .try_reserve_owned()
                    .map_err(command_queue_error)?;

                let encoded_message = serde_json::to_vec(&ProvenanceEvent::Message {
                    timestamp_ms: timestamp_ms(),
                    from: &owner.id,
                    to: &target_id,
                    kind: "follow_up",
                    message: &follow_up.message,
                })
                .map_err(io::Error::other);
                let encoded_message = match encoded_message {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let message = format!("could not persist follow-up provenance: {error}");
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                };
                let encoded_status = (!running_now)
                    .then(|| {
                        serde_json::to_vec(&ProvenanceEvent::AgentStatus {
                            timestamp_ms: timestamp_ms(),
                            agent_id: &target_id,
                            status: &DelegatedAgentStatus::Pending,
                        })
                        .map_err(io::Error::other)
                    })
                    .transpose();
                let encoded_status = match encoded_status {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let message = format!(
                            "could not persist pending follow-up status provenance: {error}"
                        );
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                };
                let resume = match (permit, session.take(), resumed_commands.take(), claim) {
                    (Some(initial_permit), Some(session), Some(commands), Some(claim)) => Some((
                        WorkerStartup {
                            generation: state.records[&target_id].worker_generation + 1,
                            identity: state.records[&target_id].identity.clone(),
                            session,
                            commands,
                            shutdown,
                            initial_permit,
                            extension_policy,
                            deadline: None,
                            deadline_ms: None,
                        },
                        claim,
                    )),
                    _ => None,
                };
                (
                    target_id,
                    target_path,
                    running_now,
                    follow_up,
                    encoded_message,
                    encoded_status,
                    command_permit,
                    resume,
                )
            };
            if let Err(error) = self.journal.append_encoded(&encoded_message) {
                let message = format!("could not persist follow-up provenance: {error}");
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(message);
            }
            if let Some(encoded) = &encoded_status {
                if let Err(error) = self.journal.append_encoded(encoded) {
                    let message =
                        format!("could not persist pending follow-up status provenance: {error}");
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    self.fail_persistence_locked(&mut state, &error);
                    return Err(message);
                }
            }
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !running_now {
                    if let Some(record) = state.records.get_mut(&target_id) {
                        record.status = DelegatedAgentStatus::Pending;
                        // A resumed worker is no longer terminal; drop the stale
                        // completion timestamp so elapsed-time consumers do not see
                        // a frozen first-run interval.
                        record.completed_at_ms = None;
                        // A follow-up to a settled worker starts a new run. If the
                        // original wall budget already elapsed while the worker was
                        // idle, the spawn-frozen deadline would start an instantly
                        // expired run; re-anchor it from the requested timeout so
                        // the resumed run owns a fresh budget.
                        if let (Some(deadline), Some(timeout)) = (
                            record.deadline_at_ms,
                            record
                                .extension_policy
                                .as_ref()
                                .and_then(|policy| policy.timeout_ms),
                        ) {
                            let now = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
                            if deadline <= now {
                                record.deadline_at_ms = Some(now.saturating_add(timeout));
                            }
                        }
                        if let Some((startup, claim)) = &resume {
                            // The explicit decision clears the park: the worker
                            // may run again under this session owner's claim.
                            record.claim = Some(claim.clone());
                            record.worker_generation = startup.generation;
                            record.detached = false;
                            record.durable_diagnostic = None;
                            record.live_task = true;
                        }
                    }
                }
                if let Some(record) = state.records.get_mut(&target_id) {
                    let usage = follow_up.usage();
                    record.queued_follow_ups.add_usage(usage);
                    record.pending_follow_ups.push_back(follow_up.clone());
                }
                // The journal proves provenance; the fleet roster proves the
                // queued payload. Do not acknowledge acceptance until both are
                // durable, otherwise a restart could silently lose this input.
                self.persist_durable_fleet_locked(&mut state);
                if let Some(error) = state.persistence_error.clone() {
                    return Err(format!("could not persist queued follow-up: {error}"));
                }
                command_permit.send(WorkerCommand::follow_up());
            }
            (target_id, target_path, running_now, resume)
        };
        if let Some((mut startup, _)) = resume {
            // A resumed worker keeps its host-owned wall budget: adopt the
            // durable deadline the follow-up just re-anchored so the resumed run
            // cannot outlive it or start already expired.
            let host_deadline = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .records
                .get(&startup.identity.id)
                .and_then(|record| record.deadline_at_ms);
            startup.deadline_ms = host_deadline;
            startup.deadline = host_deadline.map(wall_deadline_instant);
            self.spawn_worker(startup);
        }
        self.changed.notify_waiters();
        Ok(json!({
            "agent_id": target_id,
            "agent_path": target_path,
            "delivery": if running_now {"follow_up"} else {"new_run"}
        }))
    }

    pub(super) async fn wait(
        self: &Arc<Self>,
        owner: &AgentIdentity,
        timeout: Duration,
        cancellation: &crate::CancellationToken,
        output_limit: usize,
    ) -> Result<WaitOutput, String> {
        if let Some(result) = self.take_wait_result(owner, output_limit)? {
            return Ok(result);
        }
        let _waiter = self.register_waiter(owner)?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.take_wait_result(owner, output_limit)? {
                return Ok(result);
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err("wait_agent cancelled".into()),
                _ = tokio::time::sleep_until(deadline) => {
                    let agents = self.list_value_for(owner)?;
                    return Ok(WaitOutput {
                        value: json!({"timed_out": true, "messages": [], "agents": agents}),
                        delivery_id: None,
                    });
                }
                _ = &mut notified => {}
            }
        }
    }

    pub(super) fn register_waiter(&self, owner: &AgentIdentity) -> Result<WaiterGuard<'_>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_owner_active_locked(&state, owner)?;
        if state.active_waiters >= self.config.limits.max_total_agents {
            return Err(format!(
                "delegation waiter limit reached ({})",
                self.config.limits.max_total_agents
            ));
        }
        state.active_waiters += 1;
        Ok(WaiterGuard { manager: self })
    }

    pub(super) fn take_wait_result(
        &self,
        owner: &AgentIdentity,
        output_limit: usize,
    ) -> Result<Option<WaitOutput>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_owner_active_locked(&state, owner)?;
        let delivery_id = state.next_mailbox_delivery;
        let leased = if owner.id == ROOT_AGENT_ID {
            if state.root_mailbox.is_empty() {
                None
            } else {
                self.verify_held_fleet_lease()?;
                if state.root_mailbox_delivery.is_some() {
                    return Err(
                        "a root mailbox delivery is already awaiting durable acknowledgement"
                            .into(),
                    );
                }
                let (value, plan) =
                    lease_mailbox_page(&mut state.root_mailbox, delivery_id, output_limit)?;
                state.root_mailbox_delivery = Some(plan);
                Some(value)
            }
        } else {
            let record = state
                .records
                .get_mut(&owner.id)
                .expect("validated owner exists");
            if record.mailbox.is_empty() {
                None
            } else {
                self.verify_held_fleet_lease()?;
                if record.mailbox_delivery.is_some() {
                    return Err(
                        "an agent mailbox delivery is already awaiting durable acknowledgement"
                            .into(),
                    );
                }
                let (value, plan) =
                    lease_mailbox_page(&mut record.mailbox, delivery_id, output_limit)?;
                record.mailbox_delivery = Some(plan);
                Some(value)
            }
        };
        if let Some(value) = leased {
            state.next_mailbox_delivery = state.next_mailbox_delivery.saturating_add(1);
            self.persist_durable_fleet_locked(&mut state);
            if let Some(error) = state.persistence_error.clone() {
                return Err(format!("could not persist mailbox delivery: {error}"));
            }
            return Ok(Some(WaitOutput {
                value,
                delivery_id: Some(delivery_id),
            }));
        }

        let descendants_running = state.records.values().any(|record| {
            is_descendant_path(&record.identity.path, &owner.path) && record.status.is_running()
        });
        if !descendants_running {
            Ok(Some(WaitOutput {
                value: json!({"timed_out": false, "messages": [], "agents": list_value_locked(&state)}),
                delivery_id: None,
            }))
        } else {
            Ok(None)
        }
    }

    pub(super) fn resolve_mailbox_delivery(
        &self,
        owner_id: &str,
        delivery_id: u64,
        delivered: bool,
    ) {
        if self.verify_held_fleet_lease().is_err() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let resolved = if owner_id == ROOT_AGENT_ID {
            match state.root_mailbox_delivery {
                Some(plan) if plan.id == delivery_id => {
                    state.root_mailbox_delivery = None;
                    resolve_mailbox_page(&mut state.root_mailbox, plan, delivered);
                    true
                }
                _ => false,
            }
        } else if let Some(record) = state.records.get_mut(owner_id) {
            match record.mailbox_delivery {
                Some(plan) if plan.id == delivery_id => {
                    record.mailbox_delivery = None;
                    resolve_mailbox_page(&mut record.mailbox, plan, delivered);
                    true
                }
                _ => false,
            }
        } else {
            false
        };
        if resolved {
            self.persist_durable_fleet_locked(&mut state);
        }
        drop(state);
        if resolved {
            self.changed.notify_waiters();
        }
    }

    pub(super) fn list_value_for(&self, owner: &AgentIdentity) -> Result<Value, String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_owner_active_locked(&state, owner)?;
        Ok(list_value_locked(&state))
    }

    pub(super) async fn interrupt(
        &self,
        owner: &AgentIdentity,
        target: &str,
    ) -> Result<Value, String> {
        let (target_id, path, status, requested) = {
            // The journal_order guard spans decide → append → commit so that
            // journal record order matches state mutation order, while the
            // state lock is dropped across the journal's durable `sync_data`.
            let _journal_order = self
                .journal_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_fleet_lease()?;
            let (target_id, path, status, requested, encoded) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.ensure_owner_active_locked(&state, owner)?;
                let target_id = Self::resolve_id_locked(&state, target)
                    .ok_or_else(|| format!("unknown delegation target: {target}"))?;
                if target_id == ROOT_AGENT_ID {
                    return Err("interrupt_agent cannot target the root agent".into());
                }
                let record = state
                    .records
                    .get(&target_id)
                    .expect("resolved child exists");
                if !is_descendant_path(&record.identity.path, &owner.path)
                    && owner.id != ROOT_AGENT_ID
                {
                    return Err("an agent may only interrupt its descendants".into());
                }
                let path = record.identity.path.clone();
                let status = record.status.clone();
                let requested = (status.is_running() || status == DelegatedAgentStatus::Idle)
                    && !record.interrupt_requested;
                let encoded = requested
                    .then(|| {
                        serde_json::to_vec(&ProvenanceEvent::InterruptRequested {
                            timestamp_ms: timestamp_ms(),
                            from: &owner.id,
                            to: &target_id,
                        })
                        .map_err(io::Error::other)
                    })
                    .transpose();
                let encoded = match encoded {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let message = format!("could not persist interrupt provenance: {error}");
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                };
                (target_id, path, status, requested, encoded)
            };
            if requested {
                if let Some(encoded) = encoded {
                    if let Err(error) = self.journal.append_encoded(&encoded) {
                        let message = format!("could not persist interrupt provenance: {error}");
                        let mut state = self
                            .state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                }
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(record) = state.records.get_mut(&target_id) {
                    record.interrupt_requested = true;
                }
            }
            (target_id, path, status, requested)
        };
        if requested {
            self.request_shutdown_descendants(&target_id);
            self.changed.notify_waiters();
        }
        Ok(
            json!({"agent_id": target_id, "agent_path": path, "previous_status": status.label(), "interrupt_requested": requested}),
        )
    }

    /// Cumulative accounting snapshots for extension-owned descendants.
    ///
    /// Reading never consumes a watermark: only the owning session's synced
    /// usage ledger establishes which increments have actually been mirrored.
    /// Include uncertainty-only snapshots even when no turn completed.
    pub(super) fn extension_usage_records(&self, owner_id: &str) -> Vec<DelegatedUsageRecord> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owner_path = if owner_id == ROOT_AGENT_ID {
            Some(ROOT_AGENT_PATH.to_owned())
        } else {
            state
                .records
                .get(owner_id)
                .map(|record| record.identity.path.clone())
        };
        let Some(owner_path) = owner_path else {
            return Vec::new();
        };
        state
            .records
            .values()
            .filter(|record| {
                record.extension_principal.is_some()
                    && is_descendant_path(&record.identity.path, &owner_path)
            })
            .map(|record| DelegatedUsageRecord {
                agent_id: record.identity.id.clone(),
                usage: record.usage,
                usage_uncertain: record.usage_uncertain,
                usage_exposure: record.usage_exposure,
                cost: record.cost,
                turn_count: record.turn_count,
                tool_call_count: record.tool_call_count,
            })
            .collect()
    }

    /// Durable idempotency: the spawn result for an extension principal's
    /// idempotency key, reconstructed from the session-owned record that
    /// survived the parent turn or a process restart.
    pub(super) fn extension_owned_record(
        &self,
        principal: &str,
        resource_owner: &str,
        idempotency_key: &str,
    ) -> Option<ExtensionDurableSpawn> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = state.records.values().find(|record| {
            record.extension_principal.as_deref() == Some(principal)
                && record.extension_idempotency_key.as_deref() == Some(idempotency_key)
                // Legacy records without an owner are returned only to refuse
                // an unverifiable retry, never to create a duplicate worker.
                && record.extension_resource_owner.as_deref().is_none_or(|owner| owner == resource_owner)
        })?;
        Some(ExtensionDurableSpawn {
            task_name: record
                .display_task_name
                .clone()
                .unwrap_or_else(|| record.task_name.clone()),
            profile: record.extension_profile.clone(),
            fingerprint: record.extension_fingerprint.clone(),
            policy: record.extension_requested_policy.clone(),
            resource_owner: record.extension_resource_owner.clone(),
            message_sha256: record.extension_message_sha256.clone(),
            result: extension_spawn_result_value(record),
        })
    }

    pub(super) fn request_shutdown_descendants(&self, owner_id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owner_path = if owner_id == ROOT_AGENT_ID {
            state.root_active = false;
            // The owning session is gone. Live workers must stop, but their
            // durable child sessions are retained: each worker parks as a
            // recoverable `detached` record instead of retiring, so the next
            // session owner (a rebuilt agent, a restarted process) can reattach
            // and continue it.
            state.session_owner_released = true;
            ROOT_AGENT_PATH.to_owned()
        } else if let Some(record) = state.records.get(owner_id) {
            record.identity.path.clone()
        } else {
            return;
        };
        for record in state
            .records
            .values()
            .filter(|record| is_descendant_path(&record.identity.path, &owner_path))
        {
            record.shutdown.cancel();
            let _ = record.command_tx.try_send(WorkerCommand::shutdown());
        }
        drop(state);
        self.changed.notify_waiters();
    }

    pub(super) fn request_shutdown_agent_trees(&self, roots: &BTreeSet<String>) {
        if roots.is_empty() {
            return;
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root_paths = roots
            .iter()
            .filter_map(|id| {
                state
                    .records
                    .get(id)
                    .map(|record| record.identity.path.clone())
            })
            .collect::<Vec<_>>();
        for record in state.records.values().filter(|record| {
            roots.contains(&record.identity.id)
                || root_paths
                    .iter()
                    .any(|root| is_descendant_path(&record.identity.path, root))
        }) {
            record.shutdown.cancel();
            let _ = record.command_tx.try_send(WorkerCommand::shutdown());
        }
        drop(state);
        self.changed.notify_waiters();
    }
}
