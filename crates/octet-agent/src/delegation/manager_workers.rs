//! DelegationManager workers: spawning, the worker loop and child runs.

use super::*;

impl DelegationManager {
    pub(super) fn tools(self: &Arc<Self>, identity: &AgentIdentity) -> Vec<Arc<dyn Tool>> {
        CollaborationToolKind::ALL
            .into_iter()
            .map(|kind| {
                Arc::new(CollaborationTool {
                    manager: Arc::downgrade(self),
                    owner: identity.clone(),
                    kind,
                }) as Arc<dyn Tool>
            })
            .collect()
    }

    pub(super) fn prepare_owning_run(
        self: &Arc<Self>,
        owner: &AgentIdentity,
    ) -> Result<(), String> {
        if owner.id == ROOT_AGENT_ID {
            if owner.path != ROOT_AGENT_PATH || owner.depth != 0 {
                return Err("invalid root delegation identity".into());
            }
            // Session-scoped lifetime: a new owning run reattaches the fleet
            // that survived the previous turn instead of retiring it. Live
            // workers keep running; restored workers run undelivered tasks or
            // settle as interrupted for explicit continuation. Execution capacity
            // is *not* handed back here: a surviving worker keeps its slot, so
            // the cap cannot drift up on reattachment.
            {
                let _journal_order = self
                    .journal_order
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let owns_lease = self.ensure_fleet_lease().is_ok();
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(error) = &state.persistence_error {
                    return Err(format!("delegation persistence is unavailable: {error}"));
                }
                if state.shutting_down {
                    return Err("delegation team is shutting down".into());
                }
                // A previous explicit stop (owner teardown, team stop, or a
                // dropped agent) reactivates for the next owning run so the
                // session can never be bricked by its own stop. Retired
                // records - shut-down workers with no live task and no
                // resumable work - release their name and slot here; the
                // journal keeps the shutdown evidence.
                state.root_active = true;
                // A worker parked by the previous session owner is reattachable
                // again at this new owning-run boundary.
                state.session_owner_released = false;
                let before = state.records.len();
                if owns_lease {
                    state.records.retain(|_, record| {
                        !matches!(record.status, DelegatedAgentStatus::Shutdown)
                            || record.detached_commands.is_some()
                    });
                }
                if state.records.len() != before {
                    state.total_agents = state
                        .total_agents
                        .saturating_sub(before - state.records.len())
                        .max(1);
                    self.persist_durable_fleet_locked(&mut state);
                }
            }
            self.reattach_detached(owner)?;
            return Ok(());
        }

        // Serialize the reset against journaled operations'
        // decide → append → commit windows so it cannot interleave with
        // their commit phases.
        let _journal_order = self
            .journal_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_owner_active_locked(&state, owner)?;
        let descendants = state
            .records
            .iter()
            .filter(|(_, record)| is_descendant_path(&record.identity.path, &owner.path))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in &descendants {
            if let Some(record) = state.records.get(id) {
                record.shutdown.cancel();
                let _ = record.command_tx.try_send(WorkerCommand::shutdown());
            }
        }
        for id in descendants {
            state.records.remove(&id);
        }
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
        Ok(())
    }

