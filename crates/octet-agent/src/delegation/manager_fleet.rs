//! DelegationManager durable fleet: team files, the lease, persistence, restore and reattach.

use super::*;

impl DelegationManager {
    pub(super) fn create_team_file(&self, path: &Path) -> Result<File, SecureFileError> {
        match &self.team_storage {
            Some(directory) => directory.create_regular_file_for_append(path),
            None => secure_fs::create_regular_file_for_append(path),
        }
    }

    pub(super) fn open_team_file_for_append(&self, path: &Path) -> Result<File, SecureFileError> {
        match &self.team_storage {
            Some(directory) => directory.open_regular_file_for_append(path),
            None => secure_fs::open_regular_file_for_append(path),
        }
    }

    pub(super) fn remove_team_file_if_exists(&self, path: &Path) -> Result<bool, SecureFileError> {
        match &self.team_storage {
            Some(directory) => directory.remove_regular_file_if_exists(path),
            None => secure_fs::remove_regular_file_if_exists(path),
        }
    }

    pub(super) fn reopen_child_session(&self, path: &Path) -> Result<Session, DelegationError> {
        let file = match &self.team_storage {
            Some(directory) if path.parent() == Some(directory.path()) => {
                self.open_team_file_for_append(path)?
            }
            // A record restored from an earlier process points at its original
            // team directory, which is retained on disk. Open it by absolute
            // path instead of through the current team capability.
            _ => secure_fs::open_regular_file_for_append(path)?,
        };
        Ok(Session::open_with_file(path, file)?)
    }