    pub(super) fn current_permits(&self) -> Arc<Semaphore> {
        Arc::clone(
            &self
                .permits
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    pub(super) fn spawn(
        self: &Arc<Self>,
        owner: &AgentIdentity,
        request: SpawnRequest,
    ) -> Result<Value, String> {
        let SpawnRequest {
            task_name,
            display_task_name,
            message,
            mut extension_policy,
            extension_provenance,
        } = request;
        validate_task_name(&task_name)?;
        if let Some(display_task_name) = display_task_name.as_deref() {
            validate_task_name(display_task_name)?;
        }
        validate_durable_text("spawn task", &message)?;
        if extension_provenance.is_some() != extension_policy.is_some() {
            return Err(
                "extension delegation policy and durable ownership provenance must be paired"
                    .into(),
            );
        }
        if let Some(provenance) = extension_provenance.as_ref() {
            if provenance.parent_session_id.trim().is_empty()
                || provenance.parent_session_id.len() > 256
                || provenance
                    .parent_session_id
                    .chars()
                    .any(char::is_whitespace)
            {
                return Err("invalid extension delegation parent session".into());
            }
            if provenance.principal.trim().is_empty() || provenance.principal.len() > 256 {
                return Err("invalid extension delegation principal".into());
            }
            ExtensionDelegationService::validate_resource_owner(&provenance.resource_owner)?;
        }
        let extension_requested_policy = extension_policy.clone();
        let resolved = self.template.resolve_model(extension_policy.as_ref())?;
        if extension_policy.is_some()
            && resolved.model.spec.pricing.is_none()
            && (extension_policy
                .as_ref()
                .is_some_and(|p| p.max_cost_microdollars.is_some())
                || self
                    .template
                    .runtime
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .max_session_cost_microdollars
                    .is_some())
        {
            return Err(
                "extension children with a cost ceiling require trusted model pricing".into(),
            );
        }
        if let Some(policy) = extension_policy.as_mut() {
            policy.resolved_model = Some(resolved.metadata.clone());
            policy.resolved_reasoning = Some(resolved.reasoning.clone());
        }
        if let Some(policy) = extension_policy.as_mut() {
            policy.validate()?;
            if owner.depth.saturating_add(1) > policy.max_depth {
                return Err(format!(
                    "extension child depth limit reached at {} (max depth {})",
                    owner.path, policy.max_depth
                ));
            }
            let allowed = policy.tools.iter().cloned().collect::<BTreeSet<_>>();
            let (_, effective_tools) = self.template.extensions.scoped_tool_snapshot(&allowed)?;
            policy.tools = effective_tools;
            if let Some(parent_turns) = self.template.max_turns {
                policy.max_turns = match policy.max_turns {
                    Some(requested) => Some(requested.min(parent_turns)),
                    None => Some(parent_turns),
                };
            }
            let runtime = self
                .template
                .runtime
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(parent_tokens) = runtime.max_session_tokens {
                policy.max_tokens = Some(
                    policy
                        .max_tokens
                        .map_or(parent_tokens, |requested| requested.min(parent_tokens)),
                );
            }
            if let Some(parent_cost) = runtime.max_session_cost_microdollars {
                policy.max_cost_microdollars = match policy.max_cost_microdollars {
                    Some(requested) => Some(requested.min(parent_cost)),
                    None => Some(parent_cost),
                };
            }
        }
        let orchestration_provenance = child_orchestration_provenance(extension_policy.as_ref());
        let effective_tool_policy = self
            .template
            .sandbox
            .effective_tool_policy(self.template.effect_broker.policy());
        let initial_task = message;
        let initial_delivery_id = new_delivery_id()?;
        if owner.depth >= self.config.limits.max_depth {
            return Err(format!(
                "delegation depth limit reached at {} (max depth {})",
                owner.path, self.config.limits.max_depth
            ));
        }
        let permit = self.current_permits().try_acquire_owned().map_err(|_| {
            "delegation concurrency limit reached; wait for an active agent".to_owned()
        })?;

        let created_at_ms = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
        let deadline = extension_policy.as_ref().and_then(|policy| {
            policy
                .timeout_ms
                .map(|timeout_ms| tokio::time::Instant::now() + Duration::from_millis(timeout_ms))
        });
        let deadline_at_ms = extension_policy
            .as_ref()
            .and_then(|policy| policy.timeout_ms)
            .map(|timeout_ms| created_at_ms.saturating_add(timeout_ms));

        let (identity, session, command_rx, shutdown, task_name) = {
            // The journal_order guard spans decide → append → commit so that
            // journal record order matches state mutation order, while the
            // state lock is dropped across the journal's durable `sync_data`.
            let _journal_order = self
                .journal_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_fleet_lease()?;
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_owner_active_locked(&state, owner)?;
            if state.total_agents >= self.config.limits.max_total_agents {
                return Err(format!(
                    "delegation team agent limit reached ({})",
                    self.config.limits.max_total_agents
                ));
            }
            let child_path = format!("{}/{}", owner.path.trim_end_matches('/'), task_name);
            if let Some(existing) = state
                .records
                .values()
                .find(|record| record.identity.path == child_path)
            {
                // Worker names are session-scoped, not run-scoped: the name
                // stays owned by the surviving worker's durable record, so the
                // refusal names that worker and the tool that resumes it
                // instead of letting a second worker shadow it.
                return Err(existing_task_name_error(existing));
            }
            let number = state.next_agent_number;
            state.next_agent_number = state.next_agent_number.saturating_add(1);
            let identity = AgentIdentity {
                id: format!("agent-{number}"),
                path: child_path,
                depth: owner.depth + 1,
            };
            let session_path = self
                .team_directory
                .join(format!("{number:04}-{task_name}.jsonl"));
            // Create the isolated durable session before publishing the worker.
            let session_file = self
                .create_team_file(&session_path)
                .map_err(|error| error.to_string())?;
            let session = match Session::create_with_file(&session_path, session_file) {
                Ok(session) => session,
                Err(error) => {
                    let _ = self.remove_team_file_if_exists(&session_path);
                    return Err(error.to_string());
                }
            };
            let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
            let shutdown = crate::CancellationToken::default();
            let extension_session_reference = extension_provenance.as_ref().map(|_| {
                delegated_session_reference(&session_path)
                    .expect("generated extension child path has a delegated session reference")
            });
            let encoded = match serde_json::to_vec(&ProvenanceEvent::AgentSpawned {
                timestamp_ms: u128::from(created_at_ms),
                agent_id: &identity.id,
                agent_path: &identity.path,
                parent_id: &owner.id,
                task_name: &task_name,
                display_task_name: display_task_name.as_deref(),
                extension_parent_session_id: extension_provenance
                    .as_ref()
                    .map(|provenance| provenance.parent_session_id.as_str()),
                extension_principal: extension_provenance
                    .as_ref()
                    .map(|provenance| provenance.principal.as_str()),
                extension_resource_owner: extension_provenance
                    .as_ref()
                    .map(|provenance| provenance.resource_owner.as_str()),
                extension_profile: extension_provenance
                    .as_ref()
                    .and_then(|provenance| provenance.profile.as_deref()),
                extension_idempotency_key: extension_provenance
                    .as_ref()
                    .map(|provenance| provenance.idempotency_key.as_str()),
                extension_fingerprint: extension_provenance
                    .as_ref()
                    .and_then(|provenance| provenance.fingerprint.as_deref()),
                task: extension_provenance
                    .is_none()
                    .then_some(initial_task.as_str()),
                session: extension_provenance
                    .is_none()
                    .then_some(session_path.as_path()),
                session_reference: extension_session_reference.as_deref(),
                effective_tool_policy: &effective_tool_policy,
                orchestration_provenance: &orchestration_provenance,
            }) {
                Ok(encoded) => encoded,
                Err(error) => {
                    let message = format!("could not persist delegation provenance: {error}");
                    let error = io::Error::other(error);
                    self.fail_persistence_locked(&mut state, &error);
                    drop(state);
                    drop(command_tx);
                    drop(session);
                    let _ = self.remove_team_file_if_exists(&session_path);
                    return Err(message);
                }
            };
            drop(state);
            // Durable provenance sync runs without the state lock held.
            if let Err(error) = self.journal.append_encoded(&encoded) {
                let message = format!("could not persist delegation provenance: {error}");
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                drop(state);
                drop(command_tx);
                drop(session);
                let _ = self.remove_team_file_if_exists(&session_path);
                return Err(message);
            }
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.persistence_error.is_some() || !state.root_active || state.shutting_down {
                // The team was invalidated mid-operation; discard the prepared
                // worker instead of resurrecting state after its provenance.
                let message = "delegation team is shutting down".to_owned();
                drop(state);
                drop(command_tx);
                drop(session);
                let _ = self.remove_team_file_if_exists(&session_path);
                return Err(message);
            }
            let turn_limit = extension_policy
                .as_ref()
                .and_then(|policy| policy.max_turns)
                .or(self.template.max_turns);
            state.records.insert(
                identity.id.clone(),
                AgentRecord {
                    identity: identity.clone(),
                    task_name: task_name.clone(),
                    display_task_name: display_task_name.clone(),
                    parent_id: owner.id.clone(),
                    session_path: session_path.clone(),
                    status: DelegatedAgentStatus::Pending,
                    command_tx: command_tx.clone(),
                    shutdown: shutdown.clone(),
                    interrupt_requested: false,
                    pending_messages: VecDeque::new(),
                    inflight_message_ids: BTreeSet::new(),
                    reserved_messages: QueueUsage::default(),
                    queued_follow_ups: QueueUsage::default(),
                    pending_follow_ups: VecDeque::new(),
                    pending_initial_task: Some(QueuedInitialTask {
                        task: initial_task.clone(),
                        delivery_id: initial_delivery_id,
                        attempts: 0,
                    }),
                    mailbox: VecDeque::new(),
                    mailbox_delivery: None,
                    resource_owner: None,
                    extension_policy: extension_policy.clone(),
                    effective_tool_policy: effective_tool_policy.clone(),
                    orchestration_provenance: orchestration_provenance.clone(),
                    extension_principal: extension_provenance
                        .as_ref()
                        .map(|provenance| provenance.principal.clone()),
                    extension_profile: extension_provenance
                        .as_ref()
                        .and_then(|provenance| provenance.profile.clone()),
                    extension_idempotency_key: extension_provenance
                        .as_ref()
                        .map(|provenance| provenance.idempotency_key.clone()),
                    extension_resource_owner: extension_provenance
                        .as_ref()
                        .map(|provenance| provenance.resource_owner.clone()),
                    extension_message_sha256: extension_provenance
                        .as_ref()
                        .map(|_| format!("{:x}", Sha256::digest(initial_task.as_bytes()))),
                    extension_requested_policy,
                    extension_fingerprint: extension_provenance
                        .as_ref()
                        .and_then(|provenance| provenance.fingerprint.clone()),
                    created_at_ms,
                    started_at_ms: None,
                    completed_at_ms: None,
                    turn_count: 0,
                    tool_call_count: 0,
                    active_tools: BTreeMap::new(),
                    recent_tools: VecDeque::new(),
                    child_events: ChildEventLog::default(),
                    usage: Usage::default(),
                    streamed_output_bytes: 0,
                    usage_uncertain: false,
                    usage_exposure: None,
                    cost: (extension_policy.is_some() && resolved.model.spec.pricing.is_some())
                        .then_some(Cost::default()),
                    cost_microdollars: (extension_policy.is_some()
                        && resolved.model.spec.pricing.is_some())
                    .then_some(0),
                    deadline_at_ms,
                    turn_limit,
                    detached: false,
                    live_task: true,
                    worker_generation: 1,
                    detached_commands: None,
                    durable_diagnostic: None,
                    claim: self.current_claim(),
                },
            );
            state.total_agents += 1;
            self.persist_durable_fleet_locked(&mut state);
            if let Some(error) = &state.persistence_error {
                return Err(format!("could not persist initial delegated task: {error}"));
            }
            (identity, session, command_rx, shutdown, task_name)
        };

        let result_policy = extension_policy.clone();
        let result_turn_limit = result_policy
            .as_ref()
            .and_then(|policy| policy.max_turns)
            .or(self.template.max_turns);
        self.spawn_worker(WorkerStartup {
            generation: 1,
            identity: identity.clone(),
            session,

            commands: command_rx,
            shutdown,
            initial_permit: permit,
            extension_policy,
            deadline,
            deadline_ms: deadline_at_ms,
        });
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);

        Ok(json!({
            "agent_id": identity.id,
            "agent_path": identity.path,
            "task_name": task_name,
            "profile": extension_provenance
                .as_ref()
                .and_then(|provenance| provenance.profile.as_deref()),
            "idempotency_key": extension_provenance
                .as_ref()
                .map(|provenance| provenance.idempotency_key.as_str()),
            "fingerprint": extension_provenance
                .as_ref()
                .and_then(|provenance| provenance.fingerprint.as_deref()),
            "status": "pending",
            "resolved_model": resolved_model_json(result_policy.as_ref()),
            "policy": public_policy_json(result_policy.as_ref()),
            "effective_tool_policy": effective_tool_policy,
            "orchestration_provenance": orchestration_provenance,
            "created_at_ms": created_at_ms,
            "started_at_ms": Value::Null,
            "completed_at_ms": Value::Null,
            "turn_limit": result_turn_limit,
            "deadline_at_ms": deadline_at_ms,
        }))
    }

    /// Keep a join handle for the actual worker future so a panic or external
    /// abort cannot strand its record in `Running`.
    pub(super) fn spawn_worker(self: &Arc<Self>, startup: WorkerStartup) {
        let id = startup.identity.id.clone();
        // The incarnation was advanced atomically with publication of the
        // pending record: an old supervisor cannot settle its replacement even
        // in the interval before this new task is actually spawned.
        let generation = startup.generation;
        let claim = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get(&id)
            .and_then(|record| record.claim.clone());
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let worker = Arc::clone(&supervisor);
            let result = tokio::spawn(async move { worker.run_worker(startup).await }).await;
            if let Err(error) = result {
                let cause = if error.is_panic() {
                    "worker task panicked"
                } else {
                    "worker task was aborted"
                };
                supervisor.mark_worker_aborted(&id, claim.as_ref(), generation, cause);
            }
        });
    }

    pub(super) async fn run_worker(self: Arc<Self>, startup: WorkerStartup) {
        let WorkerStartup {
            generation,
            identity,
            session,
            mut commands,
            shutdown,
            initial_permit,
            extension_policy,
            mut deadline,
            mut deadline_ms,
        } = startup;
        let session_path = session.path().to_path_buf();
        let _liveness = WorkerLiveness::new(&self, identity.id.clone(), generation);
        let mut unopened_session = Some(session);
        let mut agent = None;
        let mut queued_tasks = self.restored_tasks(&identity.id);
        let mut initial_permit = Some(initial_permit);
        let mut retry_undelivered_task = false;
        let mut retry_pending_messages = 0;
        let mut timeout_settled = false;
        loop {
            // A follow-up accepted for a settled worker may re-anchor the
            // host-owned wall budget; adopt a newer deadline before the
            // local, spawn-frozen budget can end the resumed worker.
            self.adopt_host_deadline(&identity, &mut deadline, &mut deadline_ms);
            if deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
                if !timeout_settled {
                    if !self.set_status(&identity.id, DelegatedAgentStatus::TimedOut, true) {
                        return;
                    }
                    self.request_shutdown_descendants(&identity.id);
                    initial_permit.take();
                    queued_tasks.retain(|task| matches!(task, QueuedTask::FollowUp(_)));
                    timeout_settled = true;
                }
                // Keep the receiver and accepted follow-ups, but perform no
                // work until an explicit follow-up re-anchors the host budget.
            } else {
                timeout_settled = false;
            }
            if shutdown.is_cancelled() {
                if self.session_owner_released() {
                    self.park_released_worker(&identity.id, Some(commands));
                } else {
                    self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                }
                self.request_shutdown_descendants(&identity.id);
                return;
            }
            if self.interrupt_requested(&identity.id) {
                queued_tasks.retain(|task| matches!(task, QueuedTask::FollowUp(_)));
                initial_permit.take();
                let saw_shutdown =
                    self.drain_interrupted_commands(&identity.id, &mut commands, &mut queued_tasks);
                if saw_shutdown || shutdown.is_cancelled() {
                    if self.session_owner_released() {
                        self.park_released_worker(&identity.id, Some(commands));
                    } else {
                        self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                    }
                    self.request_shutdown_descendants(&identity.id);
                    return;
                }
                self.set_status(&identity.id, DelegatedAgentStatus::Interrupted, true);
                self.request_shutdown_descendants(&identity.id);
                retry_undelivered_task = false;
                continue;
            }

            if queued_tasks.is_empty() {
                // An idle worker owns no execution slot. The fleet cap counts
                // running work, so a worker that reattached with nothing to do
                // releases its slot instead of parking on it; it acquires a new
                // one through `acquire_follow_up_permit` when work arrives.
                initial_permit.take();
            }
            if !queued_tasks.is_empty() && !retry_undelivered_task && !timeout_settled {
                if agent.is_none() {
                    let child_session = match unopened_session.take() {
                        Some(session) => Ok(session),
                        None => self.reopen_child_session(&session_path),
                    };
                    let build = child_session.and_then(|session| {
                        self.build_child_agent(session, &identity, extension_policy.as_ref())
                    });
                    match build {
                        Ok(child) => agent = Some(child),
                        Err(error) => {
                            // Initialization has not durably accepted the active
                            // task. Release its execution slot, preserve the task
                            // at the FIFO head, and retry only after explicit new
                            // work prevents a persistent failure hot loop.
                            initial_permit.take();
                            let task = queued_tasks.pop_front().expect("startup task exists");
                            let restored = restore_undelivered_task(
                                &mut queued_tasks,
                                task.clone(),
                                false,
                                &WorkerOutcome::Failed(error.to_string()),
                            );
                            self.persist_task_result(&identity.id, &task, &restored, false);
                            let diagnostic = match restored {
                                TaskRestore::DeadLettered { attempts } => format!(
                                    "delegated task was dead-lettered after {attempts} undelivered attempts: {error}"
                                ),
                                _ => format!(
                                    "delegated agent could not start; task retained for retry: {error}"
                                ),
                            };
                            self.fail_worker_start(&identity.id, bounded_text(&diagnostic));
                            self.request_shutdown_descendants(&identity.id);
                            retry_undelivered_task =
                                matches!(restored, TaskRestore::Restored { .. });
                            retry_pending_messages = self.pending_message_count(&identity.id);
                            continue;
                        }
                    }
                }
                let permit = if let Some(permit) = initial_permit.take() {
                    permit
                } else {
                    if !self.set_pending_if_needed(&identity.id) {
                        return;
                    }
                    match self
                        .acquire_follow_up_permit(&identity.id, &shutdown, deadline)
                        .await
                    {
                        PermitWait::Acquired(permit) => permit,
                        PermitWait::TimedOut => continue,
                        PermitWait::Interrupted => {
                            queued_tasks.retain(|task| matches!(task, QueuedTask::FollowUp(_)));
                            let saw_shutdown = self.drain_interrupted_commands(
                                &identity.id,
                                &mut commands,
                                &mut queued_tasks,
                            );
                            if saw_shutdown || shutdown.is_cancelled() {
                                if self.session_owner_released() {
                                    self.park_released_worker(&identity.id, Some(commands));
                                } else {
                                    self.set_status(
                                        &identity.id,
                                        DelegatedAgentStatus::Shutdown,
                                        true,
                                    );
                                }
                                self.request_shutdown_descendants(&identity.id);
                                return;
                            }
                            self.set_status(&identity.id, DelegatedAgentStatus::Interrupted, true);
                            self.request_shutdown_descendants(&identity.id);
                            continue;
                        }
                        PermitWait::Shutdown => {
                            if self.session_owner_released() {
                                self.park_released_worker(&identity.id, Some(commands));
                            } else {
                                self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                            }
                            self.request_shutdown_descendants(&identity.id);
                            return;
                        }
                    }
                };
                if shutdown.is_cancelled() {
                    drop(permit);
                    if self.session_owner_released() {
                        self.park_released_worker(&identity.id, Some(commands));
                    } else {
                        self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                    }
                    self.request_shutdown_descendants(&identity.id);
                    return;
                }
                if !self.set_status(&identity.id, DelegatedAgentStatus::Running, true) {
                    return;
                }
                let task = queued_tasks
                    .pop_front()
                    .expect("checked delegated task queue is not empty");
                let active_follow_up_usage = match &task {
                    QueuedTask::Initial(_) => None,
                    QueuedTask::FollowUp(follow_up) => Some(follow_up.usage()),
                };
                let pending = self.take_pending_messages(&identity.id);
                let formatted_task = task.format(&pending);
                let execution = self
                    .execute_child_run(
                        agent.as_mut().expect("delegated agent initialized"),
                        formatted_task,
                        ChildRunContext {
                            queued_delivery_ids: queued_tasks
                                .iter()
                                .chain(std::iter::once(&task))
                                .map(|task| task.delivery_id().to_owned())
                                .collect(),
                            identity: &identity,
                            commands: &mut commands,
                            shutdown: &shutdown,
                            extension_policy: extension_policy.as_ref(),
                            deadline,
                        },
                    )
                    .await;
                self.update_agent_session_accounting(
                    &identity.id,
                    agent
                        .as_ref()
                        .expect("delegated agent remains initialized")
                        .session(),
                    agent
                        .as_ref()
                        .expect("initialized child")
                        .model()
                        .spec
                        .pricing
                        .is_some(),
                );
                drop(permit);

                let WorkerExecution {
                    mut outcome,
                    deferred_follow_ups,
                    acknowledged_follow_ups,
                    task_delivered,
                } = execution;
                // An owning terminal signal wins a race with a child terminal
                // event that was already dequeued but not yet recorded.
                if shutdown.is_cancelled() {
                    outcome = WorkerOutcome::Shutdown;
                } else if !matches!(&outcome, WorkerOutcome::Shutdown)
                    && self.interrupt_requested(&identity.id)
                {
                    outcome = WorkerOutcome::Interrupted;
                }
                let mut delivered_follow_ups = acknowledged_follow_ups;
                if task_delivered {
                    self.release_prompt_message_reservations(&identity.id, &pending);
                } else if !matches!(&outcome, WorkerOutcome::Shutdown) {
                    self.restore_pending_messages(&identity.id, pending);
                }
                if task_delivered {
                    if let Some(usage) = active_follow_up_usage {
                        delivered_follow_ups.add_usage(usage);
                    }
                }
                let task_restore = restore_undelivered_task(
                    &mut queued_tasks,
                    task.clone(),
                    task_delivered,
                    &outcome,
                );
                self.persist_task_result(&identity.id, &task, &task_restore, task_delivered);
                let task_restored = matches!(task_restore, TaskRestore::Restored { .. });
                match task_restore {
                    TaskRestore::Restored { .. } => {}
                    TaskRestore::DeadLettered { attempts } => {
                        outcome = WorkerOutcome::Failed(format!(
                            "delegated task was dead-lettered after {attempts} undelivered attempts"
                        ));
                    }
                    TaskRestore::NotRestored => {}
                }
                self.release_follow_up_usage(&identity.id, delivered_follow_ups);
                let retained_pending_messages = self.pending_message_count(&identity.id);

                match outcome {
                    WorkerOutcome::Shutdown => {
                        if self.session_owner_released() {
                            self.park_released_worker(&identity.id, Some(commands));
                        } else {
                            self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                        }
                        self.request_shutdown_descendants(&identity.id);
                        return;
                    }
                    WorkerOutcome::TimedOut => {
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::FollowUp));
                        retry_undelivered_task = false;
                        retry_pending_messages = 0;
                    }
                    WorkerOutcome::Interrupted => {
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::follow_up));
                        let saw_shutdown = self.drain_interrupted_commands(
                            &identity.id,
                            &mut commands,
                            &mut queued_tasks,
                        );
                        if saw_shutdown || shutdown.is_cancelled() {
                            if self.session_owner_released() {
                                self.park_released_worker(&identity.id, Some(commands));
                            } else {
                                self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                            }
                            self.request_shutdown_descendants(&identity.id);
                            return;
                        }
                        self.set_status(&identity.id, DelegatedAgentStatus::Interrupted, true);
                        self.request_shutdown_descendants(&identity.id);
                        retry_undelivered_task = false;
                        retry_pending_messages = 0;
                    }
                    WorkerOutcome::LimitReached {
                        output,
                        turn_count,
                        turn_limit,
                    } => {
                        self.set_status(
                            &identity.id,
                            DelegatedAgentStatus::LimitReached {
                                output: bounded_text(&output),
                                turn_count,
                                turn_limit,
                            },
                            true,
                        );
                        self.request_shutdown_descendants(&identity.id);
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::follow_up));
                        // A max-turn run is a durable terminal completion, not
                        // a retryable task failure. Follow-ups may explicitly
                        // resume the same child session.
                        retry_undelivered_task = task_restored;
                        retry_pending_messages = retained_pending_messages;
                    }
                    WorkerOutcome::Failed(error) => {
                        if self.worker_is_detached(&identity.id)
                            && is_missing_approval_authority(&error)
                        {
                            // Unattended mutation fails closed. A detached
                            // worker that needs a decision it no longer has
                            // authority for parks in a bounded, durable state:
                            // it neither proceeds nor blocks forever. A later
                            // turn that reattaches can supply the decision.
                            self.set_status(
                                &identity.id,
                                DelegatedAgentStatus::AwaitingApproval {
                                    reason: bounded_text(&error),
                                },
                                true,
                            );
                            self.request_shutdown_descendants(&identity.id);
                            return;
                        }
                        self.set_status(
                            &identity.id,
                            DelegatedAgentStatus::Failed {
                                error: bounded_text(&error),
                            },
                            true,
                        );
                        self.request_shutdown_descendants(&identity.id);
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::follow_up));
                        // A pre-flight prompt failure did not durably accept the
                        // active task. Keep it at the FIFO head, but wait for an
                        // explicit message or follow-up instead of hot-looping
                        // on a persistent storage failure.
                        retry_undelivered_task = task_restored;
                        retry_pending_messages = retained_pending_messages;
                    }
                    WorkerOutcome::Completed(output) => {
                        self.set_status(
                            &identity.id,
                            DelegatedAgentStatus::Completed {
                                output: bounded_text(&output),
                            },
                            true,
                        );
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::follow_up));
                        retry_undelivered_task = false;
                        retry_pending_messages = 0;
                    }
                }
                continue;
            }

            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.interrupt_requested(&identity.id) {
                continue;
            }
            let command = tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    // The receiver is still borrowed by this select; the parked
                    // record rebuilds its command channel when it is reattached.
                    if self.session_owner_released() {
                        self.park_released_worker(&identity.id, None);
                    } else {
                        self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                    }
                    self.request_shutdown_descendants(&identity.id);
                    return;
                }
                _ = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if deadline.is_some() && !timeout_settled => {
                    // Re-check the authoritative deadline and settle once at
                    // the loop boundary; keep the idle receiver resumable.
                    continue;
                }
                _ = &mut notified => {
                    if retry_undelivered_task
                        && self.pending_message_count(&identity.id) > retry_pending_messages
                    {
                        retry_undelivered_task = false;
                        retry_pending_messages = 0;
                    }
                    continue;
                },
                command = commands.recv() => command,
                _ = async {
                    match agent.as_mut() {
                        Some(child) => child.drive_cache_warming().await,
                        None => std::future::pending().await,
                    }
                }, if !timeout_settled => {
                    let child = agent.as_ref().expect("idle warmer owns initialized child");
                    self.update_agent_session_accounting(
                        &identity.id,
                        child.session(),
                        child.model().spec.pricing.is_some(),
                    );
                    continue;
                }
            };
            let Some(command) = command else {
                self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, false);
                self.request_shutdown_descendants(&identity.id);
                return;
            };
            match command.kind {
                WorkerCommandKind::Message(message) => {
                    self.queue_reserved_message(&identity.id, message);
                    retry_undelivered_task = false;
                    retry_pending_messages = 0;
                }
                WorkerCommandKind::FollowUp => {
                    let pending = self.restored_tasks(&identity.id);
                    // A wakeup can outlive queue seeding or delivery. It is not
                    // a second acceptance and must not trigger another retry.
                    let new_work = pending.iter().any(|pending| {
                        !queued_tasks
                            .iter()
                            .any(|queued| queued.delivery_id() == pending.delivery_id())
                    });
                    queued_tasks = pending;
                    if !new_work {
                        continue;
                    }
                    retry_undelivered_task = false;
                    retry_pending_messages = 0;
                }
                WorkerCommandKind::Shutdown => {
                    self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                    self.request_shutdown_descendants(&identity.id);
                    return;
                }
            }
        }
    }

    /// A follow-up accepted for a settled worker starts a new run. When the
    /// original wall budget already elapsed, the manager re-anchors the
    /// record deadline; the worker's own deadline was frozen at spawn, so
    /// adopt the newer host-owned value before the local budget can end the
    /// resumed worker. Restored workers also initialize an absent local budget
    /// from that same durable absolute deadline.
    pub(super) fn adopt_host_deadline(
        &self,
        identity: &AgentIdentity,
        deadline: &mut Option<tokio::time::Instant>,
        deadline_ms: &mut Option<u64>,
    ) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get(&identity.id) else {
            return;
        };
        let Some(host) = record.deadline_at_ms else {
            return;
        };
        if deadline.is_none() || deadline_ms.is_none_or(|local| host > local) {
            *deadline = Some(wall_deadline_instant(host));
            *deadline_ms = Some(host);
        }
    }

    pub(super) fn build_child_agent(
        self: &Arc<Self>,
        mut session: Session,
        identity: &AgentIdentity,
        extension_policy: Option<&ExtensionAgentSessionPolicy>,
    ) -> Result<Agent, DelegationError> {
        let resolved = self
            .template
            .resolve_model(extension_policy)
            .map_err(DelegationError::InvalidConfig)?;
        let parent_path = identity
            .path
            .rsplit_once('/')
            .map(|(parent, _)| {
                if parent.is_empty() {
                    ROOT_AGENT_PATH
                } else {
                    parent
                }
            })
            .unwrap_or(ROOT_AGENT_PATH);
        let base_system = self
            .template
            .base_system
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let (system, extensions, max_turns) = if let Some(policy) = extension_policy {
            let allowed = policy.tools.iter().cloned().collect::<BTreeSet<_>>();
            let (extensions, effective) = self
                .template
                .extensions
                .scoped_tool_snapshot(&allowed)
                .map_err(DelegationError::InvalidConfig)?;
            if effective != policy.tools {
                return Err(DelegationError::InvalidConfig(
                    "effective extension child tool scope changed after spawn admission".into(),
                ));
            }
            let scope_note = if policy
                .tools
                .iter()
                .all(|tool| matches!(tool.as_str(), "read" | "search"))
            {
                "Only the listed read/search tools are installed; mutation, shell, and collaboration tools are unavailable."
            } else {
                "The listed standard file and shell tools are installed for this task; collaboration tools are unavailable. Mutating tools remain subject to the inherited approval policy; host policy is authoritative."
            };
            (
                format!(
                    "{base_system}\n\nThis is a host-enforced depth-one child session. {scope_note}"
                ),
                extensions,
                policy.max_turns,
            )
        } else {
            (
                format!(
                    "{}\n\n{}",
                    base_system,
                    child_instructions(identity, parent_path, &self.config.limits)
                ),
                self.template.extensions.clone(),
                self.template.max_turns,
            )
        };
        let runtime = self
            .template
            .runtime
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if extension_policy.is_some() {
            let mut cursor = session.head_ref();
            let mut matches_config = false;
            while let Some(id) = cursor {
                let entry = session.entry(id).expect("session branch entries exist");
                if let crate::session::EntryValue::Config {
                    model, reasoning, ..
                } = &entry.value
                {
                    matches_config = model.as_deref() == Some(&resolved.metadata.model)
                        && reasoning.as_deref() == Some(&resolved.metadata.reasoning);
                    break;
                }
                cursor = entry.parent.as_ref();
            }
            if !matches_config {
                session.append(crate::session::EntryValue::Config {
                    model: Some(resolved.metadata.model.clone()),
                    reasoning: Some(resolved.metadata.reasoning.clone()),
                    reasoning_mode: Some(
                        match self.template.reasoning_mode {
                            octet_ai::ReasoningMode::Standard => "standard",
                            octet_ai::ReasoningMode::Pro => "pro",
                        }
                        .into(),
                    ),
                })?;
            }
        }
        let mut agent = Agent::new(AgentConfig {
            client: self.template.client.clone(),
            model: resolved.model.clone(),
            session,
            system,
            sandbox: self.template.sandbox.clone(),
            effect_broker: self.template.effect_broker.clone(),
            extensions,
            max_turns,
            reasoning: resolved.reasoning.clone(),
            reasoning_mode: self.template.reasoning_mode,
            cache_retention: self.template.cache_retention,
            session_id: None,
        })?;
        if extension_policy.is_some() {
            agent.mark_ultra_observation_managed();
        }
        if let Some(record) = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get_mut(&identity.id)
        {
            record.resource_owner = Some(agent.resource_owner_id().to_owned());
        }
        agent.set_compaction_model(runtime.compaction_model);
        agent.set_compaction_token_mode(
            runtime.auto_compaction_mode,
            runtime.auto_compaction_threshold,
            runtime.compaction_keep_recent_tokens,
        )?;
        agent.set_completion_policy(runtime.completion_policy);
        agent.set_output_modalities(runtime.output_modalities);
        agent.set_tool_schema_budget_bytes(runtime.tool_schema_budget_bytes);
        if let Some(policy) = extension_policy {
            agent.inherit_max_output_tokens(
                runtime
                    .max_output_tokens
                    .min(resolved.model.spec.limits.max_output_tokens),
            );
            let max_session_tokens = match (runtime.max_session_tokens, policy.max_tokens) {
                (Some(parent), Some(requested)) => Some(parent.min(requested)),
                (Some(parent), None) => Some(parent),
                (None, requested) => requested,
            };
            let max_session_cost_microdollars = match (
                runtime.max_session_cost_microdollars,
                policy.max_cost_microdollars,
            ) {
                (Some(parent), Some(requested)) => Some(parent.min(requested)),
                (Some(parent), None) => Some(parent),
                (None, requested) => requested,
            };
            agent.set_max_session_tokens(max_session_tokens);
            agent.set_max_session_cost_microdollars(max_session_cost_microdollars);
        } else {
            agent.inherit_max_output_tokens(
                runtime
                    .max_output_tokens
                    .min(resolved.model.spec.limits.max_output_tokens),
            );
            agent.set_max_session_tokens(runtime.max_session_tokens);
            agent.set_max_session_cost_microdollars(runtime.max_session_cost_microdollars);
        }
        agent.inherit_cache_warming_mode_control(runtime.cache_warming_mode);
        agent.set_provider_retries_enabled(runtime.provider_retries_enabled);
        agent.set_max_network_wait(runtime.max_network_wait);
        if extension_policy.is_none() {
            let binding = DelegationBinding {
                manager: Arc::clone(self),
                identity: identity.clone(),
                system_instructions: Arc::from(""),
            };
            agent.install_delegation_tools(self.tools(identity));
            agent.set_delegation_binding(binding)?;
        }
        agent.finalize_tool_surface();
        Ok(agent)
    }

    pub(super) async fn execute_child_run(
        self: &Arc<Self>,
        agent: &mut Agent,
        task: String,
        context: ChildRunContext<'_>,
    ) -> WorkerExecution {
        let ChildRunContext {
            mut queued_delivery_ids,
            identity,
            commands,
            shutdown,
            extension_policy,
            deadline,
        } = context;
        if deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
            return WorkerExecution::new(WorkerOutcome::TimedOut);
        }
        if shutdown.is_cancelled() {
            return WorkerExecution::new(WorkerOutcome::Shutdown);
        }
        if self.interrupt_requested(&identity.id) {
            return WorkerExecution::new(WorkerOutcome::Interrupted);
        }
        // Row 3.5: one delegation boundary per driven child run. The child
        // agent observes with the span's derived context, so every span it
        // records nests under `octet.agent.delegation`.
        let delegation_guard = self
            .span_context()
            .begin_typed::<DelegationSpan>(EmptyAttributes {});
        agent.set_telemetry_context(delegation_guard.context());
        let entries_before_prompt = agent.session().entries().len();
        let session_path = agent.session().path().to_path_buf();
        // Bind prompt-error inspection to the exact session object already
        // owned by the child. Reopening the path after `prompt` would allow a
        // directory-entry replacement to misclassify durable task delivery.
        let inspection_file = match agent.session().try_clone_file() {
            Ok(file) => file,
            Err(error) => {
                return WorkerExecution::new(WorkerOutcome::Failed(format!(
                    "delegated task could not start because its session descriptor could not be cloned; task retained for retry: {error}"
                )));
            }
        };
        let persisted_task = task.clone();
        let prompt = agent.prompt(task).await;
        let mut run = match prompt {
            Ok(run) => run,
            Err(error) => {
                // `Run` borrows the agent on success, so inspect a clone of the
                // already-authorized session descriptor on this error path.
                let task_delivered =
                    Session::open_read_only_with_file(&session_path, inspection_file)
                        .ok()
                        .map(|session| {
                            session
                                .entries()
                                .iter()
                                .skip(entries_before_prompt)
                                .any(|entry| {
                                    matches!(
                                        &entry.value,
                                        crate::session::EntryValue::Message(
                                            octet_ai::Message::User(message)
                                        ) if message.content.len() == 1
                                            && matches!(
                                                &message.content[0],
                                                octet_ai::UserPart::Text(text)
                                                    if text == &persisted_task
                                            )
                                    )
                                })
                        })
                        .unwrap_or(false);
                let diagnostic = if task_delivered {
                    format!(
                        "delegated run could not start after the task was durably accepted: {error}"
                    )
                } else {
                    format!(
                        "delegated prompt was not durably accepted; task retained for retry: {error}"
                    )
                };
                let mut execution = WorkerExecution::new(WorkerOutcome::Failed(diagnostic));
                execution.task_delivered = task_delivered;
                return execution;
            }
        };
        let control = run.control();
        if extension_policy.is_some() {
            self.record_child_event(
                &identity.id,
                json!({"kind": "run_started", "message": persisted_task}),
            );
        }

        let output_limit = extension_policy
            .map(|policy| policy.max_output_bytes)
            .unwrap_or(MAX_PROVENANCE_TEXT_BYTES);
        let mut output = String::new();
        // A successful nonblocking control enqueue is not delivery: the agent
        // acknowledges only after the input has been appended durably. Keep the
        // original work and its queue reservation until that event so a run
        // failure cannot silently discard accepted steering or follow-ups.
        let mut submitted_messages = VecDeque::new();
        let mut deferred_messages = VecDeque::new();
        let mut defer_messages = false;
        let mut submitted_follow_ups = VecDeque::new();
        let mut deferred_follow_ups = VecDeque::new();
        let mut defer_follow_ups = queued_delivery_ids.len() > 1;
        let mut acknowledged_follow_ups = QueueUsage::default();
        let mut requested_interrupt = false;
        let mut requested_shutdown = false;
        let mut requested_timeout = false;
        let mut requested_token_limit = false;
        let turn_limit = extension_policy
            .and_then(|policy| policy.max_turns)
            .or(self.template.max_turns);
        let mut turns_completed = 0_u64;
        let mut last_stream_update = None;
        let mut commands_open = true;
        enum Next {
            Event,
            Command(Option<WorkerCommand>),
            Changed,
            Shutdown,
            Deadline,
        }
        let outcome = loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !requested_interrupt && self.interrupt_requested(&identity.id) {
                requested_interrupt = true;
                control.abort();
            }
            // Keep the large event outside the small control discriminator.
            let mut next_event = None;
            let next = tokio::select! {
                biased;
                _ = shutdown.cancelled(), if !requested_shutdown => Next::Shutdown,
                _ = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if deadline.is_some() && !requested_timeout => Next::Deadline,
                _ = &mut notified, if !requested_interrupt => Next::Changed,
                event = run.next() => {
                    next_event = event;
                    Next::Event
                },
                command = commands.recv(), if commands_open => Next::Command(command),
            };
            if extension_policy.is_some() {
                if let Some(event) = next_event.as_ref() {
                    self.observe_child_event(&identity.id, event);
                }
            }
            match next {
                Next::Changed => {}
                Next::Deadline => {
                    control.abort();
                    requested_timeout = true;
                }
                Next::Shutdown => {
                    control.abort();
                    requested_shutdown = true;
                }
                Next::Command(None) => {
                    commands_open = false;
                    control.abort();
                    requested_shutdown = true;
                }
                Next::Command(Some(command)) => match command.kind {
                    WorkerCommandKind::Message(message) => {
                        if defer_messages
                            || requested_interrupt
                            || requested_shutdown
                            || control.try_steer(format_direct_message(&message)).is_err()
                        {
                            // Once one message cannot enter the active run, keep
                            // later messages behind it to preserve FIFO order.
                            defer_messages = true;
                            deferred_messages.push_back(message);
                        } else {
                            submitted_messages.push_back(message);
                        }
                    }
                    WorkerCommandKind::FollowUp => {
                        for task in self.restored_tasks(&identity.id) {
                            let QueuedTask::FollowUp(follow_up) = task else {
                                continue;
                            };
                            if !queued_delivery_ids.insert(follow_up.delivery_id.clone()) {
                                continue;
                            }
                            if defer_follow_ups
                                || requested_interrupt
                                || requested_shutdown
                                || control
                                    .try_follow_up(format_follow_up(&follow_up, &[]))
                                    .is_err()
                            {
                                // Preserve FIFO across the active run-control queue
                                // and the worker's deferred queue.
                                defer_follow_ups = true;
                                deferred_follow_ups.push_back(follow_up);
                            } else {
                                submitted_follow_ups.push_back(follow_up);
                            }
                        }
                    }
                    WorkerCommandKind::Shutdown => {
                        requested_shutdown = true;
                        control.abort();
                    }
                },
                Next::Event => match next_event {
                    None => {
                        break if requested_shutdown {
                            WorkerOutcome::Shutdown
                        } else if requested_timeout {
                            WorkerOutcome::TimedOut
                        } else if requested_token_limit {
                            WorkerOutcome::Failed("maximum delegated token budget reached".into())
                        } else if requested_interrupt {
                            WorkerOutcome::Interrupted
                        } else {
                            WorkerOutcome::Failed(
                                "delegated run ended without a terminal event".into(),
                            )
                        };
                    }
                    Some(AgentEvent::SteeringDelivered { messages }) => {
                        for _ in 0..messages.len() {
                            let Some(message) = submitted_messages.pop_front() else {
                                debug_assert!(
                                    false,
                                    "steering acknowledgement exceeded submissions"
                                );
                                break;
                            };
                            if extension_policy.is_some() {
                                self.record_child_event(&identity.id, json!({
                                    "kind": "user_message", "message": format_direct_message(&message),
                                }));
                            }
                            self.release_message_reservation(&identity.id, &message);
                        }
                    }
                    Some(AgentEvent::FollowUpDelivered { messages }) => {
                        for _ in 0..messages.len() {
                            let Some(follow_up) = submitted_follow_ups.pop_front() else {
                                debug_assert!(
                                    false,
                                    "follow-up acknowledgement exceeded submissions"
                                );
                                break;
                            };
                            if extension_policy.is_some() {
                                self.record_child_event(&identity.id, json!({
                                    "kind": "user_message", "message": format_follow_up(&follow_up, &[]),
                                }));
                            }
                            self.acknowledge_follow_up_delivery(&identity.id, &follow_up);
                            acknowledged_follow_ups.add_usage(follow_up.usage());
                        }
                    }
                    Some(AgentEvent::ToolStarted { id, name, args }) => {
                        let args_summary = tool_args_summary(&args);
                        self.update_agent_tool_started(&identity.id, &id.0, name, args_summary);
                    }
                    Some(AgentEvent::ToolFinished { id, result, .. }) => {
                        let is_error = match &result {
                            Err(_) => true,
                            Ok(output) => output.is_error(),
                        };
                        self.update_agent_tool_finished(&identity.id, &id.0, is_error);
                    }
                    Some(AgentEvent::OutputDelta { text, .. }) => {
                        self.update_agent_streamed_output(
                            &identity.id,
                            text.len(),
                            &mut last_stream_update,
                        );
                    }
                    Some(AgentEvent::ProviderRetry { .. }) => {
                        self.clear_agent_streamed_output(&identity.id);
                        last_stream_update = None;
                    }
                    Some(AgentEvent::ProviderUsageUncertain) => {
                        self.mark_agent_usage_uncertain(&identity.id);
                    }
                    Some(AgentEvent::CandidateRejected {
                        usage,
                        session_cost_microdollars,
                        ..
                    }) => {
                        self.update_agent_usage(
                            &identity.id,
                            usage,
                            session_cost_microdollars,
                            false,
                        );
                        last_stream_update = None;
                    }
                    Some(AgentEvent::TurnFinished {
                        message,
                        usage,
                        session_cost_microdollars,
                        ..
                    }) => {
                        self.update_agent_usage(
                            &identity.id,
                            usage,
                            session_cost_microdollars,
                            true,
                        );
                        last_stream_update = None;
                        turns_completed = turns_completed.saturating_add(1);
                        if extension_policy.is_some_and(|policy| {
                            policy
                                .max_tokens
                                .is_some_and(|limit| delegation_usage_tokens(&usage) > limit)
                        }) {
                            control.abort();
                            requested_token_limit = true;
                        }
                        for part in message.content {
                            if let AssistantPart::Text(text) = part {
                                if !output.is_empty() {
                                    output.push('\n');
                                }
                                output.push_str(&text);
                                if output.len() > output_limit {
                                    output = bounded_text_to(&output, output_limit);
                                }
                            }
                        }
                    }
                    Some(AgentEvent::RunFinished { reason, .. }) => {
                        break if requested_shutdown {
                            WorkerOutcome::Shutdown
                        } else if requested_timeout {
                            WorkerOutcome::TimedOut
                        } else if requested_token_limit {
                            WorkerOutcome::Failed("maximum delegated token budget reached".into())
                        } else if requested_interrupt {
                            WorkerOutcome::Interrupted
                        } else {
                            match reason {
                                FinishReason::Completed => WorkerOutcome::Completed(output),
                                FinishReason::Aborted => WorkerOutcome::Interrupted,
                                FinishReason::Failed(error) => {
                                    WorkerOutcome::Failed(error.to_string())
                                }
                                FinishReason::MaxTurns => WorkerOutcome::LimitReached {
                                    output,
                                    turn_count: turns_completed,
                                    turn_limit: turn_limit.expect(
                                        "MaxTurns requires a configured delegated turn limit",
                                    ),
                                },
                            }
                        };
                    }
                    Some(_) => {}
                },
            }
        };

        submitted_messages.extend(deferred_messages);
        for message in submitted_messages {
            self.queue_reserved_message(&identity.id, message);
        }
        // Any unacknowledged control submission is older than work deferred
        // after the control queue filled, so prepend it to retain acceptance
        // order for the next child run.
        submitted_follow_ups.extend(deferred_follow_ups);
        // Row 3.5: settle the child span at its single driven outcome.
        delegation_guard.finish(matches!(
            outcome,
            WorkerOutcome::Failed(_) | WorkerOutcome::TimedOut
        ));
        WorkerExecution {
            outcome,
            deferred_follow_ups: submitted_follow_ups,
            acknowledged_follow_ups,
            task_delivered: true,
        }
    }
}