    /// Whether this manager holds the session's durable fleet lease.
    pub(super) fn lease_held(&self) -> bool {
        self.lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// Verify an existing claim without acquiring ownership while state is locked.
    pub(super) fn verify_held_fleet_lease(&self) -> Result<(), String> {
        self.lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .ok_or_else(|| {
                self.lease_refusal_reason()
                    .unwrap_or_else(|| "this session does not hold the durable fleet lease".into())
            })?
            .is_current()
    }

    /// Claim this manager holds, when it holds one.
    pub(super) fn current_claim(&self) -> Option<DurableFleetClaim> {
        self.lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(FleetLease::claim)
    }

    /// Bounded refusal reason when the lease is not held.
    pub(super) fn lease_refusal_reason(&self) -> Option<String> {
        self.lease_refusal
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Acquire the lease once, at the first boundary where this session owns a
    /// run. Retrying here is what lets a rebuilt or restarted session take the
    /// fleet over from a previous owner that has since released it.
    pub(super) fn ensure_fleet_lease(&self) -> Result<(), String> {
        if let Some(lease) = self
            .lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            return lease.is_current();
        }
        let Some(roster_path) = self.roster_path.clone() else {
            return Err("this session has no durable fleet roster or lease".into());
        };
        let session_directory = roster_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.team_directory.clone());
        let mut lease = self
            .lease
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(held) = lease.as_ref() {
            return held.is_current();
        }
        match FleetLease::try_acquire(&session_directory, &self.root_session) {
            Ok(acquired) => {
                // The observer's construction-time snapshot can be arbitrarily
                // old. Validate and load the owner's latest roster while the
                // newly acquired lock excludes every other writer.
                let fleet = match self.read_durable_fleet() {
                    Ok(Some(fleet)) => fleet,
                    Ok(None) => DurableFleet {
                        version: FLEET_ROSTER_VERSION,
                        root_session: self.root_session.clone(),
                        records: Vec::new(),
                        next_mailbox_delivery: 1,
                        root_mailbox: VecDeque::new(),
                        root_mailbox_delivery: None,
                    },
                    Err(reason) => {
                        drop(acquired);
                        drop(lease);
                        *self
                            .lease_refusal
                            .write()
                            .unwrap_or_else(|p| p.into_inner()) = Some(reason.clone());
                        return Err(reason);
                    }
                };
                *lease = Some(acquired);
                drop(lease);
                *self
                    .lease_refusal
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                self.restore_durable_fleet_from(fleet, true);
                if let Some(error) = &self
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .persistence_error
                {
                    return Err(format!("could not persist reclaimed fleet: {error}"));
                }
                Ok(())
            }
            Err(reason) => {
                drop(lease);
                *self
                    .lease_refusal
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason.clone());
                Err(reason)
            }
        }
    }

    /// Fresh claim for starting a restored or newly spawned worker.
    ///
    /// Fails closed whenever the lease is absent, stale, or already superseded
    /// by a newer generation than the record was written under.
    pub(super) fn fresh_claim_for(
        &self,
        record: &AgentRecord,
    ) -> Result<DurableFleetClaim, String> {
        let lease = self
            .lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(lease) = lease.as_ref() else {
            return Err(self
                .lease_refusal_reason()
                .unwrap_or_else(|| "this session does not hold the durable fleet lease".into()));
        };
        lease.is_current()?;
        let claim = lease.claim();
        if let Some(existing) = &record.claim {
            if existing.generation > claim.generation
                || (existing.generation == claim.generation && existing.instance != claim.instance)
            {
                return Err(format!(
                    "worker record is claimed by a newer session fleet owner (instance {}, generation {}); this owner holds generation {}",
                    existing.instance, existing.generation, claim.generation
                ));
            }
        }
        Ok(claim)
    }

    /// Whether the owning session released its root agent while workers lived.
    pub(super) fn session_owner_released(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .session_owner_released
    }

    /// Persist the session-owned fleet roster.
    ///
    /// Fail-closed in two directions: serialization or write failure marks the
    /// team unusable through the same `persistence_error` path as the
    /// provenance journal, and a manager that does not hold the session fleet
    /// lease never writes the roster, so a duplicate session open cannot
    /// overwrite the real owner's durable records.
    pub(super) fn persist_durable_fleet_locked(&self, state: &mut ManagerState) {
        let Some(path) = self.roster_path.clone() else {
            return;
        };
        if state.persistence_error.is_some() || !self.lease_held() {
            return;
        }
        if let Err(reason) = self.verify_held_fleet_lease() {
            self.fail_persistence_locked(state, &io::Error::other(reason));
            return;
        }
        let fleet = DurableFleet {
            version: FLEET_ROSTER_VERSION,
            root_session: self.root_session.clone(),
            records: state.records.values().map(durable_fleet_record).collect(),
            next_mailbox_delivery: state.next_mailbox_delivery,
            root_mailbox: state.root_mailbox.iter().map(Into::into).collect(),
            root_mailbox_delivery: state.root_mailbox_delivery,
        };
        let encoded = match encode_durable_fleet(fleet) {
            Ok(encoded) => encoded,
            Err(error) => {
                self.fail_persistence_locked(state, &io::Error::other(error));
                return;
            }
        };
        if encoded.len() > MAX_FLEET_ROSTER_BYTES {
            self.fail_persistence_locked(
                state,
                &io::Error::other("durable fleet roster exceeded its bounded size"),
            );
            return;
        }
        if let Err(error) = secure_fs::write_private_atomic(&path, &encoded, MAX_FLEET_ROSTER_BYTES)
        {
            self.fail_persistence_locked(state, &io::Error::other(error));
        }
    }

    /// Maps one durable record into the in-memory worker record.
    ///
    /// This is the only place the durable fields are threaded into
    /// [`AgentRecord`], so the roster restore and every test fixture share one
    /// mapping and a new durable field cannot be missed by a fixture. Fixtures
    /// state attachment explicitly afterwards, because `detached: true` here is
    /// the roster-restore default (a restored record has no live task).
    pub(super) fn agent_record_from_durable(
        durable: DurableFleetRecord,
        effective_tool_policy: EffectiveToolPolicy,
        refusal: Option<&str>,
        command_tx: mpsc::Sender<WorkerCommand>,
        command_rx: Option<mpsc::Receiver<WorkerCommand>>,
    ) -> AgentRecord {
        let status = match durable.status {
            DelegatedAgentStatus::Pending
            | DelegatedAgentStatus::Idle
            | DelegatedAgentStatus::Running => DelegatedAgentStatus::Detached,
            other => other,
        };
        // A record restored from an earlier owner is only runnable when the
        // restoring manager can prove a fresh fleet claim. Without one the
        // refusal is visible on the record instead of a silent stall.
        let resumable = !matches!(status, DelegatedAgentStatus::Shutdown);
        let claim_reason = match (refusal, resumable) {
            (Some(reason), true) => Some(format!("not reattached: {reason}")),
            _ => None,
        };
        let extension_policy = durable.extension_policy.clone();
        let orchestration_provenance = child_orchestration_provenance(extension_policy.as_ref());
        AgentRecord {
            identity: AgentIdentity {
                id: durable.agent_id,
                path: durable.agent_path,
                depth: durable.depth,
            },
            task_name: durable.task_name,
            display_task_name: durable.display_task_name,
            parent_id: durable.parent_id,
            session_path: durable.session_path,
            status,
            command_tx,
            shutdown: crate::CancellationToken::default(),
            interrupt_requested: false,
            pending_messages: durable.pending_messages,
            inflight_message_ids: BTreeSet::new(),
            reserved_messages: QueueUsage::default(),
            queued_follow_ups: durable.queued_follow_ups.iter().fold(
                QueueUsage::default(),
                |mut usage, follow_up| {
                    usage.add_usage(follow_up.usage());
                    usage
                },
            ),
            pending_follow_ups: durable.queued_follow_ups,
            pending_initial_task: durable.pending_initial_task,
            mailbox: durable.mailbox.into_iter().map(Into::into).collect(),
            mailbox_delivery: durable.mailbox_delivery,
            resource_owner: durable.resource_owner,
            extension_policy,
            effective_tool_policy,
            orchestration_provenance,
            extension_principal: durable.extension_principal,
            extension_profile: durable.extension_profile,
            extension_idempotency_key: durable.extension_idempotency_key,
            extension_resource_owner: durable.extension_resource_owner,
            extension_message_sha256: durable.extension_message_sha256,
            extension_requested_policy: durable.extension_requested_policy,
            extension_fingerprint: durable.extension_fingerprint,
            created_at_ms: durable.created_at_ms,
            started_at_ms: durable.started_at_ms,
            completed_at_ms: durable.completed_at_ms,
            turn_count: durable.turn_count,
            tool_call_count: durable.tool_call_count,
            active_tools: BTreeMap::new(),
            recent_tools: VecDeque::new(),
            usage: durable.usage,
            streamed_output_bytes: 0,
            usage_uncertain: durable.usage_uncertain,
            usage_exposure: durable.usage_exposure,
            cost: durable.cost,
            cost_microdollars: durable.cost_microdollars,
            deadline_at_ms: durable.deadline_at_ms,
            turn_limit: durable.turn_limit,
            detached: true,
            live_task: false,
            worker_generation: 0,
            // A restored record keeps its command receiver for every state that
            // an explicit follow-up can resume, so a later turn is never told
            // "no longer available" for a worker the session still owns.
            detached_commands: resumable.then_some(command_rx).flatten(),
            durable_diagnostic: claim_reason.or(durable.durable_diagnostic),
            claim: durable.claim,
        }
    }

    pub(super) fn discard_session_delivered_inputs(record: &mut AgentRecord) -> Result<(), String> {
        let file = secure_fs::open_private_file_for_read(&record.session_path)
            .map_err(|error| format!("could not open child session: {error}"))?;
        let session = Session::open_read_only_with_file(record.session_path.clone(), file)
            .map_err(|error| format!("could not read child session: {error}"))?;
        Self::discard_inputs_delivered_to_session(record, &session)
    }

    pub(super) fn discard_inputs_delivered_to_session(
        record: &mut AgentRecord,
        session: &Session,
    ) -> Result<(), String> {
        // Reconcile only the active head's ancestry. Historical entries on an
        // abandoned branch are not authority for a later resumed worker.
        let mut delivered = BTreeSet::new();
        let mut cursor = session.head();
        while let Some(id) = cursor {
            let entry = session
                .entry(&id)
                .ok_or_else(|| "child session has an invalid active ancestry".to_owned())?;
            if let crate::session::EntryValue::Message(octet_ai::Message::User(message)) =
                &entry.value
            {
                for text in message.content.iter().filter_map(|part| match part {
                    octet_ai::UserPart::Text(text) => Some(text.as_str()),
                    _ => None,
                }) {
                    delivered.extend(delivery_ids_in_envelopes(text));
                }
            }
            cursor = entry.parent.clone();
        }
        if record
            .pending_initial_task
            .as_ref()
            .is_some_and(|task| delivered.contains(&task.delivery_id))
        {
            record.pending_initial_task = None;
        }
        // Legacy empty IDs have no safe identity evidence and are retained.
        record.pending_messages.retain(|message| {
            message.delivery_id.is_empty() || !delivered.contains(&message.delivery_id)
        });
        record.pending_follow_ups.retain(|follow_up| {
            follow_up.delivery_id.is_empty() || !delivered.contains(&follow_up.delivery_id)
        });
        record.queued_follow_ups =
            record
                .pending_follow_ups
                .iter()
                .fold(QueueUsage::default(), |mut usage, follow_up| {
                    usage.add_usage(follow_up.usage());
                    usage
                });
        Ok(())
    }

    /// Reconstruct the session-owned fleet persisted by an earlier run or
    /// process.
    ///
    /// Records that were live when the snapshot was written have no task in
    /// this process, so they surface explicitly as
    /// [`DelegatedAgentStatus::Detached`] with a bounded diagnostic — never as
    /// a silently-forgotten worker. A malformed, oversized, foreign, or
    /// unreadable roster is ignored entirely (fail closed) rather than
    /// partially trusted.
    pub(super) fn read_durable_fleet(&self) -> Result<Option<DurableFleet>, String> {
        let Some(path) = self.roster_path.as_ref() else {
            return Ok(None);
        };
        let (bytes, from_legacy) =
            match secure_fs::read_private_file_bounded(path, MAX_FLEET_ROSTER_BYTES) {
                Ok(bytes) => (bytes, false),
                // Legacy rosters are read-only migration sources. New scoped
                // snapshots always take precedence, even when unreadable.
                Err(SecureFileError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    let legacy = self.config.session_directory.join(FLEET_ROSTER_FILE);
                    match secure_fs::read_private_file_bounded(&legacy, MAX_FLEET_ROSTER_BYTES) {
                        Ok(bytes) => (bytes, true),
                        Err(SecureFileError::Io(error))
                            if error.kind() == io::ErrorKind::NotFound =>
                        {
                            return Ok(None);
                        }
                        Err(error) => {
                            return Err(format!("durable fleet roster is unavailable: {error}"));
                        }
                    }
                }
                Err(error) => return Err(format!("durable fleet roster is unavailable: {error}")),
            };
        let fleet: DurableFleet = serde_json::from_slice(&bytes)
            .map_err(|error| format!("durable fleet roster is invalid: {error}"))?;
        if from_legacy && fleet.root_session != self.root_session {
            return Ok(None);
        }
        if !matches!(fleet.version, 1 | FLEET_ROSTER_VERSION)
            || fleet.root_session != self.root_session
        {
            return Err("durable fleet roster has an incompatible version or owner".into());
        }
        Ok(Some(fleet))
    }

    pub(super) fn restore_durable_fleet(&self) {
        match self.read_durable_fleet() {
            Ok(Some(fleet)) => self.restore_durable_fleet_from(fleet, false),
            Ok(None) => {}
            Err(reason) if self.lease_held() => {
                // A claimed but unreadable roster is not an empty fleet. Never
                // admit a new worker that would overwrite unknown accepted work.
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                self.fail_persistence_locked(&mut state, &io::Error::other(reason));
            }
            Err(_) => {} // A lease-refused observer has no authority to repair it.
        }
    }

    /// A new lease must replace the observer's old projection, including
    /// counters and mailboxes, before any admission or roster write.
    pub(super) fn restore_durable_fleet_from(&self, fleet: DurableFleet, replace: bool) {
        let effective_tool_policy = self
            .template
            .sandbox
            .effective_tool_policy(self.template.effect_broker.policy());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.shutting_down || state.persistence_error.is_some() {
            return;
        }
        if replace {
            state.records.clear();
            state.total_agents = 1;
            state.next_agent_number = 1;
            state.root_mailbox.clear();
            state.root_mailbox_delivery = None;
        }
        state.next_mailbox_delivery = fleet.next_mailbox_delivery.max(1);
        state.root_mailbox = fleet.root_mailbox.into_iter().map(Into::into).collect();
        state.root_mailbox_delivery = fleet.root_mailbox_delivery;
        let refusal = self.lease_refusal_reason();
        for durable in fleet.records {
            if state.records.contains_key(&durable.agent_id) {
                continue;
            }
            let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
            let mut record = Self::agent_record_from_durable(
                durable,
                effective_tool_policy.clone(),
                refusal.as_deref(),
                command_tx,
                Some(command_rx),
            );
            // The child session is authoritative for prompt delivery. A roster
            // snapshot can lag a successful append, so never replay a queued
            // input that is already present in that durable session. An unreadable
            // authority fails closed rather than guessing from queue payloads.
            if let Err(error) = Self::discard_session_delivered_inputs(&mut record) {
                // Unavailable authority is not evidence of delivery. Preserve
                // every accepted payload and attempt count until a successful
                // reopen can reconcile them, even across another restart.
                record.status = DelegatedAgentStatus::Detached;
                record.detached = true;
                record.durable_diagnostic = Some(bounded_text(&format!(
                    "child session authority could not be read; queued inputs retained without replay: {error}"
                )));
            }
            if let Some(number) = agent_number_from_id(&record.identity.id) {
                state.next_agent_number = state.next_agent_number.max(number.saturating_add(1));
            }
            state.records.insert(record.identity.id.clone(), record);
            state.total_agents = state.total_agents.saturating_add(1);
        }
        // Upgrade legacy records and acknowledge any child-session delivery
        // observations before another process can reconstruct this roster.
        self.persist_durable_fleet_locked(&mut state);
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    /// Session-scoped detachment: leave the fleet alive when the owning run
    /// ends and record that boundary explicitly.
    ///
    /// Nothing is cancelled or cleared. Workers that need no new authority
    /// keep running; workers that do park in
    /// [`DelegatedAgentStatus::AwaitingApproval`]. The durable roster is
    /// refreshed so a later turn or a restarted process can reconstruct the
    /// fleet.
    pub(super) fn detach_run(&self, owner: &AgentIdentity) {
        if owner.id != ROOT_AGENT_ID {
            return;
        }
        let detached_ids = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.shutting_down || state.persistence_error.is_some() {
                return;
            }
            let mut detached_ids = Vec::new();
            for record in state.records.values_mut() {
                if record.status.is_running() || record.status.is_recoverable() {
                    record.detached = true;
                    detached_ids.push(record.identity.id.clone());
                }
            }
            self.persist_durable_fleet_locked(&mut state);
            detached_ids
        };
        if !detached_ids.is_empty() {
            let event = ProvenanceEvent::RunDetached {
                timestamp_ms: timestamp_ms(),
                agent_ids: detached_ids,
            };
            if let Ok(encoded) = serde_json::to_vec(&event) {
                let _ = self.journal.append_encoded(&encoded);
            }
        }
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    /// Reattach every detached record the owning session can prove it may run.
    ///
    /// Reattachment runs only undelivered durable tasks. If reconciliation
    /// leaves no task, the worker settles as interrupted with its session
    /// retained for an explicit follow-up; it must not idle forever as pending
    /// or replay already delivered work. Each start acquires a permit so the
    /// concurrency cap still holds across the turn boundary.
    ///
    /// Every start requires a fresh durable fleet claim: a manager whose lease
    /// was refused, was superseded, or cannot be re-proved refuses the record
    /// and keeps the bounded reason on it. A worker parked at the approval
    /// boundary is never resumed here — it stays parked until an explicit
    /// decision arrives. Reattachment emits the same journaled lifecycle
    /// notifications the live spawn path emits, plus an explicit
    /// `run_reattached` boundary record.
    pub(super) fn reattach_detached(self: &Arc<Self>, owner: &AgentIdentity) -> Result<(), String> {
        if owner.id != ROOT_AGENT_ID {
            return Ok(());
        }
        let _journal_order = self
            .journal_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Prove (or re-acquire) the durable claim before trusting any record.
        // A refusal is recorded on every affected record and surfaced below.
        let _ = self.ensure_fleet_lease();
        let mut plans = Vec::new();
        let mut reattached = Vec::new();
        let mut parked = Vec::new();
        let mut refused = Vec::new();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.shutting_down {
                return Err("delegation team is shutting down".into());
            }
            if let Some(error) = &state.persistence_error {
                return Err(format!("delegation persistence is unavailable: {error}"));
            }
            let ids = state.records.keys().cloned().collect::<Vec<_>>();
            for id in ids {
                let Some(record) = state.records.get(&id) else {
                    continue;
                };
                // Two kinds of record are reattachable at a new owning run:
                // * a live worker that survived the previous turn in this process
                //   (`detached` marker set while its task keeps running), and
                // * a durable record reconstructed from the roster with no live
                //   task (`Detached`).
                let live_task = record.live_task;
                let detached = record.detached || record.status.is_recoverable();
                if !detached {
                    continue;
                }
                if live_task {
                    // The worker still owns its receiver and its permit in this
                    // process: reattachment only clears the run-scoped
                    // detachment marker. No second worker, no second permit.
                    if let Some(record) = state.records.get_mut(&id) {
                        record.detached = false;
                        record.durable_diagnostic = None;
                    }
                    continue;
                }
                let park_reason = match &record.status {
                    DelegatedAgentStatus::AwaitingApproval { reason } => Some(reason.clone()),
                    _ => None,
                };
                if let Some(reason) = park_reason {
                    // Unattended mutation fails closed. A worker parked on
                    // authority it no longer has stays parked across
                    // reattachment; only an explicit decision (an owner-bound
                    // follow-up issued in a run that carries authority) may
                    // resume it.
                    let diagnostic = format!(
                        "parked at the approval boundary and not resumed by reattachment; an explicit decision is required: {reason}"
                    );
                    if let Some(record) = state.records.get_mut(&id) {
                        record.detached = true;
                        record.durable_diagnostic = Some(bounded_text(&diagnostic));
                    }
                    parked.push((id, reason));
                    continue;
                }
                if !matches!(record.status, DelegatedAgentStatus::Detached)
                    && !record.status.is_running()
                {
                    // A settled record is resumed by an explicit follow-up, not
                    // by reattachment.
                    continue;
                }
                let claim = match self.fresh_claim_for(record) {
                    Ok(claim) => claim,
                    Err(reason) => {
                        let diagnostic = format!("worker was not reattached: {reason}");
                        if let Some(record) = state.records.get_mut(&id) {
                            record.detached = true;
                            record.durable_diagnostic = Some(bounded_text(&diagnostic));
                        }
                        refused.push((id, bounded_text(&reason)));
                        continue;
                    }
                };
                let session = match self.reopen_child_session(&record.session_path) {
                    Ok(session) => session,
                    Err(error) => {
                        let reason = bounded_text(&format!(
                            "worker was not reattached: the child session could not be reopened: {error}"
                        ));
                        let record = state.records.get_mut(&id).expect("selected child exists");
                        record.status = DelegatedAgentStatus::Detached;
                        record.detached = true;
                        record.durable_diagnostic = Some(reason.clone());
                        refused.push((id, reason));
                        continue;
                    }
                };
                let record = state.records.get_mut(&id).expect("selected child exists");
                if let Err(error) = Self::discard_inputs_delivered_to_session(record, &session) {
                    let reason = bounded_text(&format!("worker was not reattached: {error}"));
                    record.detached = true;
                    record.durable_diagnostic = Some(reason.clone());
                    refused.push((id, reason));
                    continue;
                }
                // A delivered task needs no execution slot. Otherwise many
                // interrupted transcripts can starve a later runnable child.
                let permit = if record.pending_initial_task.is_some()
                    || !record.pending_follow_ups.is_empty()
                {
                    match self.current_permits().try_acquire_owned() {
                        Ok(permit) => Some(permit),
                        // The bound is authoritative: a record that cannot acquire
                        // a slot stays visibly detached instead of oversubscribing.
                        Err(_) => {
                            let reason = "no free execution slot is available for reattachment";
                            if let Some(record) = state.records.get_mut(&id) {
                                record.detached = true;
                                record.durable_diagnostic = Some(bounded_text(&format!(
                                    "worker was not reattached: {reason}"
                                )));
                            }
                            refused.push((id, reason.to_owned()));
                            continue;
                        }
                    }
                } else {
                    None
                };
                // Keep receivers and liveness on the records until the entire
                // batch's provenance is committed. A failed reopen only refuses
                // this child; a journal failure leaves all unstarted receivers
                // owned by their records and drops the reserved permits.
                reattached.push(id.clone());
                plans.push(ReattachPlan {
                    id,
                    session,
                    initial_permit: permit,
                    claim,
                });
            }
        }
        // Journal the reattachment boundary before publishing runnable tasks
        // or settling delivered-only transcripts as interrupted.
        for plan in &plans {
            let status = if plan.initial_permit.is_some() {
                DelegatedAgentStatus::Pending
            } else {
                DelegatedAgentStatus::Interrupted
            };
            let event = ProvenanceEvent::AgentStatus {
                timestamp_ms: timestamp_ms(),
                agent_id: &plan.id,
                status: &status,
            };
            if let Err(error) = self.journal.append(&event) {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment provenance: {error}"
                ));
            }
        }
        if !reattached.is_empty() {
            let event = ProvenanceEvent::RunReattached {
                timestamp_ms: timestamp_ms(),
                agent_ids: reattached.clone(),
            };
            if let Err(error) = self.journal.append(&event) {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment provenance: {error}"
                ));
            }
        }
        for (id, reason) in &parked {
            let event = ProvenanceEvent::ReattachParked {
                timestamp_ms: timestamp_ms(),
                agent_id: id,
                reason,
            };
            if let Err(error) = self.journal.append(&event) {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment provenance: {error}"
                ));
            }
        }
        for (id, reason) in &refused {
            let event = ProvenanceEvent::ReattachRefused {
                timestamp_ms: timestamp_ms(),
                agent_id: id,
                reason,
            };
            if let Err(error) = self.journal.append(&event) {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment provenance: {error}"
                ));
            }
        }
        for plan in plans {
            let has_tasks = !self.restored_tasks(&plan.id).is_empty();
            let status = if has_tasks {
                DelegatedAgentStatus::Pending
            } else {
                DelegatedAgentStatus::Interrupted
            };
            let settled_at = timestamp_ms();
            if let Err(error) = self.journal.append(&ProvenanceEvent::AgentStatus {
                timestamp_ms: settled_at,
                agent_id: &plan.id,
                status: &status,
            }) {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment settlement: {error}"
                ));
            }
            let startup = {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let record = state
                    .records
                    .get_mut(&plan.id)
                    .expect("selected child exists");
                record.status = status;
                record.detached = false;
                record.claim = Some(plan.claim);
                record.live_task = has_tasks;
                let startup = if has_tasks {
                    let commands = match record.detached_commands.take() {
                        Some(commands) => commands,
                        None => {
                            let (tx, rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
                            record.command_tx = tx;
                            rx
                        }
                    };
                    record.durable_diagnostic = None;
                    record.worker_generation += 1;
                    Some(WorkerStartup {
                        generation: record.worker_generation,
                        identity: record.identity.clone(),
                        session: plan.session,
                        commands,
                        shutdown: record.shutdown.clone(),
                        initial_permit: plan
                            .initial_permit
                            .expect("runnable reattachment reserved a slot"),
                        extension_policy: record.extension_policy.clone(),
                        deadline: record.deadline_at_ms.map(wall_deadline_instant),
                        deadline_ms: record.deadline_at_ms,
                    })
                } else {
                    record.completed_at_ms = Some(u64::try_from(settled_at).unwrap_or(u64::MAX));
                    record.durable_diagnostic = Some(
                        "worker was interrupted before its outcome was retained; no undelivered task remains. Use subagent_continue (or followup_task) to resume the retained session explicitly; delivered work was not replayed".into(),
                    );
                    None
                };
                self.persist_durable_fleet_locked(&mut state);
                if let Some(error) = &state.persistence_error {
                    return Err(format!(
                        "could not persist reattachment settlement: {error}"
                    ));
                }
                startup
            };
            if let Some(startup) = startup {
                self.spawn_worker(startup);
            }
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.persist_durable_fleet_locked(&mut state);
            if let Some(error) = &state.persistence_error {
                return Err(format!("could not persist reattachment roster: {error}"));
            }
        }
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
        Ok(())
    }

    /// In-process resolution of a worker's launchable interactive handle.
    ///
    /// Unlike [`resolve_launchable_child_session`], this knows the
    /// process-local liveness the durable roster cannot carry, so it refuses a
    /// session that a live worker still owns.
    pub(crate) fn launchable_child_session(
        &self,
        reference: &str,
    ) -> Result<LaunchableChildSession, DelegationError> {
        validate_launch_reference(reference)?;
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = state
            .records
            .values()
            .find(|record| {
                delegated_session_reference(&record.session_path).as_deref() == Some(reference)
            })
            .ok_or_else(|| DelegationError::Unlaunchable("unknown worker handle".into()))?;
        launchability(record).map_err(DelegationError::Unlaunchable)?;
        Ok(LaunchableChildSession {
            reference: reference.to_owned(),
            session_path: record.session_path.clone(),
            agent_id: record.identity.id.clone(),
            agent_path: record.identity.path.clone(),
            status: record.status.label().to_owned(),
        })
    }
}
