//! The agent's turn loop: model calls, tool dispatch and settlement until the run ends.

use super::*;
use crate::cache_warmer::{CacheWarmHost, CacheWarmLimits, CacheWarmStep};

impl Agent {
    /// Reconciles unresolved calls from the latest persisted assistant turn.
    ///
    /// Only tools explicitly marked [`ReplaySafety::Safe`] execute again.
    /// Every other call receives a durable indeterminate error, preserving
    /// provider call/result pairing without silently duplicating an external
    /// mutation after a process crash.
    pub(super) async fn recover_pending_tools(
        &mut self,
        previous_run_was_dropped: bool,
    ) -> Result<(), AgentError> {
        let Some((calls, persisted)) = pending_tool_state(&self.session) else {
            return Ok(());
        };
        // Keep each call's original assistant-turn index. Filtering first
        // would renumber unresolved calls and let crash recovery execute calls
        // that the live path would have skipped after the per-turn limit.
        let unresolved: Vec<(usize, ToolCall)> = calls
            .into_iter()
            .enumerate()
            .filter(|(_, call)| !persisted.contains(&call.id))
            .collect();
        if unresolved.is_empty() {
            return Ok(());
        }

        if previous_run_was_dropped {
            persist_pending_cancellations(&mut self.session)?;
            return Ok(());
        }

        let replay_safe_calls = pending_tool_replay_safety(&self.session).cloned();
        let (tool_generation, tools) = self.extensions.tool_snapshot();
        let mut tool_map: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        for tool in &tools {
            let definition = tool.definition();
            tool_map.insert(definition.name, Arc::clone(tool));
        }
        let mut registered_tools = tool_map.keys().cloned().collect::<Vec<_>>();
        registered_tools.sort();
        let sandbox = self.sandbox.clone();
        let tool_scope = self.tool_scope.clone();
        let resource_owner = self.resource_owner.clone();
        let recovery_run_id = format!("{tool_scope}:recovery");
        let effect_broker = self.effect_broker.clone();
        let tool_call_hooks = self.extensions.tool_call_hooks.clone();
        for (call_index, call) in unresolved {
            if let Some((message, metadata)) =
                self.session.persisted_invocation_result(call_index)?
            {
                self.session
                    .append_with_metadata(EntryValue::Message(Message::User(message)), metadata)?;
                continue;
            }
            let result = if let Some(argument_error) = call.argument_error {
                // A schema-rejected call was never admitted for execution in
                // the live path; retain that fact across a restart as well.
                Err(rejected_argument_tool_error(argument_error))
            } else if call.async_execution {
                Err(ToolError::new(
                    "indeterminate background call after restart; not automatically replayed",
                ))
            } else if call_index >= MAX_TOOL_CALLS_PER_TURN {
                Err(ToolError::new(
                    "tool call skipped: per-turn tool-call limit reached",
                ))
            } else {
                let partial_output = self.session.invocation_partial_output(call_index)?;
                match tool_map.get(&call.name) {
                    None => Err(ToolError::new(format!(
                        "unknown tool: {}\n{}",
                        call.name,
                        synthesize_interruption(partial_output.as_deref()).text
                    ))),
                    Some(tool)
                        if !call.async_execution
                            && replay_safe_calls
                                .as_ref()
                                .is_none_or(|safe| safe.contains(&call_index))
                            && tool.replay_safety() == ReplaySafety::Safe =>
                    {
                        execute_recovery_call(
                            call_index,
                            Arc::clone(tool),
                            &tool_call_hooks,
                            &effect_broker,
                            tool_generation,
                            &recovery_run_id,
                            &call,
                            &sandbox,
                            &tool_scope,
                            &resource_owner,
                            &registered_tools,
                            &mut self.session,
                        )
                        .await?
                    }
                    Some(_) => Err(ToolError::new(format!(
                        "indeterminate after restart: `{}` was not replayed.\n{}",
                        call.name,
                        synthesize_interruption(partial_output.as_deref()).text
                    ))),
                }
            };
            let (message, _, _, _, details) = lower_tool_result(
                call.id,
                &result,
                &self.model,
                sandbox.max_output_bytes,
                Vec::new(),
            );
            self.session.append_with_metadata(
                EntryValue::Message(Message::User(message)),
                details.map(|tool_output| EntryMetadata {
                    tool_output: Some(tool_output),
                    ..EntryMetadata::default()
                }),
            )?;
            resolve_tool_delivery_after_persistence(&result, sandbox.max_output_bytes);
        }
        Ok(())
    }

    pub(super) async fn prompt_with_tools(
        &mut self,
        input: UserInput,
        tools_enabled: bool,
        prewarm_responses: bool,
    ) -> Result<Run<'_>, AgentError> {
        if self.reasoning == octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra)
            && self.delegation.is_none()
            && !self.ultra_observation_managed
        {
            return Err(AgentError::Delegation(
                "Ultra requires an enabled child-session observation runtime".into(),
            ));
        }
        if self.session.has_unsettled_native_steering() {
            return Err(AiError::Config(octet_ai::ConfigError::Parse("unresolved native steering intent; automatic replay is prohibited; use a new session".into())).into());
        }
        // Direct library callers may not have an explicit construction
        // boundary. Keep this idempotent fallback so their first owning run
        // cannot leave dynamic publishers waiting forever.
        self.extensions.finalize_tool_surface();
        // A previous process may have died after persisting an assistant tool
        // call but before persisting its result. Repair that semantic boundary
        // before appending a new user message; otherwise strict provider
        // validation would reject the resumed conversation as malformed.
        let previous_run_was_dropped = self
            .last_run_lifecycle
            .take()
            .is_some_and(|lifecycle| lifecycle.dropped.load(Ordering::Acquire));
        self.recover_pending_tools(previous_run_was_dropped).await?;
        // This snapshot is both the preflight boundary and the first provider
        // request's frozen tool surface. Refusing it before the prompt append
        // leaves a frontend free to revise and retry the same draft.
        let (initial_tool_revision, initial_tools) =
            self.extensions.model_tool_snapshot(&self.resource_owner);
        let initial_tool_defs: Vec<ToolDef> = if tools_enabled {
            advertised_tool_surface(&initial_tools, &self.model)
        } else {
            Vec::new()
        };
        require_tool_schema_budget(&initial_tool_defs, self.tool_schema_budget_bytes)?;
        // Capture only settled context, before appending this submission.
        // Opening is deferred until the first request passes run admission;
        // merely constructing a Run (or an idle RPC host) performs no I/O.
        let mut responses_prewarm = if prewarm_responses {
            self.responses_prewarm_request().ok().flatten()
        } else {
            None
        };
        let completion_policy = self.completion_policy;
        let mut terminal_gate_evidence =
            TerminalGateEvidence::for_run(completion_policy, &self.session, &input)?;
        let input = prepare_user_images(input, &self.model, None).await?;
        let prompt_metadata = self.prompt_entry_metadata();
        // `display_text` belongs only to the draft that started this run.
        // Steering and follow-up inputs are independent user submissions and
        // must render their own durable message bodies after replay.
        let control_prompt_metadata = EntryMetadata {
            display_text: None,
            ..prompt_metadata.clone()
        };
        let observer_input = (!self.extensions.observers.is_empty()).then(|| input.clone());
        if self.model.responses_features().reasoning_effort_updates {
            persist_reasoning_selection(&mut self.session, &self.model, &self.reasoning)?;
        }
        let custom_message_cursor = self.session.entries().len();
        let first_entry = input.append_to(&mut self.session, Some(prompt_metadata.clone()))?;
        if let Some(input) = observer_input.as_ref() {
            for observer in &self.extensions.observers {
                observer.on_run_started_for_owner(
                    &first_entry.0,
                    input,
                    &self.model,
                    &self.resource_owner,
                );
            }
        }
        let lifecycle = Arc::new(RunLifecycle {
            finished: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
        });
        self.last_run_lifecycle = Some(lifecycle.clone());
        let context = Arc::new(ContextTracker::default());
        let stream_context = context.clone();

        let (control_tx, mut control_rx) = mpsc::channel::<Control>(8);
        let abort = Arc::new(AbortFlag::default());
        let control_admission = Arc::new(std::sync::Mutex::new(true));
        let control = RunControl {
            cache_warming_mode: self.cache_warmer.mode_control(),
            cache_warming_status: self.cache_warmer.diagnostics(),
            reasoning_model: self
                .model
                .responses_features()
                .reasoning_effort_updates
                .then(|| self.model.clone()),
            ultra_observed: self.delegation.is_some() || self.ultra_observation_managed,
            admission: control_admission.clone(),
            tx: control_tx,
            pending_count: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_CONTROL_INPUTS)),
            pending_bytes: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_CONTROL_BYTES)),
            abort: abort.clone(),
        };

        // Disjoint borrows: the run stream owns clones of everything except
        // the session, which it borrows mutably for the run's lifetime —
        // preserving one authoritative head.
        let client = provider_context::provider_request_client(
            &self.client,
            &self.extensions.provider_request_hooks,
            &self.resource_owner,
        )?;
        let model = self.model.clone();
        let compaction_model = self
            .compaction_model
            .clone()
            .unwrap_or_else(|| model.clone());
        let system = self.model_visible_system(tools_enabled);
        let sandbox = self.sandbox.clone();
        let extension_host = self.extensions.clone();
        let initial_context =
            observe_context_tracker(&context, &self.session, &model, &system, &initial_tool_defs)?;
        let initial_capacity =
            ContextCapacityCache::seeded(&self.session, initial_tool_revision, &initial_context);
        if let Some(delegation) = &self.delegation {
            delegation.prepare_owning_run()?;
        }
        let observers = ObserverDispatch {
            observers: self.extensions.observers.clone(),
            resource_owner: self.resource_owner.clone(),
        };
        let tool_call_hooks = self.extensions.tool_call_hooks.clone();
        let provider_retry_hooks = self.extensions.provider_retry_hooks.clone();
        let persistence_metadata_hooks = self.extensions.persistence_metadata_hooks.clone();
        let max_turns = self.max_turns;
        let mut reasoning = self.reasoning.clone();
        let reasoning_mode = self.reasoning_mode;
        let cache_retention = self.cache_retention;
        let session_id = self.session_id.clone();
        let resource_owner = self.resource_owner.clone();
        let tool_scope = self.tool_scope.clone();
        let effect_broker = self.effect_broker.clone();
        let effect_run_id = format!("run:{}", first_entry.0);
        let output_modalities = self.output_modalities.clone();
        let provider_output_ceiling = self.max_output_tokens;
        let compaction_reserve_tokens = self.compaction_reserve_tokens();
        let effective_reasoning = &mut self.reasoning;
        let parallel_read_wave_width = self.parallel_read_wave_width;
        let max_session_tokens = self.max_session_tokens;
        let max_session_cost_microdollars = self.max_session_cost_microdollars;
        let tool_schema_budget_bytes = self.tool_schema_budget_bytes;
        let auto_compaction_mode = if self.model.responses_features().reasoning_effort_updates
            && self.auto_compaction_mode == AgentCompactionMode::NativeResponses
        {
            AgentCompactionMode::Local
        } else {
            self.auto_compaction_mode
        };
        // The caller-selected provider service tier rides on every Responses
        // request this run builds; the builder re-checks the route capability.
        let service_tier = self.service_tier;
        let compaction_threshold_fraction = self.compaction_threshold_fraction;
        let compaction_keep_recent_tokens = self.compaction_keep_recent_tokens;
        let provider_retries_enabled = self.provider_retries_enabled;
        let max_network_wait = self.max_network_wait;
        let owner_tool_images_enabled = self.owner_tool_images_enabled;
        let stream_delegation = self.delegation.clone();
        // Row 4.7: the host's opt-in, captured before the session borrow, so the
        // run can publish live partial-output checkpoints without touching the
        // session log.
        #[cfg(any(unix, windows))]
        let partial_output_checkpoints = self.partial_output_checkpoints.clone();
        let run_delegation = self.delegation.clone();
        let mut delegation_telemetry = self
            .delegation
            .as_ref()
            .and_then(DelegationBinding::telemetry_receiver);
        let stream_lifecycle = lifecycle.clone();
        let telemetry = self.telemetry.clone();
        let cache_warmer = &mut self.cache_warmer;
        let session = &mut self.session;

        let stream = async_stream::stream! {
            // This guard owns the mutable session borrow for exactly as long as
            // the generated stream. If the caller drops the stream at any
            // suspension point, its Drop implementation durably pairs pending
            // tool calls before `Run::drop` returns.
            let session_guard = RunSessionGuard {
                session,
                cache_warmer,
                lifecycle: stream_lifecycle.clone(),
            };
            let session = &mut *session_guard.session;
            let cache_warmer = &mut *session_guard.cache_warmer;
            let mut context_capacity = initial_capacity;
            let mut custom_message_cursor = custom_message_cursor;
            for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                notify_observers(&observers, &event); yield event;
            }
            // Parity 1e.2 durability half: republish the partial assistant
            // turn a killed stream left behind. The frame journal is consumed
            // exactly once and only its user-visible text/reasoning progress is
            // re-emitted; a partial tool call is never a result. A journal
            // removed at terminal settlement yields nothing here, so a
            // completed turn is never replayed as progress.
            match session.take_partial_assistant() {
                Ok(Some(partial)) => {
                    for part in partial.content {
                        let (channel, text) = match part {
                            AssistantPart::Text(text) => (OutputChannel::Text, text),
                            AssistantPart::Reasoning(reasoning) => (
                                OutputChannel::Reasoning,
                                reasoning.text.unwrap_or_default(),
                            ),
                            AssistantPart::ToolCall(_)
                            | AssistantPart::Media(_)
                            | AssistantPart::ProviderMetadata(_) => continue,
                        };
                        if text.is_empty() {
                            continue;
                        }
                        let ev = AgentEvent::RecoveredOutput { channel, text };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }
                }
                Ok(None) => {}
                // A recovery aid must never fail the run it observes.
                Err(_) => {}
            }
            // Row 3.5: one run span owns the generated stream's lifetime. Its
            // children (turns) are derived only from this explicit context, and
            // the guard is settled explicitly at the durable run boundary.
            let run_guard = telemetry.begin_typed::<RunSpan>(EmptyAttributes {});
            let run_context = run_guard.context();

            let mut tool_revision = initial_tool_revision;
            let mut composition_tools = initial_tools;
            let tools = crate::tool_composition::direct_surface(&composition_tools);
            let mut tool_defs = initial_tool_defs;
            let mut tool_map: HashMap<String, Arc<dyn Tool>> =
                HashMap::with_capacity(if tools_enabled { tools.len() } else { 0 });
            if tools_enabled {
                for tool in &tools {
                    let definition = tool.definition();
                    tool_map.insert(definition.name, Arc::clone(tool));
                }
            }
            let mut registered_tools = tool_map.keys().cloned().collect::<Vec<_>>();
            registered_tools.sort();
            // Tool names already visible to the provider either as static
            // schemas or via an earlier `added_tool_names` announcement.
            let mut announced_tools: std::collections::HashSet<String> =
                registered_tools.iter().cloned().collect();

            let mut native = native_steering::NativeState::default();
            let (native_updates_tx, mut native_updates_rx) = mpsc::channel(128);
            let native_enabled = model.responses_features().steering
                && extension_host.provider_request_hooks.is_empty()
                && extension_host.provider_context_hooks.is_empty()
                && model.endpoint.transport == octet_ai::EndpointTransport::WebSocketPreferred
                && max_session_tokens.is_none() && max_session_cost_microdollars.is_none();
            let mut background_tools = background_tools::BackgroundTools::new(parallel_read_wave_width);
            let background_cancellation = abort.cancellation.clone();
            let mut pending_reasoning = None;
            let mut pending_steer: Vec<ReservedInput> = Vec::new();
            let mut pending_context: Vec<ReservedInput> = Vec::new();
            let mut followups: VecDeque<ReservedInput> = VecDeque::new();
            // Preserve octet's historical defaults; frontends that expose queue
            // modes can update either mode through RunControl.
            let mut steering_mode = QueueDeliveryMode::All;
            let mut follow_up_mode = QueueDeliveryMode::OneAtATime;
            let mut control_open = true;
            let mut answer_only = !tools_enabled;
            let mut finish_pending = false;
            let mut completed_turns: u64 = 0;
            let mut model_turn_hooks = model_turn::ModelTurnHooks::new(
                &extension_host.session_operation_hooks, &effect_run_id,
            );
            let mut context_retries = 0usize;
            // Shared by open/body retries, re-preparation and transport fallback.
            // Reset only on a complete successful assistant response.
            let mut stream_retries = 0usize;
            let mut recovery_budget = ProviderRecoveryBudget::default();
            let mut network_retries = 0usize;
            let mut network_deadline = None;
            let mut failed_usage_unknown = session.has_uncertain_usage();
            if failed_usage_unknown {
                let event = AgentEvent::ProviderUsageUncertain;
                notify_observers(&observers, &event);
                yield event;
            }
            let mut pending_recovery: Option<PendingProviderRecovery> = None;
            let mut run_usage = Usage::default();
            let mut run_cost = CostAccumulator::default();
            let mut recent_tool_calls: VecDeque<(String, String)> =
                VecDeque::with_capacity(MAX_RECENT_TOOL_CALLS);

            // Row 3.5 boundary state: the live turn guard and its derived child
            // context, plus the per-attempt outcome that decides whether the
            // previous turn settled as a completed or a failed attempt.
            let mut previous_turn: Option<SpanGuard> = None;
            let mut turn_attempt_opened = false;
            let mut turn_attempt_succeeded = false;

            let mut reason: FinishReason = 'run: loop {
                for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                    notify_observers(&observers, &event); yield event;
                }
                // Row 3.5: an iteration is one turn boundary. The previous
                // turn is settled here (a `continue 'run` continuation is a
                // completed turn, not an error) and the current one begins.
                // A turn that opened a provider attempt without a finished
                // response is reported as an error attempt.
                if let Some(settled) = previous_turn.take() {
                    settled.finish(turn_attempt_opened && !turn_attempt_succeeded);
                }
                turn_attempt_opened = false;
                turn_attempt_succeeded = false;
                let turn_guard = run_context.begin_typed::<TurnSpan>(EmptyAttributes {});
                let turn_context = turn_guard.context();
                previous_turn = Some(turn_guard);
                if let Some(recovery) = pending_recovery.take() {
                    if recovery.usage_unknown() {
                        let first = !session.has_uncertain_usage();
                        if let Err(error) = session.record_usage_uncertainty_with_bound(model.endpoint.id.clone(), model.spec.id.clone(), "assistant_turn", recovery.exposure) {
                            if first {
                                let event = AgentEvent::ProviderUsageUncertain;
                                notify_observers(&observers, &event);
                                yield event;
                            }
                            break 'run FinishReason::Failed(error.into());
                        }
                        if first {
                            let event = AgentEvent::ProviderUsageUncertain;
                            notify_observers(&observers, &event);
                            yield event;
                        }
                    }
                    failed_usage_unknown |= recovery.usage_unknown();
                    let waiting_for_network = recovery.waiting_for_network();
                    if waiting_for_network && network_deadline.is_none() {
                        network_deadline = max_network_wait.and_then(|limit| tokio::time::Instant::now().checked_add(limit));
                    }
                    let retry_limit = recovery_budget.limit(stream_retries, &recovery);
                    let hard_budget = max_session_tokens.is_some()
                        || max_session_cost_microdollars.is_some();
                    let bounded = !uncertainty_blocks_ceiling(session, max_session_tokens, max_session_cost_microdollars);
                    // A hard ceiling admits a replacement only on bounded exposure.
                    let eligible = provider_retries_enabled
                        && (waiting_for_network || stream_retries < retry_limit)
                        && (bounded || !hard_budget);
                    let host_delay = if waiting_for_network {
                        network_wait_delay(&effect_run_id, network_retries)
                    } else {
                        retry_after(&recovery.error, stream_retries)
                    };
                    let decision = if eligible {
                        let decision_future = provider_retry_decision(ProviderRetryRequest {
                            hooks: &provider_retry_hooks,
                            context: ProviderRetryContext {
                                operation: None,
                                run_id: effect_run_id.clone(),
                                resource_owner: resource_owner.clone(),
                                attempt: if waiting_for_network { network_retries } else { stream_retries }.saturating_add(1),
                                max_attempts: (!waiting_for_network).then_some(retry_limit),
                                host_delay,
                                kind: if waiting_for_network {
                                    ProviderRetryKind::WaitingForNetwork
                                } else if recovery.qualified && interrupted_inference_error(&recovery.error) {
                                    ProviderRetryKind::InterruptedInference
                                } else if recovery.opened {
                                    ProviderRetryKind::StreamStart
                                } else {
                                    ProviderRetryKind::BeforeGeneration
                                },
                            },
                            abort: &abort,
                        });
                        tokio::select! {
                            biased;
                            _ = abort.wait() => break 'run FinishReason::Aborted,
                            _ = wait_network_deadline(network_deadline) => break 'run FinishReason::Failed(AgentError::NetworkWaitLimit { limit: max_network_wait.unwrap(), usage_unknown: failed_usage_unknown }),
                            decision = decision_future => decision,
                        }
                    } else {
                        ProviderRetryDecision { proceed: false, additional_delay: Duration::ZERO }
                    };
                    if abort.is_set() {
                        break 'run FinishReason::Aborted;
                    }
                    if !decision.proceed {
                        let error = if (hard_budget && !bounded)
                            || (stream_retries > 0 && !is_replayable_network_failure(&recovery.error)) {
                            AgentError::ProviderRecovery {
                                retries: stream_retries,
                                usage_unknown: failed_usage_unknown,
                                source: recovery.error,
                            }
                        } else {
                            provider_failure(recovery.error, stream_retries)
                        };
                        break 'run FinishReason::Failed(error);
                    }
                    if waiting_for_network {
                        network_retries = network_retries.saturating_add(1);
                    } else {
                        recovery_budget.admit(&recovery);
                        stream_retries += 1;
                    }
                    let delay = host_delay.saturating_add(decision.additional_delay);
                    let mut diagnostic = provider_retry_diagnostic(&model, &recovery.error);
                    if failed_usage_unknown {
                        diagnostic = format!("failed_usage=unknown {diagnostic}");
                        truncate_public_diagnostic(&mut diagnostic);
                    }
                    stream_context.provider_retry();
                    let ev = if waiting_for_network {
                        AgentEvent::ProviderWaitingForNetwork {
                            attempt: network_retries, delay, error: diagnostic,
                        }
                    } else {
                        AgentEvent::ProviderRetry {
                            attempt: stream_retries, max_attempts: retry_limit,
                            delay, error: diagnostic,
                        }
                    };
                    notify_observers(&observers, &ev);
                    yield ev;
                    let wait = tokio::time::sleep(delay);
                    tokio::pin!(wait);
                    loop {
                        tokio::select! {
                            biased;
                            _ = abort.wait() => break 'run FinishReason::Aborted,
                            _ = wait_network_deadline(network_deadline) => break 'run FinishReason::Failed(AgentError::NetworkWaitLimit { limit: max_network_wait.unwrap(), usage_unknown: failed_usage_unknown }),
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                    context_capacity.invalidate();
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => break 'run FinishReason::Aborted,
                                None => control_open = false,
                            },
                            _ = &mut wait => break,
                        }
                    }
                    // Resume the ordinary safe preparation boundary, not a
                    // stale clone: steering, FinishNow and tool snapshots agree.
                }

                // ── Drain control at the turn boundary ─────────────────────
                while control_open {
                    match control_rx.try_recv() {
                        Ok(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                        Ok(Control::FollowUp(input)) => followups.push_back(input),
                        Ok(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                        Ok(Control::FinishNow(input)) => {
                            input.push_pending(&mut pending_steer);
                            answer_only = true;
                            finish_pending = true;
                            context_capacity.invalidate();
                        }
                        Ok(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                        Ok(Control::SetSteeringMode(mode)) => steering_mode = mode,
                        Ok(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                        Ok(Control::Abort) => break 'run FinishReason::Aborted,
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => control_open = false,
                    }
                }
                if abort.is_set() {
                    break 'run FinishReason::Aborted;
                }

                // A finished observation belongs in the next request, not one
                // response later. Give spawned immediate reads a scheduling
                // opportunity; never await an unfinished job at this boundary.
                if !background_tools.is_empty() {
                    tokio::task::yield_now().await;
                }
                while background_tools.front_ready() {
                    if abort.is_set() { break 'run FinishReason::Aborted; }
                    match background_tools.settle_one(session, &model, &sandbox,
                        &stream_context, &mut run_usage, &mut terminal_gate_evidence).await {
                        Ok(events) => for event in events { notify_observers(&observers, &event); yield event; },
                        Err(error) => break 'run FinishReason::Failed(error),
                    }
                    context_capacity.invalidate();
                }
                if abort.is_set() { break 'run FinishReason::Aborted; }
                let observation = model_turn_hooks.settle(session, &abort.cancellation).await;
                for event in model_turn_hooks.take_warnings() {
                    notify_observers(&observers, &event); yield event;
                }
                if let Err(finish) = observation {
                    break 'run finish;
                }

                if background_tools.is_empty() && !native.has_pending() {
                    if let Err(error) = append_context_inputs(&mut pending_context, session, &model).await {
                        break 'run FinishReason::Failed(error);
                    }
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                    context_capacity.invalidate();
                }

                if let Some(selection) = if native.connection.is_none() { pending_reasoning.take() } else { None } {
                    if let Err(error) = persist_reasoning_selection(session, &model, &selection) {
                        break 'run FinishReason::Failed(error);
                    }
                    reasoning = selection.clone();
                    if let Some(binding) = &stream_delegation {
                        binding.update_reasoning(selection.clone());
                    }
                    *effective_reasoning = selection;
                    context_capacity.invalidate();
                }

                // ── Steering enters here, at the model-turn boundary ───────
                pending_steer.retain(ReservedInput::is_pending);
                if native.connection.is_none() && (!pending_steer.is_empty() || pending_reasoning.is_some()) {
                    let queued = if std::mem::take(&mut finish_pending) {
                        std::mem::take(&mut pending_steer)
                    } else {
                        match steering_mode {
                            QueueDeliveryMode::All => std::mem::take(&mut pending_steer),
                            QueueDeliveryMode::OneAtATime => vec![pending_steer.remove(0)],
                        }
                    };
                    let visible_tools = if answer_only {
                        &[][..]
                    } else {
                        tool_defs.as_slice()
                    };
                    let observation = ContextObservation {
                        tracker: &stream_context,
                        model: &model,
                        system: &system,
                        tools: visible_tools,
                    };
                    match deliver_control_inputs(
                        queued,
                        ControlDeliveryKind::Steering,
                        session,
                        &control_prompt_metadata,
                        &mut terminal_gate_evidence,
                        &observation,
                        Some(&abort),
                    ).await {
                        ControlDelivery::Completed { event } => {
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                            if let Some(ev) = event {
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                        }
                        ControlDelivery::Interrupted { event, finish } => {
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                            if let Some(ev) = event {
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            break 'run finish;
                        }
                    }
                }

                // ── Turn guard ─────────────────────────────────────────────
                if let Some(limit) = max_turns {
                    if completed_turns >= limit {
                        break 'run FinishReason::MaxTurns;
                    }
                }

                // Await the real logical-iteration observation before request
                // preparation. Recovery/compaction re-entry must not repeat it.
                let observation = model_turn_hooks.start(session, completed_turns, &abort.cancellation).await;
                for event in model_turn_hooks.take_warnings() {
                    notify_observers(&observers, &event); yield event;
                }
                if let Err(finish) = observation {
                    break 'run finish;
                }

                // Freeze one coherent schema/implementation snapshot after
                // control and steering have settled but before context sizing.
                // Every call emitted by this request resolves against exactly
                // the tool set the provider saw.
                let (current_revision, current_tools) = extension_host.model_tool_snapshot(&resource_owner);
                if current_revision != tool_revision {
                    tool_revision = current_revision;
                    if tools_enabled && !answer_only {
                        let next_tool_defs = advertised_tool_surface(&current_tools, &model);
                        if let Err(error) = require_tool_schema_budget(
                            &next_tool_defs,
                            tool_schema_budget_bytes,
                        ) {
                            break 'run FinishReason::Failed(error);
                        }
                        tool_defs = next_tool_defs;
                        composition_tools = current_tools;
                        let current_tools = crate::tool_composition::direct_surface(&composition_tools);
                        tool_map.clear();
                        tool_map.reserve(current_tools.len());
                        for tool in &current_tools {
                            let definition = tool.definition();
                            tool_map.insert(definition.name, Arc::clone(tool));
                        }
                        registered_tools = tool_map.keys().cloned().collect();
                        registered_tools.sort();
                        announced_tools.extend(registered_tools.iter().cloned());
                    }
                }
                let request_tool_defs = if answer_only {
                    Vec::new()
                } else {
                    tool_defs.clone()
                };

                // ── Reconstruct and size context for this exact turn ───────
                // This gate is inside the autonomous loop, after every tool
                // result, and uses the exact active tool schema set.
                let (compaction_event_tx, mut compaction_event_rx) =
                    mpsc::unbounded_channel::<AgentEvent>();
                let capacity = {
                    let mut compaction = CompactionContext {
                        run_id: &effect_run_id,
                        resource_owner: &resource_owner,
                        retry_hooks: &provider_retry_hooks,
                        compaction_strategy: extension_host.compaction_strategy.as_ref(),
                        session_operation_hooks: &extension_host.session_operation_hooks,
                        max_network_wait,
                        provider_retries_enabled,
                        client: &client,
                        model: &model,
                        compaction_model: &compaction_model,
                        summary_operation: crate::events::ProviderOperation::LocalCompaction,
                        session,
                        usage: &mut run_usage,
                        run_cost: &mut run_cost,
                        cache_retention,
                        reasoning: &reasoning,
                        reasoning_mode,
                        session_id: &session_id,
                        max_session_tokens,
                        max_session_cost_microdollars,
                        abort: &abort,
                        mode: if background_tools.is_empty() && native.connection.is_none() { auto_compaction_mode } else { AgentCompactionMode::Disabled },
                        threshold_fraction: compaction_threshold_fraction,
                        keep_recent_tokens: compaction_keep_recent_tokens,
                        events: &compaction_event_tx,
                        context: &stream_context,
                        tool_generation: tool_revision,
                        capacity: &mut context_capacity,
                        telemetry: turn_context.clone(),
                    };
                    let preparation = ProviderContextPreparation {
                        hooks: &extension_host.provider_context_hooks,
                        tool_choice: if answer_only { ToolChoice::None } else { ToolChoice::Auto },
                        output_modalities: output_modalities.clone(),
                        service_tier,
                        replay_mode: auto_compaction_mode,
                    };
                    let operation = compaction.ensure_capacity(
                        &system,
                        &request_tool_defs,
                        compaction_reserve_tokens,
                        provider_output_ceiling,
                        (!preparation.hooks.is_empty()).then_some(&preparation),
                    );
                    tokio::pin!(operation);
                    let result = loop {
                        tokio::select! {
                            biased;
                            _ = abort.wait() => break Err(AgentError::Cancelled),
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => { abort.set(); }
                                None => control_open = false,
                            },
                            Some(event) = compaction_event_rx.recv() => {
                                notify_observers(&observers, &event);
                                yield event;
                            }
                            result = &mut operation => break result,
                        }
                    };
                    while let Ok(event) = compaction_event_rx.try_recv() {
                        notify_observers(&observers, &event);
                        yield event;
                    }
                    result
                };
                let capacity = match capacity {
                    Ok(capacity) => capacity,
                    Err(error) => {
                        break 'run if matches!(&error, AgentError::Cancelled) {
                            FinishReason::Aborted
                        } else {
                            FinishReason::Failed(error)
                        };
                    }
                };
                pending_steer.retain(ReservedInput::is_pending);
                if native.connection.is_none() && (!pending_steer.is_empty() || pending_reasoning.is_some()) {
                    context_capacity.invalidate();
                    continue 'run;
                }
                let input_tokens = capacity.input_tokens;
                let request_max_output_tokens = capacity.max_output_tokens;
                let messages = match session.context() {
                    Ok(m) => m,
                    Err(e) => break 'run FinishReason::Failed(e.into()),
                };
                let active_system = capacity.active_system;
                let responses =
                    if auto_compaction_mode == AgentCompactionMode::NativeResponses {
                        match native_responses_options(
                            session,
                            &model,
                            &active_system,
                            service_tier,
                        ) {
                            Ok(options) => Some(options),
                            Err(error) => break 'run FinishReason::Failed(error),
                        }
                    } else {
                        match durable_responses_options(
                            session,
                            &model,
                            &active_system,
                            service_tier,
                        ) {
                            Ok(options) => options,
                            Err(error) => break 'run FinishReason::Failed(error),
                        }
                    };

                let request = match capacity.effective_request {
                    Some(request) => request,
                    None => Request {
                    system: if active_system.is_empty() { None } else { Some(active_system.clone()) },
                    messages,
                    tools: request_tool_defs.clone(),
                    tool_choice: if answer_only {
                        ToolChoice::None
                    } else {
                        ToolChoice::Auto
                    },
                    max_output_tokens: Some(request_max_output_tokens),
                    temperature: None,
                    stop: vec![],
                    reasoning: match request_reasoning_for_replay(
                        session,
                        &model,
                        responses.as_ref(),
                        &reasoning,
                    ) {
                        Ok(selection) => selection,
                        Err(error) => break 'run FinishReason::Failed(error),
                    },
                    reasoning_mode,
                    responses,
                    output_format: OutputFormat::Text,
                    output_modalities: output_modalities.clone(),
                    compatibility: CompatibilityMode::Strict,
                    cache_retention,
                    session_id: Some(session_id.clone()),
                    },
                };
                let prepared = PreparedTurn::new(
                    session.head(),
                    active_system.clone(),
                    tool_revision,
                    request,
                    input_tokens,
                );
                let current_tool_generation = extension_host.model_tool_snapshot(&resource_owner).0;
                if !prepared.is_current(session, &active_system, current_tool_generation) {
                    // Re-enter the boundary so a publication or append that
                    // crossed compaction cannot pair an old request with a new
                    // tool map or durable cursor.
                    continue 'run;
                }
                let input_tokens = prepared.input_tokens;
                let mut request = prepared.request;

                // Settle an older warm's exposure before the new main request
                // reserves its budget. Its new timer starts at opening below,
                // not during preparation or a TurnStarted yield suspension.
                let warm_usage_uncertainty_count = session.usage_uncertainty_records().len();
                if let Err(error) = cache_warmer.cancel(session, "new provider request") {
                    break 'run FinishReason::Failed(error.into());
                }
                if session.usage_uncertainty_records().len() > warm_usage_uncertainty_count {
                    let event = AgentEvent::ProviderUsageUncertain;
                    notify_observers(&observers, &event);
                    yield event;
                }

                let reserved_output_tokens = match reservation_output_tokens(
                    session, &model, request_max_output_tokens, max_session_tokens, max_session_cost_microdollars,
                ) {
                    Ok(tokens) => tokens,
                    Err(error) => break 'run FinishReason::Failed(error),
                };
                if let Err(error) = reserve_request_tokens(
                    session,
                    input_tokens,
                    reserved_output_tokens,
                    max_session_tokens,
                ) {
                    break 'run FinishReason::Failed(error);
                }
                if let Err(error) = reserve_request_cost_with_tier(
                    session,
                    &model,
                    input_tokens,
                    reserved_output_tokens,
                    max_session_cost_microdollars,
                    request.responses.as_ref().and_then(|options| options.service_tier),
                    request.cache_retention,
                ) {
                    break 'run FinishReason::Failed(error);
                }

                let attempt_bound = request_uncertainty_bound(
                    &model,
                    None,
                    request_max_output_tokens,
                    request.responses.as_ref().and_then(|options| options.service_tier),
                    request.cache_retention,
                );

                // ── Open the provider stream (abortable) ───────────────────
                // Row 3.5: one logical provider request. It is opened here so
                // that turn iterations that never reach the provider (steering
                // or a stale prepared turn) do not fabricate a request span.
                let request_guard = turn_context.begin_typed::<ProviderRequestSpan>(
                    RequestAttributes {
                        operation: SpanOperation::Assistant,
                    },
                );
                turn_attempt_opened = true;
                // A new provider request for this model turn starts here.
                // Anchor first-token-latency measurement for consumers that
                // track it per attempt: the first OutputDelta of this stream
                // measured from this event is the attempt's TTFT.
                let ev = AgentEvent::TurnStarted;
                notify_observers(&observers, &ev);
                yield ev;
                let qualified = !native_enabled && qualified_inference_replacement(&model, &request);
                // A continuation needs the same settings, but not a clone of
                // the full history: required_input_request replaces messages
                // with only the new tool results. Ordinary streams need no
                // continuation request at all.
                let native_delta = if native_enabled {
                    let messages = std::mem::take(&mut request.messages);
                    let delta = native_steering::required_input_request(request.clone(), session, &model);
                    request.messages = messages;
                    match delta {
                        Ok(delta) => Some(delta),
                        Err(error) => break 'run FinishReason::Failed(error),
                    }
                } else {
                    None
                };
                let native_delta_has_results = native_delta.as_ref().is_some_and(|delta| delta.messages.iter().any(|message| matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(_))))));
                if abort.is_set() { break 'run FinishReason::Aborted; }
                // Snapshot the actual request at REQUEST OPEN. The warmer alone
                // decides eligibility before cloning it or allocating a timer.
                if let Err(error) = cache_warmer.start(
                    &model, &request, session.head(), input_tokens, tool_revision, session,
                ) {
                    break 'run FinishReason::Failed(error.into());
                }
                let opening_client = client.track_request_dispatch();
                let opened = {
                    // Pin once: a warm/control wake must never drop an opening
                    // future and replay an already accepted main POST.
                    let opening = async {
                        if let Some(connection) = native.connection.take() {
                            Ok(Some(native_steering::ProviderStream::Native(connection, native_updates_tx.clone(), None)))
                        } else if native_enabled {
                            opening_client.steerable_responses(&model, request).await.map(|connection| Some(native_steering::ProviderStream::Native(connection, native_updates_tx.clone(), None)))
                        } else if let Some((_, _, warm_request)) = responses_prewarm.take() {
                            // Erase the optional setup future: its credential /
                            // transport state must not inflate every run's stack.
                            Box::pin(opening_client.stream_with_responses_prewarm(&model, request, warm_request))
                                .await.map(|stream| Some(native_steering::ProviderStream::Ordinary(stream)))
                        } else {
                            open_provider_stream(&opening_client, &model, request, &abort).await.map(|stream| stream.map(native_steering::ProviderStream::Ordinary))
                        }
                    };
                    tokio::pin!(opening);
                    loop {
                        tokio::select! {
                            biased;
                            _ = abort.wait() => break Ok(None),
                            _ = wait_network_deadline(network_deadline) => {
                                if opening_client.request_may_have_been_sent() {
                                    let first = !session.has_uncertain_usage();
                                    let recorded = session.record_usage_uncertainty_with_bound(model.endpoint.id.clone(), model.spec.id.clone(), "assistant_turn", attempt_bound);
                                    if first {
                                        let event = AgentEvent::ProviderUsageUncertain;
                                        notify_observers(&observers, &event);
                                        yield event;
                                    }
                                    if let Err(error) = recorded {
                                        break 'run FinishReason::Failed(error.into());
                                    }
                                    failed_usage_unknown = true;
                                }
                                break 'run FinishReason::Failed(AgentError::NetworkWaitLimit { limit: max_network_wait.unwrap(), usage_unknown: failed_usage_unknown });
                            },
                            // Native steering must stay queued until the socket
                            // control exists; ordinary requests use turn-boundary
                            // queues. Abort remains independently level-triggered.
                            control = control_rx.recv(), if control_open && !native_enabled => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                    context_capacity.invalidate();
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => { abort.set(); break Ok(None); }
                                None => control_open = false,
                            },
                            result = &mut opening => {
                                break if abort.is_set() { Ok(None) } else { result };
                            },
                            step = cache_warmer.next_step() => {
                                // Opening can set abort while returning Pending
                                // in this same select poll.
                                if abort.is_set() { break Ok(None); }
                                match cache_warmer.advance(step, CacheWarmHost {
                                    session,
                                    client: &client,
                                    hooks: &extension_host.cache_warming_decision_hooks,
                                    resource_owner: &resource_owner,
                                    tool_generation: extension_host.tool_snapshot().0,
                                    limits: CacheWarmLimits {
                                        max_session_tokens,
                                        max_session_cost_microdollars,
                                        pending_request: attempt_bound,
                                    },
                                }) {
                                    Ok(Some(event)) => {
                                        notify_observers(&observers, &event);
                                        yield event;
                                    }
                                    Ok(None) => {}
                                    Err(error) => break 'run FinishReason::Failed(error.into()),
                                }
                            },
                        }
                    }
                };
                let mut response_stream = match opened {
                    Err(error) if native_enabled => {
                        if opening_client.request_may_have_been_sent() {
                            if let Err(record_error) = session.record_usage_uncertainty(model.endpoint.id.clone(), model.spec.id.clone(), "native_steering") { break 'run FinishReason::Failed(record_error.into()); }
                            let event = AgentEvent::ProviderUsageUncertain; notify_observers(&observers, &event); yield event;
                        }
                        break 'run FinishReason::Failed(error.into());
                    }
                    Err(error) if context_retries < MAX_PROVIDER_RETRIES && looks_like_context_error(&error) => {
                        context_retries += 1;
                        let compacted = {
                            let mut compaction = CompactionContext {
                        run_id: &effect_run_id,
                        resource_owner: &resource_owner,
                        retry_hooks: &provider_retry_hooks,
                        compaction_strategy: extension_host.compaction_strategy.as_ref(),
                        session_operation_hooks: &extension_host.session_operation_hooks,
                        max_network_wait,
                        provider_retries_enabled,
                        client: &client,
                                model: &model,
                                compaction_model: &compaction_model,
                                summary_operation: crate::events::ProviderOperation::LocalCompaction,
                                session,
                                usage: &mut run_usage,
                                run_cost: &mut run_cost,
                                cache_retention,
                                reasoning: &reasoning,
                                reasoning_mode,
                                session_id: &session_id,
                                max_session_tokens,
                                max_session_cost_microdollars,
                                abort: &abort,
                                mode: if background_tools.is_empty() && native.connection.is_none() { auto_compaction_mode } else { AgentCompactionMode::Disabled },
                                threshold_fraction: compaction_threshold_fraction,
                                keep_recent_tokens: compaction_keep_recent_tokens,
                                events: &compaction_event_tx,
                                context: &stream_context,
                                tool_generation: tool_revision,
                                capacity: &mut context_capacity,
                                telemetry: turn_context.clone(),
                            };
                            let operation = compaction.force_one_boundary(
                                &system,
                                &request_tool_defs,
                                compaction_reserve_tokens,
                            );
                            tokio::pin!(operation);
                            let result = loop {
                                tokio::select! {
                                    biased;
                                    _ = abort.wait() => break Err(AgentError::Cancelled),
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => { abort.set(); }
                                None => control_open = false,
                            },
                            Some(event) = compaction_event_rx.recv() => {
                                        notify_observers(&observers, &event);
                                        yield event;
                                    }
                                    result = &mut operation => break result,
                                }
                            };
                            while let Ok(event) = compaction_event_rx.try_recv() {
                                notify_observers(&observers, &event);
                                yield event;
                            }
                            result
                        };
                        if let Err(compaction_error) = compacted {
                            break 'run if matches!(&compaction_error, AgentError::Cancelled) {
                                FinishReason::Aborted
                            } else {
                                FinishReason::Failed(compaction_error)
                            };
                        }
                        continue 'run;
                    }
                    Err(error) => {
                        pending_recovery = Some(PendingProviderRecovery {
                            error, qualified, saw_generation: false, opened: false,
                            exposure: attempt_bound,
                        });
                        continue 'run;
                    }
                    Ok(None) => break 'run FinishReason::Aborted,
                    Ok(Some(s)) => {
                        // Connectivity recovered. Body deadlines belong to the
                        // provider; a later pre-send outage gets its own clock.
                        network_deadline = None;
                        s
                    },
                };
                if let Some(control) = response_stream.control() {
                    if native.control.is_none() { native.begin(format!("{effect_run_id}:{completed_turns}"), control); }
                }
                if let Some(delta) = native_delta.as_ref().filter(|_| native.required_input && native_delta_has_results) {
                    native.required_input = false;
                    if let Some(control) = native.control.as_ref() {
                        if let Err(error) = control.continue_with(delta.clone()).await { break 'run FinishReason::Failed(error.into()); }
                    }
                }
                // Parity 1e.2 durability half: encode the in-flight assistant
                // message into compact frames and journal them beside the
                // session between deltas, so a killed process can republish the
                // partial prefix. Frames never include a terminal event, and a
                // journal fault never affects the provider stream.
                let mut assistant_frame_encoder = octet_ai::AssistantMessageFrameEncoder::new(
                    model.spec.id.clone(),
                    model.spec.protocol,
                );
                let mut assistant_frame_journal = session.begin_assistant_frame_journal().ok();

                // ── Consume the stream, staying responsive to control ──────
                // Text/tool deltas dominate this hot path; keep StreamEvent
                // inline rather than allocating a box for every event.
                #[allow(clippy::large_enum_variant)]
                enum Next {
                    Event(Option<Result<StreamEvent, AiError>>),
                    Ctl(Option<Control>),
                    Delegation(Option<DelegationTelemetrySnapshot>),
                    Steering(octet_ai::SteeringUpdate),
                    Warm(CacheWarmStep),
                    Abort,
                }
                let mut attempt_saw_generation = false;
                // Row 3.5: the streaming response is its own boundary nested
                // under the request that produced it.
                let stream_guard = request_guard
                    .context()
                    .begin_typed::<ProviderStreamSpan>(EmptyAttributes {});
                let turn = loop {
                    let next = tokio::select! {
                        biased;
                        _ = abort.wait() => Next::Abort,
                        c = control_rx.recv(), if control_open => Next::Ctl(c),
                        Some(update) = native_updates_rx.recv() => Next::Steering(update),
                        snapshot = async {
                            match &mut delegation_telemetry {
                                Some(receiver) => next_delegation_snapshot(receiver).await,
                                None => std::future::pending().await,
                            }
                        }, if delegation_telemetry.is_some() => Next::Delegation(snapshot),
                        ev = response_stream.next() => Next::Event(ev),
                        step = cache_warmer.next_step() => Next::Warm(step),
                    };
                    // A provider/body poll can set abort after the biased abort
                    // branch was checked. No warm or semantic event outranks it.
                    let next = if abort.is_set() { Next::Abort } else { next };
                    // Apply the selected update before subsequently queued ones;
                    // both paths must drive required-input continuations.
                    let next = match next {
                        Next::Steering(update) => {
                            if let Err(error) = native.update(update, session, &model) { break 'run FinishReason::Failed(error); }
                            None
                        }
                        next => Some(next),
                    };
                    while let Ok(update) = native_updates_rx.try_recv() {
                        if let Err(error) = native.update(update, session, &model) { break 'run FinishReason::Failed(error); }
                    }
                    if let Some(delta) = native_delta.as_ref().filter(|_| native.required_input && native_delta_has_results) {
                        native.required_input = false;
                        if let Some(control) = native.control.as_ref() {
                            if let Err(error) = control.continue_with(delta.clone()).await { break 'run FinishReason::Failed(error.into()); }
                        }
                    }
                    match native.deliver(session, &control_prompt_metadata, &mut terminal_gate_evidence) {
                        Ok(Some(event)) => {
                        for custom in committed_custom_message_events(session, &mut custom_message_cursor) {
                            notify_observers(&observers, &custom); yield custom;
                        }
                        notify_observers(&observers, &event); yield event;
                    },
                        Ok(None) => {}, Err(error) => break 'run FinishReason::Failed(error),
                    }
                    let Some(next) = next else { continue; };
                    if matches!(next, Next::Event(Some(Ok(StreamEvent::Started { .. })))) { native.started(response_stream.response_id()); }
                    let next = match next {
                        Next::Event(Some(Ok(StreamEvent::Finished(response)))) => {
                            match incomplete_responses_error(&model, &response) {
                                Some(error) => Next::Event(Some(Err(error))),
                                None => Next::Event(Some(Ok(StreamEvent::Finished(response)))),
                            }
                        }
                        next => next,
                    };
                    match next {
                        Next::Abort | Next::Ctl(Some(Control::Abort)) => {
                            if let Some(journal) = assistant_frame_journal.as_mut() {
                                journal.settle();
                            }
                            break Err(FinishReason::Aborted);
                        }
                        Next::Ctl(Some(Control::Steer(input))) => {
                            match native.submit(input, session, &model).await {
                                Ok(Some(input)) => input.push_pending(&mut pending_steer),
                                Ok(None) => {}, Err(error) => break 'run FinishReason::Failed(error),
                            }
                        },
                        Next::Steering(_) => unreachable!("handled above"),
                        Next::Ctl(Some(Control::FollowUp(input))) => followups.push_back(input),
                        Next::Ctl(Some(Control::AppendCustom(input))) => input.push_pending(&mut pending_context),
                        Next::Ctl(Some(Control::FinishNow(input))) => {
                            input.push_pending(&mut pending_steer);
                            answer_only = true;
                            finish_pending = true;
                            context_capacity.invalidate();
                        }
                        Next::Ctl(Some(Control::SetReasoning(selection))) => pending_reasoning = Some(selection),
                        Next::Ctl(Some(Control::SetSteeringMode(mode))) => steering_mode = mode,
                        Next::Ctl(Some(Control::SetFollowUpMode(mode))) => follow_up_mode = mode,
                        Next::Ctl(None) => control_open = false,
                        Next::Delegation(Some(snapshot)) => {
                            let event = AgentEvent::DelegationUpdated { snapshot };
                            notify_observers(&observers, &event);
                            yield event;
                        }
                        Next::Delegation(None) => delegation_telemetry = None,
                        Next::Warm(step) => {
                            match cache_warmer.advance(step, CacheWarmHost {
                                session,
                                client: &client,
                                hooks: &extension_host.cache_warming_decision_hooks,
                                resource_owner: &resource_owner,
                                tool_generation: extension_host.tool_snapshot().0,
                                limits: CacheWarmLimits {
                                    max_session_tokens,
                                    max_session_cost_microdollars,
                                    pending_request: attempt_bound,
                                },
                            }) {
                                Ok(Some(event)) => {
                                    notify_observers(&observers, &event);
                                    yield event;
                                }
                                Ok(None) => {}
                                Err(error) => break 'run FinishReason::Failed(error.into()),
                            }
                        }
                        Next::Event(None) | Next::Event(Some(Err(_))) => {
                            let error = match next {
                                Next::Event(Some(Err(error))) => error,
                                _ => AiError::StreamProtocol(octet_ai::StreamProtocolError::MissingFinish),
                            };
                            if native_enabled {
                                native.connection = response_stream.into_native();
                                if let Err(record_error) = session.record_usage_uncertainty(model.endpoint.id.clone(), model.spec.id.clone(), "native_steering") { break 'run FinishReason::Failed(record_error.into()); }
                                let event = AgentEvent::ProviderUsageUncertain; notify_observers(&observers, &event); yield event;
                                break 'run FinishReason::Failed(error.into());
                            }
                            // Retire the failed stream before hooks, backoff,
                            // compaction or any replacement can open a transport.
                            drop(response_stream);
                            // An observed failure is settled, unlike an undriven
                            // Run drop. Its prefix must not block the replacement
                            // journal or reappear after a successful retry.
                            if let Some(journal) = assistant_frame_journal.as_mut() {
                                journal.settle();
                            }
                            if !attempt_saw_generation
                                && context_retries < MAX_PROVIDER_RETRIES
                                && looks_like_context_error(&error)
                            {
                                stream_context.provider_retry();
                                context_retries += 1;
                                let compacted = {
                                    let mut compaction = CompactionContext {
                        run_id: &effect_run_id,
                        resource_owner: &resource_owner,
                        retry_hooks: &provider_retry_hooks,
                        compaction_strategy: extension_host.compaction_strategy.as_ref(),
                        session_operation_hooks: &extension_host.session_operation_hooks,
                        max_network_wait,
                        provider_retries_enabled,
                        client: &client,
                                        model: &model,
                                        compaction_model: &compaction_model,
                                        summary_operation: crate::events::ProviderOperation::LocalCompaction,
                                        session,
                                        usage: &mut run_usage,
                                        run_cost: &mut run_cost,
                                        cache_retention,
                                        reasoning: &reasoning,
                                        reasoning_mode,
                                        session_id: &session_id,
                                        max_session_tokens,
                                        max_session_cost_microdollars,
                                        abort: &abort,
                                        mode: if background_tools.is_empty() && native.connection.is_none() { auto_compaction_mode } else { AgentCompactionMode::Disabled },
                                        threshold_fraction: compaction_threshold_fraction,
                                        keep_recent_tokens: compaction_keep_recent_tokens,
                                        events: &compaction_event_tx,
                                        context: &stream_context,
                                        tool_generation: tool_revision,
                                        capacity: &mut context_capacity,
                                        telemetry: turn_context.clone(),
                                    };
                                    let operation = compaction.force_one_boundary(
                                        &system,
                                        &request_tool_defs,
                                        compaction_reserve_tokens,
                                    );
                                    tokio::pin!(operation);
                                    let result = loop {
                                        tokio::select! {
                                            biased;
                                            _ = abort.wait() => break Err(AgentError::Cancelled),
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => { abort.set(); }
                                None => control_open = false,
                            },
                            Some(event) = compaction_event_rx.recv() => {
                                                notify_observers(&observers, &event);
                                                yield event;
                                            }
                                            result = &mut operation => break result,
                                        }
                                    };
                                    while let Ok(event) = compaction_event_rx.try_recv() {
                                        notify_observers(&observers, &event);
                                        yield event;
                                    }
                                    result
                                };
                                match compacted {
                                    Ok(()) => continue 'run,
                                    Err(error) if matches!(&error, AgentError::Cancelled) => {
                                        break 'run FinishReason::Aborted;
                                    }
                                    Err(error) => {
                                        break 'run FinishReason::Failed(error);
                                    }
                                }
                            }
                            pending_recovery = Some(PendingProviderRecovery {
                                error, qualified, saw_generation: attempt_saw_generation, opened: true,
                                exposure: attempt_bound,
                            });
                            continue 'run;
                        }
                        Next::Event(Some(Ok(event))) => {
                            stream_context.observe_stream(&event);
                            // Journal the frame this event produces, if any.
                            // Terminal events produce no frame and are handled
                            // by settlement below.
                            if let Ok(Some(frame)) = assistant_frame_encoder.encode(&event) {
                                if let Some(journal) = assistant_frame_journal.as_mut() {
                                    let _ = journal.append(&frame);
                                }
                            }
                            match event {
                            StreamEvent::ProviderLifecycle(lifecycle) => {
                                let ev = AgentEvent::ProviderLifecycle { lifecycle };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            StreamEvent::TextDelta { delta, .. } => {
                                attempt_saw_generation = true;
                                let ev = AgentEvent::OutputDelta {
                                    channel: OutputChannel::Text,
                                    text: delta,
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            StreamEvent::ReasoningDelta { delta, .. } => {
                                attempt_saw_generation = true;
                                let ev = AgentEvent::OutputDelta {
                                    channel: OutputChannel::Reasoning,
                                    text: delta,
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            // `octet-ai` assembles and validates the complete
                            // message. Tool deltas are provisional: execute only
                            // after the assistant turn is durably persisted.
                            StreamEvent::ToolCallStart { .. }
                            | StreamEvent::ToolCallArgsDelta { .. }
                            | StreamEvent::ToolCallEnd { .. } => {
                                attempt_saw_generation = true;
                            }
                            StreamEvent::MediaCompleted { index, media } => {
                                attempt_saw_generation = true;
                                let ev = AgentEvent::OutputMedia { index, media };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            StreamEvent::Finished(response) => {
                                // Terminal settlement: the frame sequence is
                                // partial progress only and must never be
                                // republished once the attempt is complete.
                                if let Some(journal) = assistant_frame_journal.as_mut() {
                                    journal.settle();
                                }
                                break Ok(response)
                            }
                            _ => {}
                            }
                        },
                    }
                };
                let response = match turn {
                    Ok(response) => {
                        turn_attempt_succeeded = true;
                        CompletionAttributes::usage(&response.usage)
                            .with_inference(response.inference.as_ref())
                            .with_uncertainty(session.has_uncertain_usage())
                            .record(&request_guard.span);
                        stream_guard.finish(false);
                        request_guard.finish(false);
                        response
                    }
                    Err(reason) => break 'run reason,
                };
                if let Some(metrics) = response.inference.clone() {
                    yield AgentEvent::ProviderInference { metrics };
                }
                // Context-recovery attempts are scoped to one logical provider
                // turn. A successful response proves the current compacted
                // prefix is accepted and restores the recovery budget for a
                // later autonomous turn in the same run.
                context_retries = 0;
                stream_retries = 0;
                recovery_budget = ProviderRecoveryBudget::default();
                network_retries = 0;
                network_deadline = None;
                failed_usage_unknown = session.has_uncertain_usage();
                // Max-turns counts completed provider turns. Context rejection
                // and transport recovery happen within the same logical turn
                // and must not consume the autonomous work budget.
                completed_turns = completed_turns.saturating_add(1);
                if native.has_pending() {
                    native.connection = response_stream.into_native();
                } else {
                    drop(response_stream);
                    native.control = None;
                }

                // ── Persist the completed assistant message ────────────────
                // StopReason is semantic control data, not parser metadata. It
                // must be inspected before deciding whether a no-tool turn is
                // a successful completion.
                let stop_reason = response.stop_reason.clone();
                let turn_usage = response.usage;
                let raw_responses_output = response.responses_output.clone();
                let deferred_handle = response.deferred.clone();
                let deferred_diagnostics = response.diagnostics.clone();
                let assistant = response.message;
                let mut calls: Vec<ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        octet_ai::AssistantPart::ToolCall(tc) => Some(tc.clone()),
                        _ => None,
                    })
                    .collect();
                let mut preparation_errors = HashMap::new();
                for call in &mut calls {
                    let Some(tool) = tool_map.get(&call.name).filter(|tool| tool.prepares_arguments()) else { continue; };
                    let Ok(arguments) = call.arguments_value() else { continue; };
                    let prepared = tokio::select! {
                        biased;
                        _ = abort.wait() => Err(cancelled_tool_error()),
                        result = tool.prepare_arguments(arguments, &resource_owner, abort.cancellation.clone()) => result,
                    };
                    match prepared {
                        Ok(arguments) => {
                            call.arguments_json = serde_json::to_string(&arguments).expect("JSON arguments serialize");
                            call.argument_error = if matches!(octet_ai::validate_tool_arguments(&call.name, &arguments, &request_tool_defs), Ok(octet_ai::ToolArgumentValidation::Valid)) {
                                None
                            } else { Some(ToolCallArgumentError::SchemaMismatch) };
                        },
                        Err(error) => { call.argument_error = Some(ToolCallArgumentError::SchemaMismatch); preparation_errors.insert(call.id.clone(), error); },
                    }
                }

                if auto_compaction_mode == AgentCompactionMode::NativeResponses
                    && model.spec.protocol == Protocol::OpenAiResponses
                    && raw_responses_output.is_none()
                    // A parked request has no response output by definition;
                    // it is a durable suspension, not a malformed native turn.
                    && !matches!(stop_reason, StopReason::Deferred)
                {
                    add_usage(&mut run_usage, &turn_usage);
                    let turn_cost = response.cost;
                    if let Err(error) = session.record_rejected_responses_turn_usage(
                        model.endpoint.id.clone(),
                        model.spec.id.clone(),
                        turn_usage,
                        turn_cost,
                    ) {
                        break 'run FinishReason::Failed(error.into());
                    }
                    run_cost.add(turn_cost);
                    break 'run FinishReason::Failed(AgentError::IncompleteResponse {
                        stop_reason:
                            "native Responses mode requires non-empty authoritative terminal output"
                                .to_owned(),
                    });
                }

                let persistence_context = assistant_persistence_context(
                    &effect_run_id,
                    &resource_owner,
                    &assistant,
                    stop_reason.clone(),
                );
                let persistence_metadata = collect_persistence_metadata(
                    &persistence_metadata_hooks,
                    &persistence_context,
                    &abort,
                )
                .await;
                let persistence_metadata = capture_tool_replay_safety(&calls, &tool_map, persistence_metadata);

                // ── Durable deferred park (rows 4.12 / 1e.1) ─────────────
                // The provider did not finish this request. Retrying the
                // generation request would be dishonest and could bill the same
                // effect twice, so the turn is persisted with its deferred stop
                // reason (Pi keeps that boundary in history) and the run parks
                // at a durable `deferred.suspended` leaf. A later permitted
                // pass polls the recorded handle; only a valid handle parks.
                if matches!(stop_reason, StopReason::Deferred) {
                    let deferred_usage_billed = usage_is_billed(&turn_usage);
                    let assistant_entry = if deferred_usage_billed {
                        match session.append_assistant_turn_with_metadata(
                            assistant.clone(),
                            model.endpoint.id.clone(),
                            model.spec.id.clone(),
                            turn_usage,
                            response.cost,
                            stop_reason.clone(),
                            None,
                            persistence_metadata.clone(),
                        ) {
                            Ok(entry) => entry,
                            Err(error) => break 'run FinishReason::Failed(error.into()),
                        }
                    } else {
                        // A parked request reports no billed usage. Recording a
                        // zero-token unpriced operation would falsely block hard
                        // cost ceilings, so the boundary is persisted without a
                        // usage record; the settled poll's response carries the
                        // authoritative usage for the whole request.
                        match session.append_with_metadata(
                            EntryValue::Message(Message::Assistant(assistant.clone())),
                            persistence_metadata.clone(),
                        ) {
                            Ok(entry) => entry,
                            Err(error) => break 'run FinishReason::Failed(error.into()),
                        }
                    };
                    add_usage(&mut run_usage, &turn_usage);
                    run_cost.add(response.cost);
                    let operation_id = format!("{effect_run_id}:deferred:{completed_turns}");
                    let declaration = DeferredResponseDeclaration {
                        stop_reason: crate::tools::deferred::DeferredStopReason::Deferred,
                        api: deferred_handle
                            .as_ref()
                            .map(|handle| handle.api.clone())
                            .unwrap_or_default(),
                        handle: deferred_handle.clone().map(DeferredHandle::from),
                    };
                    let identity = deferred_model_identity(&model);
                    let store = session.deferred_run_store();
                    match store.suspend(
                        &identity,
                        &operation_id,
                        &assistant_entry.0,
                        declaration,
                    ) {
                        Ok(DeferredSuspendDecision::Suspended(leaf)) => {
                            let suspension = DeferredRunSuspended {
                                operation_id: leaf.operation_id.clone(),
                                source_entry_id: leaf.source_entry_id.clone(),
                                stop_reason: crate::tools::deferred::DeferredStopReason::Deferred,
                                handle: leaf.handle.clone(),
                                poll: leaf.poll,
                                generation: leaf.generation,
                            };
                            for observer in &observers.observers {
                                observer.on_run_suspend(&suspension);
                            }
                            let _deferred_guard = telemetry.begin_typed::<DeferredRunSpan>(
                                DeferredRunAttributes {
                                    operation_id: bounded_deferred_label(&leaf.operation_id),
                                    stop_reason: "deferred".to_owned(),
                                    phase: "suspended".to_owned(),
                                    poll: leaf.poll,
                                    generation: leaf.generation,
                                    recovery: false,
                                    diagnostics: deferred_diagnostics.len(),
                                },
                            );
                            break 'run FinishReason::Failed(AgentError::DeferredSuspended {
                                operation_id: leaf.operation_id.clone(),
                                poll: leaf.poll,
                                generation: leaf.generation,
                            });
                        }
                        Ok(DeferredSuspendDecision::Settled) => {}
                        Ok(DeferredSuspendDecision::Failed(failure)) => {
                            break 'run FinishReason::Failed(
                                AgentError::DeferredSuspensionRefused {
                                    diagnostic: failure.diagnostic.clone(),
                                },
                            );
                        }
                        Err(error) => break 'run FinishReason::Failed(error.into()),
                    }
                }
                let assistant_entry = match session.append_assistant_turn_with_metadata(
                    assistant.clone(),
                    model.endpoint.id.clone(),
                    model.spec.id.clone(),
                    turn_usage,
                    response.cost,
                    stop_reason.clone(),
                    raw_responses_output,
                    persistence_metadata,
                ) {
                    Ok(entry) => entry,
                    Err(error) => break 'run FinishReason::Failed(error.into()),
                };
                model_turn_hooks.committed(session, completed_turns - 1, &assistant_entry, &calls);
                if let Err(error) = native.settle_successor(session, &model, assistant_entry) { break 'run FinishReason::Failed(error); }
                native.completed_prefix();
                match native.deliver(session, &control_prompt_metadata, &mut terminal_gate_evidence) {
                    Ok(Some(event)) => {
                        for custom in committed_custom_message_events(session, &mut custom_message_cursor) {
                            notify_observers(&observers, &custom); yield custom;
                        }
                        notify_observers(&observers, &event); yield event;
                    },
                    Ok(None) => {}, Err(error) => break 'run FinishReason::Failed(error),
                }
                context_capacity.observe_assistant_response(session, &model, &turn_usage);
                add_usage(&mut run_usage, &turn_usage);
                let turn_cost = response.cost;
                run_cost.add(turn_cost);
                let normal_end = matches!(stop_reason, StopReason::EndTurn | StopReason::StopSequence);
                let output_truncated = matches!(stop_reason, StopReason::MaxTokens);
                let needs_continuation = output_truncated
                    || matches!(stop_reason, StopReason::PauseTurn)
                    || (matches!(stop_reason, StopReason::Steered) && native.has_pending())
                    || matches!(&stop_reason, StopReason::Other(reason) if reason == "tool_output_locked");
                if normal_end && calls.is_empty() && background_tools.is_empty() && !assistant_has_terminal_content(&assistant) {
                    // A normal stop without terminal content is not a completed
                    // turn. Persist its message and usage above, then fail without
                    // retrying; the stop alone does not establish the cause.
                    break 'run FinishReason::Failed(AgentError::IncompleteResponse {
                        stop_reason: incomplete_terminal_response_reason(
                            &assistant,
                            &stop_reason,
                            &turn_usage,
                            &response.diagnostics,
                            request_max_output_tokens,
                        ),
                    });
                }
                let gated_candidate = completion_policy == CompletionPolicy::TerminalGate
                    && background_tools.is_empty() && !native.has_pending()
                    && calls.is_empty()
                    && normal_end;

                // Candidate turns stay provisional until their isolated gate
                // returns R. Tool turns and natural-policy answers commit now.
                if !gated_candidate {
                    let session_cost = priced_session_subtotal(session, &model);
                    let ev = AgentEvent::TurnFinished {
                        message: assistant.clone(),
                        stop_reason: stop_reason.clone(),
                        turn_usage,
                        turn_cost,
                        usage: run_usage,
                        session_cost_microdollars: session_cost,
                        run_cost_microdollars: run_cost.microdollars,
                    };
                    notify_observers(&observers, &ev);
                    yield ev;
                }

                // Results from the previous response retain their original IDs.
                // The concurrent assistant is committed first: it was generated
                // without these results. Sync/effectful work is a strict barrier.
                let completed_background = !background_tools.is_empty();
                while !background_tools.is_empty() {
                    let operation = background_tools.settle_one(session, &model, &sandbox,
                        &stream_context, &mut run_usage, &mut terminal_gate_evidence);
                    tokio::pin!(operation);
                    let settled = loop {
                        tokio::select! {
                            biased;
                            result = &mut operation => break result,
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer); answer_only = true; finish_pending = true;
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => abort.set(),
                                None => control_open = false,
                            },
                        }
                    };
                    match settled {
                        Ok(events) => for event in events { notify_observers(&observers, &event); yield event; },
                        Err(error) => break 'run FinishReason::Failed(error),
                    }
                    context_capacity.invalidate();
                }

                // Prior async batches have now settled; no-tool assistants are
                // also durable. A tool-emitting current turn stays pending until
                // its own complete result batch commits below.
                let observation = model_turn_hooks.settle(session, &abort.cancellation).await;
                for event in model_turn_hooks.take_warnings() {
                    notify_observers(&observers, &event); yield event;
                }
                if let Err(finish) = observation {
                    break 'run finish;
                }

                // Drain control before deciding whether a provisional candidate
                // is terminal. New user input takes precedence over the gate.
                {
                    let mut admission = control_admission.lock().unwrap_or_else(|error| error.into_inner());
                    while control_open {
                        match control_rx.try_recv() {
                            Ok(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                            Ok(Control::FollowUp(input)) => followups.push_back(input),
                            Ok(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                            Ok(Control::FinishNow(input)) => {
                                input.push_pending(&mut pending_steer);
                                answer_only = true;
                                finish_pending = true;
                                context_capacity.invalidate();
                            }
                            Ok(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                            Ok(Control::SetSteeringMode(mode)) => steering_mode = mode,
                            Ok(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                            Ok(Control::Abort) => {
                                abort.set();
                                break;
                            }
                            Err(mpsc::error::TryRecvError::Empty) => break,
                            Err(mpsc::error::TryRecvError::Disconnected) => control_open = false,
                        }
                    }
                    pending_steer.retain(ReservedInput::is_pending);
                    if !gated_candidate && !completed_background && !native.has_pending() && calls.is_empty() && normal_end && !needs_continuation
                        && pending_steer.is_empty() && pending_reasoning.is_none() && followups.is_empty() {
                        *admission = false;
                    }
                }

                if background_tools.is_empty() && calls.is_empty() && !native.has_pending() {
                    if let Err(error) = append_context_inputs(&mut pending_context, session, &model).await {
                        break 'run FinishReason::Failed(error);
                    }
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                    context_capacity.invalidate();
                }

                // A response is not successful merely because it contains no
                // tool calls. Refusals, pauses, provider-specific reasons, and
                // malformed tool-use endings are terminal failures; a length
                // stop gets one corrective continuation instead.
                if !normal_end
                    && !needs_continuation
                    && !matches!(stop_reason, StopReason::ToolUse)
                {
                    break 'run FinishReason::Failed(AgentError::IncompleteResponse {
                        stop_reason: stop_reason.as_canonical().to_owned(),
                    });
                }

                if native.has_pending() && calls.is_empty() {
                    if abort.is_set() { break 'run FinishReason::Aborted; }
                    continue 'run;
                }
                if completed_background && calls.is_empty() && normal_end {
                    if abort.is_set() { break 'run FinishReason::Aborted; }
                    continue 'run;
                }
                if calls.is_empty() {
                    if abort.is_set() {
                        if gated_candidate {
                            let ev = AgentEvent::CandidateRejected {
                                usage: run_usage,
                                run_cost_microdollars: run_cost.microdollars,
                                session_cost_microdollars: priced_session_subtotal(session, &model),
                            };
                            notify_observers(&observers, &ev);
                            yield ev;
                        }
                        break 'run FinishReason::Aborted;
                    }
                    if needs_continuation {
                        let instruction = continuation_instruction(&stop_reason);
                        if let Err(e) = session.append(user_message(UserInput::from(instruction))) {
                            break 'run FinishReason::Failed(e.into());
                        }
                        continue;
                    }
                    if !normal_end {
                        break 'run FinishReason::Failed(AgentError::IncompleteResponse {
                            stop_reason: stop_reason.as_canonical().to_owned(),
                        });
                    }
                    // Steering and follow-ups make this a normal intermediate
                    // turn, so commit it without spending a gate request.
                    pending_steer.retain(ReservedInput::is_pending);
                if native.connection.is_none() && (!pending_steer.is_empty() || pending_reasoning.is_some()) {
                        if gated_candidate {
                            let session_cost = priced_session_subtotal(session, &model);
                            let ev = AgentEvent::TurnFinished {
                                message: assistant.clone(),
                                stop_reason: stop_reason.clone(),
                                turn_usage,
                                turn_cost,
                                usage: run_usage,
                                session_cost_microdollars: session_cost,
                                run_cost_microdollars: run_cost.microdollars,
                            };
                            notify_observers(&observers, &ev);
                            yield ev;
                        }
                        continue;
                    }
                    if !followups.is_empty() {
                        if gated_candidate {
                            let session_cost = priced_session_subtotal(session, &model);
                            let ev = AgentEvent::TurnFinished {
                                message: assistant.clone(),
                                stop_reason: stop_reason.clone(),
                                turn_usage,
                                turn_cost,
                                usage: run_usage,
                                session_cost_microdollars: session_cost,
                                run_cost_microdollars: run_cost.microdollars,
                            };
                            notify_observers(&observers, &ev);
                            yield ev;
                        }
                        let queued = match follow_up_mode {
                            QueueDeliveryMode::All => followups.drain(..).collect::<Vec<_>>(),
                            QueueDeliveryMode::OneAtATime => {
                                vec![followups.pop_front().expect("follow-up queue is non-empty")]
                            }
                        };
                        let visible_tools = if answer_only {
                            &[][..]
                        } else {
                            tool_defs.as_slice()
                        };
                        let observation = ContextObservation {
                            tracker: &stream_context,
                            model: &model,
                            system: &system,
                            tools: visible_tools,
                        };
                        match deliver_control_inputs(
                            queued,
                            ControlDeliveryKind::FollowUp,
                            session,
                            &control_prompt_metadata,
                            &mut terminal_gate_evidence,
                            &observation,
                            Some(&abort),
                        ).await {
                            ControlDelivery::Completed { event } => {
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                                if let Some(ev) = event {
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                            }
                            ControlDelivery::Interrupted { event, finish } => {
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                                if let Some(ev) = event {
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                                break 'run finish;
                            }
                        }
                        continue;
                    }
                    if let Some(evidence) = terminal_gate_evidence.as_ref() {
                        let capsule = terminal_gate_capsule(evidence, &assistant);
                        let decision = {
                            let mut gate = TerminalGateContext {
                                run_id: &effect_run_id,
                                resource_owner: &resource_owner,
                                retry_hooks: &provider_retry_hooks,
                                max_network_wait,
                                provider_retries_enabled,
                                events: &compaction_event_tx,
                                client: &client,
                                model: &model,
                                session,
                                usage: &mut run_usage,
                                run_cost: &mut run_cost,
                                cache_retention,
                                session_id: &session_id,
                                max_session_tokens,
                                max_session_cost_microdollars,
                                abort: &abort,
                            };
                            let operation = gate.decide(capsule);
                            tokio::pin!(operation);
                            loop {
                                tokio::select! {
                                    biased;
                                    _ = abort.wait() => break Err(AgentError::Cancelled),
                                    control = control_rx.recv(), if control_open => match control {
                                        Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                        Some(Control::FollowUp(input)) => followups.push_back(input),
                                        Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                        Some(Control::FinishNow(input)) => {
                                            input.push_pending(&mut pending_steer);
                                            answer_only = true;
                                            finish_pending = true;
                                        }
                                        Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                        Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                        Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                        Some(Control::Abort) => { abort.set(); }
                                        None => control_open = false,
                                    },
                                    Some(event) = compaction_event_rx.recv() => {
                                        notify_observers(&observers, &event);
                                        yield event;
                                    }
                                    result = &mut operation => break result,
                                }
                            }
                        };
                        while let Ok(event) = compaction_event_rx.try_recv() {
                            notify_observers(&observers, &event);
                            yield event;
                        }
                        let return_candidate = matches!(decision, Ok(TerminalGateDecision::Return));
                        if return_candidate {
                            let session_cost = priced_session_subtotal(session, &model);
                            let ev = AgentEvent::TurnFinished {
                                message: assistant.clone(),
                                stop_reason: stop_reason.clone(),
                                turn_usage,
                                turn_cost,
                                usage: run_usage,
                                session_cost_microdollars: session_cost,
                                run_cost_microdollars: run_cost.microdollars,
                            };
                            notify_observers(&observers, &ev);
                            yield ev;
                        }
                        // Linearize successful submissions against terminal
                        // admission, including the gate's final poll and the
                        // TurnFinished suspension. Never hold this lock at yield.
                        {
                            let mut admission = control_admission.lock().unwrap_or_else(|error| error.into_inner());
                            while control_open {
                                match control_rx.try_recv() {
                                    Ok(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                    Ok(Control::FollowUp(input)) => followups.push_back(input),
                                    Ok(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                    Ok(Control::FinishNow(input)) => {
                                        input.push_pending(&mut pending_steer);
                                        answer_only = true;
                                        finish_pending = true;
                                        context_capacity.invalidate();
                                    }
                                    Ok(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                    Ok(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                    Ok(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                    Ok(Control::Abort) => abort.set(),
                                    Err(mpsc::error::TryRecvError::Empty) => break,
                                    Err(mpsc::error::TryRecvError::Disconnected) => control_open = false,
                                }
                            }
                            pending_steer.retain(ReservedInput::is_pending);
                            if return_candidate && pending_steer.is_empty() && pending_reasoning.is_none() && followups.is_empty() {
                                *admission = false;
                            }
                        }
                        if abort.is_set() {
                            break 'run FinishReason::Aborted;
                        }
                        if decision.is_ok() {
                            // Steering and follow-ups make this a normal intermediate
                            // turn, so commit it without spending a gate request.
                            pending_steer.retain(ReservedInput::is_pending);
                if native.connection.is_none() && (!pending_steer.is_empty() || pending_reasoning.is_some()) {
                                if gated_candidate && !return_candidate {
                                    let session_cost = priced_session_subtotal(session, &model);
                                    let ev = AgentEvent::TurnFinished {
                                        message: assistant.clone(),
                                        stop_reason: stop_reason.clone(),
                                        turn_usage,
                                        turn_cost,
                                        usage: run_usage,
                                        session_cost_microdollars: session_cost,
                                        run_cost_microdollars: run_cost.microdollars,
                                    };
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                                continue;
                            }
                            if !followups.is_empty() {
                                if gated_candidate && !return_candidate {
                                    let session_cost = priced_session_subtotal(session, &model);
                                    let ev = AgentEvent::TurnFinished {
                                        message: assistant.clone(),
                                        stop_reason: stop_reason.clone(),
                                        turn_usage,
                                        turn_cost,
                                        usage: run_usage,
                                        session_cost_microdollars: session_cost,
                                        run_cost_microdollars: run_cost.microdollars,
                                    };
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                                let queued = match follow_up_mode {
                                    QueueDeliveryMode::All => followups.drain(..).collect::<Vec<_>>(),
                                    QueueDeliveryMode::OneAtATime => {
                                        vec![followups.pop_front().expect("follow-up queue is non-empty")]
                                    }
                                };
                                let visible_tools = if answer_only {
                                    &[][..]
                                } else {
                                    tool_defs.as_slice()
                                };
                                let observation = ContextObservation {
                                    tracker: &stream_context,
                                    model: &model,
                                    system: &system,
                                    tools: visible_tools,
                                };
                                match deliver_control_inputs(
                                    queued,
                                    ControlDeliveryKind::FollowUp,
                                    session,
                                    &control_prompt_metadata,
                                    &mut terminal_gate_evidence,
                                    &observation,
                                    Some(&abort),
                                ).await {
                                    ControlDelivery::Completed { event } => {
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                                        if let Some(ev) = event {
                                            notify_observers(&observers, &ev);
                                            yield ev;
                                        }
                                    }
                                    ControlDelivery::Interrupted { event, finish } => {
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                                        if let Some(ev) = event {
                                            notify_observers(&observers, &ev);
                                            yield ev;
                                        }
                                        break 'run finish;
                                    }
                                }
                                continue;
                            }
                        }
                        match decision {
                            Ok(TerminalGateDecision::Return) => {
                                break 'run FinishReason::Completed;
                            }
                            Ok(TerminalGateDecision::Continue) => {
                                let ev = AgentEvent::CandidateRejected {
                                    usage: run_usage,
                                    run_cost_microdollars: run_cost.microdollars,
                                    session_cost_microdollars: priced_session_subtotal(session, &model),
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                                if let Err(error) = session.append(user_message(UserInput::from(
                                    TERMINAL_GATE_CORRECTION,
                                ))) {
                                    break 'run FinishReason::Failed(error.into());
                                }
                                continue;
                            }
                            Err(AgentError::Cancelled) => {
                                let ev = AgentEvent::CandidateRejected {
                                    usage: run_usage,
                                    run_cost_microdollars: run_cost.microdollars,
                                    session_cost_microdollars: priced_session_subtotal(session, &model),
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                                break 'run FinishReason::Aborted;
                            }
                            Err(error) => {
                                let ev = AgentEvent::CandidateRejected {
                                    usage: run_usage,
                                    run_cost_microdollars: run_cost.microdollars,
                                    session_cost_microdollars: priced_session_subtotal(session, &model),
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                                break 'run FinishReason::Failed(error);
                            }
                        }
                    }
                    break 'run FinishReason::Completed;
                }

                // A model can emit several independent observations in one
                // turn. Scan only the next contiguous run of exact,
                // host-classified read observations; every mutation, process,
                // delegation, extension, network, unknown, schema-invalid, or
                // sequential tool is a barrier. The live read predicate is
                // intentionally broader than the crash-replay predicate:
                // HostRead remains ambient authority and is never relabeled.
                let parallel_active_skills = session
                    .head()
                    .and_then(|head| session.resolve_active_skills(&head).ok())
                    .map(|state| state.active_skills)
                    .unwrap_or_default();
                let classification_context = ToolContext {
                    workspace: &sandbox.workspace,
                    sandbox: &sandbox,
                    execution_scope: &tool_scope,
                    resource_owner: &resource_owner,
                    active_skills: &parallel_active_skills,
                    registered_tools: &registered_tools,
                    progress: ToolProgressSink::null(),
                    cancellation: CancellationToken::default(),
                };
                // Defer only a complete, bounded batch of independent host
                // observations. Mixed/sync/effectful batches keep ordinary order.
                // Hard ceilings serialize tool accounting before another request.
                if model.responses_features().async_tools
                    && max_session_tokens.is_none() && max_session_cost_microdollars.is_none()
                    && calls.len() <= parallel_read_wave_width
                    && !abort.is_set()
                    && calls.iter().enumerate().all(|(index, call)| {
                        call.async_execution
                            && request_tool_defs.iter().any(|definition| definition.name == call.name && definition.async_execution)
                            && parallel_read_candidate(call, index, answer_only, output_truncated, &tool_map, &classification_context)
                    }) {
                    for (index, call) in calls.iter().enumerate() {
                        let invocation = match session.tool_invocation(index) {
                            Ok(handle) => handle,
                            Err(error) => break 'run FinishReason::Failed(error.into()),
                        };
                        stream_context.tool_started();
                        let event = AgentEvent::ToolStarted { id: call.id.clone(), name: call.name.clone(), args: call.arguments_value().expect("complete admitted arguments") };
                        notify_observers(&observers, &event); yield event;
                        background_tools.start(call.clone(), invocation, tool_map[&call.name].clone(),
                            tool_call_hooks.clone(), effect_broker.clone(), effect_run_id.clone(), tool_revision,
                            sandbox.clone(), tool_scope.clone(), resource_owner.clone(), parallel_active_skills.clone(),
                            registered_tools.clone(), background_cancellation.clone());
                    }
                    continue 'run;
                }
                let mut parallel_results: VecDeque<ParallelReadWaveExecution> = VecDeque::new();
                // Row 4.10: every finalized result of this assistant batch, in
                // emitted order, decides the batch's termination request.
                let mut termination_requests: Vec<bool> = Vec::with_capacity(calls.len());

                // Calls in one assistant response form a single batch. Do not
                // treat parallel or otherwise batched identical calls as a
                // no-progress loop; only compare against earlier responses.
                let batch_fingerprints: Vec<(String, String)> = calls
                    .iter()
                    .filter(|call| call.argument_error.is_none())
                    .filter_map(|call| {
                        call.arguments_value().ok().map(|args| {
                            (
                                call.name.clone(),
                                tool_call_arguments_fingerprint(&call.name, &args),
                            )
                        })
                    })
                    .collect();

                // ── Commit tool results in emitted order ───────────────────
                let mut call_index = 0usize;
                while call_index < calls.len() {
                    if parallel_results.is_empty()
                        && !abort.is_set()
                        && parallel_read_candidate(
                            &calls[call_index],
                            call_index,
                            answer_only,
                            output_truncated,
                            &tool_map,
                            &classification_context,
                        )
                    {
                        let mut wave_end = call_index;
                        while wave_end < calls.len()
                            && wave_end - call_index < parallel_read_wave_width
                            && parallel_read_candidate(
                                &calls[wave_end],
                                wave_end,
                                answer_only,
                                output_truncated,
                                &tool_map,
                                &classification_context,
                            )
                        {
                            wave_end += 1;
                        }
                        // A single eligible call gains no overlap and keeps the
                        // ordinary sequential path's hook/control behavior.
                        if wave_end - call_index > 1 {
                            // Only this admitted, bounded wave owns live slots.
                            // Over-limit/static refusals never allocate handles.
                            let invocation_handles = match (call_index..wave_end)
                                .map(|index| session.tool_invocation(index))
                                .collect::<Result<Vec<_>, _>>() {
                                Ok(handles) => handles,
                                Err(error) => break 'run FinishReason::Failed(error.into()),
                            };
                            // Row 3.5: one tool boundary per call in the wave.
                            // They are settled together once the wave resolves.
                            let mut wave_tool_guards =
                                Vec::with_capacity(wave_end - call_index);
                            for call in &calls[call_index..wave_end] {
                                wave_tool_guards.push(turn_context.begin_typed::<ToolSpan>(
                                    ToolAttributes {
                                        name: call.name.clone(),
                                    },
                                ));
                                let parsed = call
                                    .arguments_value()
                                    .expect("parallel read wave validates arguments");
                                stream_context.tool_started();
                                let ev = AgentEvent::ToolStarted {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                    args: parsed,
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }

                            let operation = execute_parallel_read_wave(
                                &calls[call_index..wave_end],
                                &invocation_handles,
                                &tool_map,
                                &tool_call_hooks,
                                &effect_broker,
                                &effect_run_id,
                                tool_revision,
                                &sandbox,
                                &tool_scope,
                                &resource_owner,
                                &parallel_active_skills,
                                &registered_tools,
                                abort.cancellation.clone(),
                            );
                            tokio::pin!(operation);
                            let mut abort_observed = abort.is_set();
                            let completed = loop {
                                tokio::select! {
                                    biased;
                                    _ = abort.wait(), if !abort_observed => {
                                        abort_observed = true;
                                    }
                                    control = control_rx.recv(), if control_open => match control {
                                        Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                        Some(Control::FollowUp(input)) => followups.push_back(input),
                                        Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                        Some(Control::FinishNow(input)) => {
                                            input.push_pending(&mut pending_steer);
                                            answer_only = true;
                                            finish_pending = true;
                                            context_capacity.invalidate();
                                        }
                                        Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                        Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                        Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                        Some(Control::Abort) => {
                                            abort.set();
                                            abort_observed = true;
                                        }
                                        None => control_open = false,
                                    },
                                    results = &mut operation => break results,
                                    step = cache_warmer.next_step(), if !abort_observed => {
                                        if abort.is_set() {
                                            abort_observed = true;
                                            continue;
                                        }
                                        match cache_warmer.advance(step, CacheWarmHost {
                                            session,
                                            client: &client,
                                            hooks: &extension_host.cache_warming_decision_hooks,
                                            resource_owner: &resource_owner,
                                            tool_generation: extension_host.tool_snapshot().0,
                                            limits: CacheWarmLimits {
                                                max_session_tokens,
                                                max_session_cost_microdollars,
                                                pending_request: None,
                                            },
                                        }) {
                                            Ok(Some(event)) => {
                                                notify_observers(&observers, &event);
                                                yield event;
                                            }
                                            Ok(None) => {}
                                            Err(error) => break 'run FinishReason::Failed(error.into()),
                                        }
                                    },
                                }
                            };
                            for (guard, entry) in
                                wave_tool_guards.into_iter().zip(completed.iter())
                            {
                                if let Some(output) = resolved_tool_output(&entry.execution.result) {
                                    if let Some(usage) = output.usage() {
                                        CompletionAttributes::usage(usage).record(&guard.span);
                                    }
                                }
                                guard.finish(tool_execution_failed(&entry.execution.result));
                            }
                            parallel_results.extend(completed);
                        }
                    }
                    let call = calls[call_index].clone();
                    let argument_error = call.argument_error;
                    let parsed = call.arguments_value();
                    let call_fingerprint = if argument_error.is_none() {
                        parsed.as_ref().ok().map(|args| {
                            (
                                call.name.clone(),
                                tool_call_arguments_fingerprint(&call.name, args),
                            )
                        })
                    } else {
                        None
                    };
                    let repeated_recently = call_fingerprint.as_ref().map_or(0, |fingerprint| {
                        recent_tool_calls
                            .iter()
                            .filter(|previous| *previous == fingerprint)
                            .count()
                    });
                    let should_annotate_repetition =
                        repeated_recently >= REPEATED_TOOL_CALL_THRESHOLD;
                    let (preexecuted, deferred_after) =
                        match parallel_results.pop_front() {
                            Some(ParallelReadWaveExecution { execution, after }) => {
                                (Some(execution), after)
                            }
                            None => (None, None),
                        };
                    let invocation = if argument_error.is_none()
                        && preexecuted.is_none()
                        && !answer_only
                        && !output_truncated
                        && call_index < MAX_TOOL_CALLS_PER_TURN
                        && !abort.is_set()
                        && tool_map.contains_key(&call.name)
                        && parsed.is_ok()
                    {
                        match session.tool_invocation(call_index) {
                            Ok(handle) => Some(handle),
                            Err(error) => break 'run FinishReason::Failed(error.into()),
                        }
                    } else {
                        None
                    };
                    let mut tool_guard: Option<SpanGuard> = None;
                    if preexecuted.is_none() {
                        // Row 3.5: one tool boundary per executed call, settled
                        // at the durable result boundary below.
                        tool_guard = Some(turn_context.begin_typed::<ToolSpan>(
                            ToolAttributes {
                                name: call.name.clone(),
                            },
                        ));
                        stream_context.tool_started();
                        let ev = AgentEvent::ToolStarted {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            args: parsed
                                .as_ref()
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }
                    let CompletedToolExecution {
                        result,
                        mut policy_decision,
                        duration,
                        mut progress_rx,
                        progress_sink,
                        cancellation_won,
                        started_unix_ms,
                        finished_unix_ms,
                    } = if let Some(argument_error) = argument_error {
                        // Do not classify effects, run hooks, or invoke the
                        // tool for a call already rejected by the request's
                        // schema snapshot. The paired static error is durable
                        // and safe to show back to the model.
                        rejected_argument_tool_execution(
                            argument_error,
                            &sandbox,
                            &effect_broker,
                        )
                    } else if let Some(execution) = preexecuted {
                        execution
                    } else {
                        // Create a fresh progress channel for every sequential
                        // call. Non-streaming tools simply never push into it.
                        let (progress_tx, mut progress_rx) =
                            mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
                        let progress_sink = ToolProgressSink::live(progress_tx);
                        let progress_sink = match &invocation {
                            Some(handle) => progress_sink.with_invocation(handle.clone()),
                            None => progress_sink,
                        };
                        let mut cancellation_won = false;
                        let start = std::time::Instant::now();
                        let started_at = Arc::new(AtomicU64::new(u64::MAX));
                        let started_at_marker = Arc::clone(&started_at);
                        let policy_decision_slot = Arc::new(Mutex::new(None));
                        // Row 4.8: the live panel's replaceable publications are
                        // paced for the whole call, and forced at its terminal
                        // boundary so a finished call never leaves stale state.
                        let mut live_preview = LivePreviewPacer::new();
                        // Row 4.7: the harness half of durable partial-output
                        // checkpoints. The tracker is created per invocation and
                        // dropped with the call, so a settled call has nothing
                        // left to republish.
                        #[cfg(any(unix, windows))]
                        let mut live_partial_output = partial_output_checkpoints.as_ref().and_then(|config| {
                            let mut resolved = config.clone();
                            if resolved.sink.is_none() {
                                resolved.sink = invocation.as_ref().map(|handle| {
                                    Arc::new(handle.clone()) as Arc<dyn crate::tool::PartialOutputCheckpointSink>
                                });
                            }
                            LivePartialOutput::for_call(&resolved, &call.name)
                        });
                        let result: Result<ToolOutput, ToolError> = if answer_only {
                            Err(ToolError::new(format!(
                                "tool call `{}` was not executed: the user requested an immediate final answer without tools",
                                call.name
                            )))
                        } else if output_truncated {
                            Err(ToolError::new(format!(
                                "tool call `{}` was not executed: the provider reached its output token limit, so the arguments may be truncated; re-issue the call with complete arguments",
                                call.name
                            )))
                        } else if call_index >= MAX_TOOL_CALLS_PER_TURN {
                            Err(ToolError::new(
                                "tool call skipped: per-turn tool-call limit reached",
                            ))
                    } else if abort.is_set() {
                        cancellation_won = true;
                        Err(cancelled_tool_error())
                    } else {
                        match (tool_map.get(&call.name), parsed) {
                            (None, _) => {
                                Err(ToolError::new(format!("unknown tool: {}", call.name)))
                            }
                            (Some(_), Err(_)) => {
                                let (error, decision) =
                                    invalid_tool_arguments_denial(&sandbox, &effect_broker);
                                *policy_decision_slot
                                    .lock()
                                    .expect("policy decision slot is not poisoned") = Some(decision);
                                Err(error)
                            }
                            (Some(tool), Ok(args)) => 'dispatch: {
                                let active_skills = session
                                    .head()
                                    .and_then(|head| session.resolve_active_skills(&head).ok())
                                    .map(|state| state.active_skills)
                                    .unwrap_or_default();
                                let (mut session_driver, session_route) = crate::extension_process::session_leaf::driver::SessionDriver::new(resource_owner.clone());
                                let routed_progress = progress_sink.clone().with_session_driver(session_route);
                                let composition_scope = tool.nested_execution().then(|| CompositionDispatcher::scope(
                                    call.id.clone(), call.name.clone(), composition_tools.clone(),
                                    sandbox.clone(), tool_scope.clone(), resource_owner.clone(),
                                    effect_run_id.clone(), tool_revision, active_skills.clone(),
                                    tool_call_hooks.clone(), effect_broker.clone(), routed_progress.clone(),
                                    abort.cancellation.clone(), session, model.clone(),
                                    max_session_tokens, max_session_cost_microdollars,
                                ));
                                let tool_progress = match &composition_scope {
                                    Some(scope) => routed_progress.clone().with_composition(scope.0.clone()),
                                    None => routed_progress.clone(),
                                };
                                let tool_ctx = ToolContext {
                                    workspace: &sandbox.workspace,
                                    sandbox: &sandbox,
                                    execution_scope: &tool_scope,
                                    resource_owner: &resource_owner,
                                    active_skills: &active_skills,
                                    registered_tools: &registered_tools,
                                    progress: tool_progress.with_tool_call_identity(call.id.0.clone(), None),
                                    cancellation: abort.cancellation.clone(),
                                };
                                let original_hook_arguments = args.clone();
                                let args = match session_driver.drive(transform_tool_arguments(
                                    &tool_call_hooks,
                                    tool.as_ref(),
                                    &call.name,
                                    args,
                                    &tool_ctx,
                                ), session, &abort.cancellation)
                                .await.unwrap_or_else(|error| Err(ToolError::new(error)))
                                {
                                    Ok(args) => args,
                                    Err(error) => {
                                        if tool_ctx.cancellation.is_cancelled() {
                                            cancellation_won = true;
                                            break 'dispatch Err(cancelled_tool_error());
                                        }
                                        let (_, decision) =
                                            secondary_hook_denial(&sandbox, &effect_broker, None);
                                        *policy_decision_slot
                                            .lock()
                                            .expect("policy decision slot is not poisoned") =
                                            Some(decision);
                                        break 'dispatch session_driver.drive(settle_tool_result_hooks(
                                            &tool_call_hooks, &call.name, &original_hook_arguments,
                                            Err(error), &tool_ctx, false,
                                        ), session, &abort.cancellation).await
                                            .unwrap_or_else(|error| Err(ToolError::new(error)));
                                    }
                                };
                                let hook_arguments = args.clone();
                                let policy_decision_marker = Arc::clone(&policy_decision_slot);
                                let operation = async {
                                    let admission = reserve_tool_effect(
                                        &effect_broker,
                                        tool.as_ref(),
                                        &call.name,
                                        &args,
                                        &tool_ctx,
                                        &resource_owner,
                                        &effect_run_id,
                                        tool_revision,
                                        &call.id,
                                        true,
                                    )
                                    .await;
                                    let ToolEffectAdmission {
                                        intent,
                                        reservation: effect_reservation,
                                        effect,
                                    } = match admission {
                                        Ok(admission) => admission,
                                        Err(ToolEffectAdmissionError { error, decision }) => {
                                            *policy_decision_marker
                                                .lock()
                                                .expect("policy decision slot is not poisoned") =
                                                Some(decision);
                                            return Err(error);
                                        }
                                    };
                                    for hook in &tool_call_hooks {
                                        if hook
                                            .before_tool_call(
                                                &call.name,
                                                &hook_arguments,
                                                &tool_ctx,
                                            )
                                            .await
                                            .is_err()
                                        {
                                            if tool_ctx.cancellation.is_cancelled() {
                                                return Err(cancelled_tool_error());
                                            }
                                            let (error, decision) = secondary_hook_denial(
                                                &sandbox,
                                                &effect_broker,
                                                Some(effect),
                                            );
                                            *policy_decision_marker
                                                .lock()
                                                .expect("policy decision slot is not poisoned") =
                                                Some(decision);
                                            return Err(error);
                                        }
                                    }
                                    if tool_ctx.cancellation.is_cancelled() {
                                        return Err(cancelled_tool_error());
                                    }
                                    let receipt = effect_reservation.commit(&intent).map_err(|error| {
                                        let (error, decision) = effect_reservation_commit_denial(
                                            &sandbox,
                                            &effect_broker,
                                            effect,
                                            &error,
                                        );
                                        *policy_decision_marker
                                            .lock()
                                            .expect("policy decision slot is not poisoned") =
                                            Some(decision);
                                        error
                                    })?;
                                    *policy_decision_marker
                                        .lock()
                                        .expect("policy decision slot is not poisoned") =
                                        Some(policy_decision(
                                            &sandbox,
                                            &effect_broker,
                                            Some(effect),
                                            Some(receipt.authorization()),
                                            None,
                                        ));
                                    started_at_marker
                                        .store(crate::session::now_unix_millis(), Ordering::Release);
                                    if composition_scope.is_some() {
                                        tokio::time::timeout(
                                            std::time::Duration::from_millis(crate::tool_composition::COMPOSITION_TIMEOUT_MS),
                                            tool.execute(args, &tool_ctx),
                                        ).await.unwrap_or_else(|_| Err(ToolError::new("composition exceeded the 30 second host deadline; state may be partially changed")))
                                    } else {
                                        tool.execute(args, &tool_ctx).await
                                    }
                                };
                                tokio::pin!(operation);
                                // Cancellation drops the pinned future, which
                                // kills any child process tree it spawned.
                                let outcome = loop {
                                    // Row 4.8's trailing timer: one deadline,
                                    // recomputed per wake, that publishes the
                                    // held replaceable state exactly once.
                                    let flush_at = live_preview
                                        .flush_deadline(std::time::Instant::now())
                                        .map(tokio::time::Instant::from_std);
                                    tokio::select! {
                                        biased;
                                        _ = abort.wait() => break None,
                                        c = control_rx.recv(), if control_open => match c {
                                            Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                            Some(Control::FollowUp(input)) => followups.push_back(input),
                                            Some(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                            Some(Control::FinishNow(input)) => {
                                                input.push_pending(&mut pending_steer);
                                                answer_only = true;
                                                finish_pending = true;
                                                context_capacity.invalidate();
                                            }
                                            Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                            Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                            Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                            Some(Control::Abort) => {
                                                abort.set();
                                                break None;
                                            }
                                            None => control_open = false,
                                        },
                                        event = session_driver.next() => {
                                            if let Err(error) = session_driver.service(event, session) {
                                                break Some(Err(ToolError::new(error)));
                                            }
                                        },
                                        r = &mut operation => break Some(r),
                                        step = cache_warmer.next_step() => {
                                            // Tool polling may synchronously
                                            // cancel while returning Pending.
                                            if abort.is_set() { break None; }
                                            match cache_warmer.advance(step, CacheWarmHost {
                                                session,
                                                client: &client,
                                                hooks: &extension_host.cache_warming_decision_hooks,
                                                resource_owner: &resource_owner,
                                                tool_generation: extension_host.tool_snapshot().0,
                                                limits: CacheWarmLimits {
                                                    max_session_tokens,
                                                    max_session_cost_microdollars,
                                                    pending_request: None,
                                                },
                                            }) {
                                                Ok(Some(event)) => {
                                                    notify_observers(&observers, &event);
                                                    yield event;
                                                }
                                                Ok(None) => {}
                                                Err(error) => break 'run FinishReason::Failed(error.into()),
                                            }
                                        },
                                        progress = progress_rx.recv() => {
                                            if let Some(p) = progress {
                                                // `operation` can enqueue progress and synchronously
                                                // trigger cancellation during the same select poll,
                                                // after the biased abort branch was already checked.
                                                // Recheck before accepting semantic state.
                                                match settle_tool_progress(p, abort.is_set(), session) {
                                                    ProgressSettlement::Cancelled => break None,
                                                    ProgressSettlement::Settled => {}
                                                    ProgressSettlement::Emit(p) => {
                                                        // Row 4.7: publish the
                                                        // bounded live snapshot
                                                        // before the panel sees
                                                        // the chunk, so durability
                                                        // never lags the panel.
                                                        #[cfg(any(unix, windows))]
                                                        if let Some(checkpoints) =
                                                            live_partial_output.as_mut()
                                                        {
                                                            checkpoints.observe_progress(
                                                                &p,
                                                                std::time::Instant::now(),
                                                            );
                                                        }
                                                        if let Some(progress) = forward_tool_progress(
                                                            p,
                                                            &mut live_preview,
                                                            std::time::Instant::now(),
                                                        ) {
                                                            let ev = AgentEvent::ToolProgress {
                                                                id: call.id.clone(),
                                                                progress,
                                                            };
                                                            notify_observers(&observers, &ev);
                                                            yield ev;
                                                        }
                                                    }
                                                }
                                            }
                                        },
                                        snapshot = async {
                                            match &mut delegation_telemetry {
                                                Some(receiver) => next_delegation_snapshot(receiver).await,
                                                None => std::future::pending().await,
                                            }
                                        }, if delegation_telemetry.is_some() => {
                                            // Keep delegated-worker telemetry
                                            // flowing while a long root tool is
                                            // executing, not only while the root
                                            // streams from the provider.
                                            match snapshot {
                                                Some(snapshot) => {
                                                    let event =
                                                        AgentEvent::DelegationUpdated { snapshot };
                                                    notify_observers(&observers, &event);
                                                    yield event;
                                                }
                                                None => delegation_telemetry = None,
                                            }
                                        },
                                        _ = tokio::time::sleep_until(
                                            flush_at.unwrap_or_else(tokio::time::Instant::now)
                                        ), if flush_at.is_some() => {
                                            // Publish the collapsed latest
                                            // replaceable state once its pace
                                            // deadline passed. Nothing else is
                                            // ever held back.
                                            if let Some(decoration) = live_preview
                                                .take_due(std::time::Instant::now())
                                            {
                                                let ev = AgentEvent::ToolProgress {
                                                    id: call.id.clone(),
                                                    progress: ToolProgress::Decoration(decoration),
                                                };
                                                notify_observers(&observers, &ev);
                                                yield ev;
                                            }
                                        },
                                    }
                                };
                                let result = match outcome {
                                    Some(_) if abort.is_set() => {
                                        cancellation_won = true;
                                        Err(cancelled_tool_error())
                                    }
                                    Some(result) => result,
                                    None => {
                                        cancellation_won = true;
                                        Err(cancelled_tool_error())
                                    }
                                };
                                if let Some(scope) = &composition_scope {
                                    scope.0.stop.cancel();
                                }
                                let result = match &composition_scope {
                                    Some(scope) => scope.0.collect_usage(result),
                                    None => result,
                                };
                                // Terminal boundary: the call is over, so the
                                // panel gets the collapsed latest decoration
                                // now, whatever the pace deadline says.
                                if let Some(decoration) =
                                    live_preview.settle(std::time::Instant::now())
                                {
                                    let ev = AgentEvent::ToolProgress {
                                        id: call.id.clone(),
                                        progress: ToolProgress::Decoration(decoration),
                                    };
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                                // Execution observers see only calls that reached
                                // the tool boundary; a call denied at admission
                                // still resolves its result transformations.
                                let executed =
                                    started_at.load(Ordering::Acquire) != u64::MAX;
                                session_driver.drive(settle_tool_result_hooks(
                                    &tool_call_hooks,
                                    &call.name,
                                    &hook_arguments,
                                    result,
                                    &tool_ctx,
                                    executed,
                                ), session, &abort.cancellation)
                                .await.unwrap_or_else(|error| Err(ToolError::new(error)))
                            }
                        }
                        };
                        let policy_decision = policy_decision_slot
                            .lock()
                            .expect("policy decision slot is not poisoned")
                            .take();
                        let started_at_value = started_at.load(Ordering::Acquire);
                        let started_unix_ms =
                            (started_at_value != u64::MAX).then_some(started_at_value);
                        CompletedToolExecution {
                            result,
                            policy_decision,
                            duration: start.elapsed(),
                            started_unix_ms,
                            finished_unix_ms: Some(crate::session::now_unix_millis()),
                            progress_rx,
                            progress_sink,
                            cancellation_won,
                        }
                    };
                    let result = if let Some(after) = deferred_after {
                        run_parallel_after_tool_hooks(
                            after,
                            &tool_call_hooks,
                            result,
                            &sandbox,
                            &tool_scope,
                            &resource_owner,
                            &parallel_active_skills,
                            &registered_tools,
                            abort.cancellation.clone(),
                        )
                        .await
                    } else {
                        result
                    };
                    let result = if should_annotate_repetition {
                        annotate_repeated_tool_result(result, repeated_recently)
                    } else {
                        result
                    };

                    apply_execution_policy_denial(&mut policy_decision, &result);

                    // A failed/cancelled script can still have completed billed
                    // nested calls. Persist their aggregate before another
                    // script is admitted; checkpoints alone are not the hard
                    // session-limit ledger.
                    let usage_commit = if let Some(usage) = resolved_tool_output(&result).and_then(ToolOutput::usage).copied() {
                        run_cost.add(None);
                        session.record_tool_composition_usage(call.id.0.clone(), usage)
                    } else { Ok(()) };

                    // Emit policy metadata before the durable result commit: a
                    // session-write failure must not hide a decision already
                    // made for this exact call.
                    if let Some(decision) = policy_decision {
                        let ev = AgentEvent::ToolPolicyDecision {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            decision,
                        };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }

                    if let Err(error) = usage_commit {
                        break 'run FinishReason::Failed(error.into());
                    }

                    // ── COMMIT BOUNDARY ──────────────────────────────────
                    // Tool::execute resolved (or an immediate error was
                    // produced). Persist the result immediately before
                    // draining progress or checking abort. An abort
                    // received after this point cannot erase an already-
                    // committed result.
                    // Every tool owns the same configured output allowance.
                    // A large early result must never starve later successful
                    // calls in the same model turn. Structured media is lowered
                    // only when the active model/protocol can replay it safely.
                    // Announce tools that appeared as a consequence of this
                    // execution (extension/MCP registrations). Later requests
                    // exclude announced schemas under deferred tool loading.
                    let (_, snapshot_tools) = extension_host.model_tool_snapshot(&resource_owner);
                    let snapshot_tools = crate::tool_composition::direct_surface(&snapshot_tools);
                    let newly_added: Vec<String> = snapshot_tools
                        .iter()
                        .map(|tool| tool.definition().name)
                        .filter(|name| !announced_tools.contains(name))
                        .collect();
                    if !newly_added.is_empty() {
                        announced_tools.extend(newly_added.iter().cloned());
                    }
                    // Recorded before the durable commit below, so the batch's
                    // termination decision can never be taken from a result
                    // that was not actually placed. A failed call has no
                    // result and never requests termination.
                    termination_requests
                        .push(tool_result_terminates_run(&result));
                    let (message, accepted_media, text, is_error, details) = lower_tool_result(
                        call.id.clone(),
                        &result,
                        &model,
                        sandbox.max_output_bytes,
                        newly_added,
                    );
                    let owner_images = if owner_tool_images_enabled {
                        Some(ToolOutput::new("").with_owner_presentation_images(
                            lowered_tool_result_media(&message),
                        ))
                    } else {
                        None
                    };
                    if let Some(evidence) = terminal_gate_evidence.as_mut() {
                        evidence.record_action(&call.name, &call.arguments_json, is_error, &text);
                    }
                    if let Err(e) = session.append_with_metadata(
                        EntryValue::Message(Message::User(message)),
                        details.map(|tool_output| EntryMetadata {
                            tool_output: Some(tool_output),
                            tool_started_unix_ms: started_unix_ms,
                            tool_finished_unix_ms: finished_unix_ms,
                            ..EntryMetadata::default()
                        }),
                    ) {
                        break 'run FinishReason::Failed(e.into());
                    }
                    // Internal durable-delivery tools may provisionally lease
                    // work while executing. Acknowledge it only once the
                    // complete, untruncated result is in the session.
                    resolve_tool_delivery_after_persistence(&result, sandbox.max_output_bytes);

                    // ── Drain accepted progress before ToolFinished ───────
                    let mut drain_preview = LivePreviewPacer::new();
                    while let Ok(p) = progress_rx.try_recv() {
                        match settle_tool_progress(p, cancellation_won, session) {
                            ProgressSettlement::Cancelled => continue,
                            ProgressSettlement::Settled => {}
                            ProgressSettlement::Emit(p) => {
                                if let Some(progress) = forward_tool_progress(
                                    p,
                                    &mut drain_preview,
                                    std::time::Instant::now(),
                                ) {
                                    let ev = AgentEvent::ToolProgress {
                                        id: call.id.clone(),
                                        progress,
                                    };
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                            }
                        }
                    }
                    // The call is over: whatever replaceable state the drain
                    // collapsed reaches the panel before ToolFinished.
                    if let Some(decoration) = drain_preview.settle(std::time::Instant::now()) {
                        let ev = AgentEvent::ToolProgress {
                            id: call.id.clone(),
                            progress: ToolProgress::Decoration(decoration),
                        };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }
                    // Report dropped progress if any.
                    let (dropped_bytes, dropped_events) = progress_sink.take_dropped();
                    if dropped_bytes > 0 || dropped_events > 0 {
                        let ev = AgentEvent::ToolProgress {
                            id: call.id.clone(),
                            progress: ToolProgress::Dropped {
                                bytes: dropped_bytes,
                                events: dropped_events,
                            },
                        };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }

                    stream_context.tool_finished();
                    let result = match result {
                        Ok(output) => Ok(output
                            .without_media_payloads_for(accepted_media)
                            .with_is_error(is_error)),
                        Err(error) => Err(error),
                    };
                    // Pi's per-tool-result usage is billed turn accounting, not
                    // model context: it is added to the run's cumulative totals
                    // below and never to `turn_usage` or a context estimate.
                    let tool_usage = resolved_tool_output(&result).and_then(ToolOutput::usage).copied();
                    let tool_failed = tool_execution_failed(&result);
                    let mut ev = AgentEvent::ToolFinished {
                        id: call.id.clone(),
                        result,
                        duration,
                    };
                    notify_observers(&observers, &ev);
                    if let (Some(images), AgentEvent::ToolFinished { result: Ok(output), .. }) =
                        (owner_images, &mut ev)
                    {
                        output.attach_owner_presentation_images(images);
                    }
                    yield ev;
                    if let Some(usage) = &tool_usage {
                        add_usage(&mut run_usage, usage);
                    }
                    if let Some(guard) = tool_guard {
                        if let Some(usage) = &tool_usage {
                            CompletionAttributes::usage(usage).record(&guard.span);
                        }
                        guard.finish(tool_failed);
                    }
                    call_index += 1;

                }
                for fingerprint in batch_fingerprints {
                    recent_tool_calls.push_back(fingerprint);
                    while recent_tool_calls.len() > MAX_RECENT_TOOL_CALLS {
                        recent_tool_calls.pop_front();
                    }
                }

                // Unlike TurnFinished, this awaited boundary includes every
                // paired tool result, in actual durable commit order.
                let observation = model_turn_hooks.settle(session, &abort.cancellation).await;
                for event in model_turn_hooks.take_warnings() {
                    notify_observers(&observers, &event); yield event;
                }
                if let Err(finish) = observation {
                    break 'run finish;
                }

                if background_tools.is_empty() && !native.has_pending() {
                    if let Err(error) = append_context_inputs(&mut pending_context, session, &model).await {
                        break 'run FinishReason::Failed(error);
                    }
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                    context_capacity.invalidate();
                }

                // Every emitted call now has a durable result, including calls
                // that were never started because the user aborted. Do not
                // enter another model turn after controlled cancellation.
                if abort.is_set() {
                    break 'run FinishReason::Aborted;
                }

                // Row 4.10: Pi's unanimity rule, applied to exactly one
                // assistant batch. Every emitted call already has a durable
                // result above, so a unanimous request ends the run instead of
                // entering another model turn; any sibling that did not ask to
                // stop keeps the batch going, and its result is never discarded.
                if batch_requests_termination(termination_requests.iter().copied()) {
                    // ToolFinished yields to the caller while admission is open.
                    // Drain and close under the same lock used by send(), just
                    // as for a natural terminal answer. Accepted user controls
                    // take precedence over a tool's request to stop.
                    let terminal = {
                        let mut admission = control_admission.lock().unwrap_or_else(|error| error.into_inner());
                        while control_open {
                            match control_rx.try_recv() {
                                Ok(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Ok(Control::FollowUp(input)) => followups.push_back(input),
                                Ok(Control::AppendCustom(input)) => input.push_pending(&mut pending_context),
                                Ok(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                    context_capacity.invalidate();
                                }
                                Ok(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Ok(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Ok(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Ok(Control::Abort) => { abort.set(); break; }
                                Err(mpsc::error::TryRecvError::Empty) => break,
                                Err(mpsc::error::TryRecvError::Disconnected) => control_open = false,
                            }
                        }
                        pending_steer.retain(ReservedInput::is_pending);
                        let terminal = pending_steer.is_empty() && followups.is_empty()
                            && pending_reasoning.is_none() && !native.has_pending();
                        if terminal { *admission = false; }
                        terminal
                    };
                    if abort.is_set() {
                        break 'run FinishReason::Aborted;
                    }
                    if terminal {
                        break 'run FinishReason::Completed;
                    }
                    if pending_steer.is_empty() && !followups.is_empty() {
                        let queued = match follow_up_mode {
                            QueueDeliveryMode::All => followups.drain(..).collect::<Vec<_>>(),
                            QueueDeliveryMode::OneAtATime => vec![followups.pop_front().expect("follow-up queue is non-empty")],
                        };
                        let observation = ContextObservation {
                            tracker: &stream_context,
                            model: &model,
                            system: &system,
                            tools: if answer_only { &[][..] } else { tool_defs.as_slice() },
                        };
                        match deliver_control_inputs(queued, ControlDeliveryKind::FollowUp, session,
                            &control_prompt_metadata, &mut terminal_gate_evidence, &observation, Some(&abort)).await {
                            ControlDelivery::Completed { event } => {
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                                if let Some(ev) = event { notify_observers(&observers, &ev); yield ev; }
                            }
                            ControlDelivery::Interrupted { event, finish } => {
                    for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                        notify_observers(&observers, &event); yield event;
                    }
                                if let Some(ev) = event { notify_observers(&observers, &ev); yield ev; }
                                break 'run finish;
                            }
                        }
                    }
                }

                if needs_continuation && !native.has_pending() {
                    let instruction = continuation_instruction(&stop_reason);
                    if let Err(e) = session.append(user_message(UserInput::from(instruction))) {
                        break 'run FinishReason::Failed(e.into());
                    }
                }
                // Context reconstruction coalesces the consecutive tool-result
                // entries into the provider-required single user message.
            };

            // Semantic/provider failure may follow a durable no-tool response.
            // Observe only genuinely settled entries; cancelled or failed hook
            // activations are never retried, including in terminal cleanup.
            let observation = model_turn_hooks.settle(session, &abort.cancellation).await;
            for event in model_turn_hooks.take_warnings() {
                notify_observers(&observers, &event); yield event;
            }
            if let Err(finish) = observation {
                reason = finish;
            }

            if session.has_unsettled_native_steering() {
                if let Err(error) = session.record_usage_uncertainty(model.endpoint.id.clone(), model.spec.id.clone(), "native_steering") { reason = FinishReason::Failed(error.into()); }
                let event = AgentEvent::ProviderUsageUncertain; notify_observers(&observers, &event); yield event;
            }
            if let Err(error) = native.cancel(session, &model) { reason = FinishReason::Failed(error); }

            // Every driven terminal cancels and pairs accepted background work.
            // Run drop instead aborts task handles; restart never replays them.
            background_cancellation.cancel();
            while !background_tools.is_empty() {
                match background_tools.settle_one(session, &model, &sandbox,
                    &stream_context, &mut run_usage, &mut terminal_gate_evidence).await {
                    Ok(events) => for event in events { notify_observers(&observers, &event); yield event; },
                    Err(error) => { reason = FinishReason::Failed(error); break; }
                }
            }

            // Row 3.5: settle the final turn before the run boundary. A turn
            // that never opened a provider attempt is not an error; one that
            // opened an attempt without a finished response is.
            if let Some(settled) = previous_turn.take() {
                settled.finish(
                    matches!(reason, FinishReason::Failed(_))
                        || (turn_attempt_opened && !turn_attempt_succeeded),
                );
            }
            *control_admission.lock().unwrap_or_else(|error| error.into_inner()) = false;
            control_rx.close();
            while let Ok(control) = control_rx.try_recv() {
                if let Control::AppendCustom(input) = control { input.push_pending(&mut pending_context); }
            }
            // No terminal path silently drops an admitted context-only message.
            if let Err(error) = append_context_inputs(&mut pending_context, session, &model).await {
                reason = FinishReason::Failed(error);
            }
            for event in committed_custom_message_events(session, &mut custom_message_cursor) {
                notify_observers(&observers, &event); yield event;
            }
            pending_steer.clear();
            followups.clear();
            // A fully driven prompt always leaves an explicit durable restore
            // point, including controlled abort/max-turn/failure outcomes. A
            // dropped stream is not complete and never reaches this boundary.
            // Failed provider turns also need an assistant boundary. Without
            // one, the next prompt is appended after the unresolved user task
            // and models commonly continue the stale request instead.
            if matches!(reason, FinishReason::Failed(_)) {
                if let Err(error) = close_failed_turn(session, &model) {
                    reason = FinishReason::Failed(error);
                }
            }
            if let Some(delegation) = &stream_delegation {
                // Session-scoped lifetime: the fleet survives this run. Mark
                // the detachment boundary explicitly, then mirror each
                // extension-owned worker's accounting delta into the root
                // ledger exactly once so surviving workers are never
                // double-counted and never lose accounting.
                delegation.detach_run();
                for delegated in delegation.delegated_usage_records() {
                    match mirror_delegated_uncertainty(session, &model, &delegated.agent_id, delegated.usage_uncertain, delegated.usage_exposure) {
                        Ok(true) => {
                            let event = AgentEvent::ProviderUsageUncertain;
                            notify_observers(&observers, &event);
                            yield event;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            // Persistence failed, but observed uncertainty must
                            // still stop presentation from claiming complete cost.
                            let event = AgentEvent::ProviderUsageUncertain;
                            notify_observers(&observers, &event);
                            yield event;
                            reason = FinishReason::Failed(error.into());
                            break;
                        }
                    }
                    if let Err(error) = record_delegated_usage_once(session, DelegatedUsage {
                        agent_id: delegated.agent_id,
                        turn_count: delegated.turn_count,
                        tool_call_count: delegated.tool_call_count,
                        endpoint: model.endpoint.id.clone(),
                        model: model.spec.id.clone(),
                        usage: delegated.usage,
                        cost: delegated.cost,
                    }) {
                        reason = FinishReason::Failed(error.into());
                        break;
                    }
                }
                if let Some(receiver) = delegation_telemetry.as_mut() {
                    if receiver.has_changed().unwrap_or(false) {
                        let snapshot = { receiver.borrow_and_update().clone() };
                        if let Some(snapshot) = snapshot {
                            let event = AgentEvent::DelegationUpdated { snapshot };
                            notify_observers(&observers, &event);
                            yield event;
                        }
                    }
                }
                delegation.detach_telemetry();
            }
            // Settlement never awaits a warm provider/hook. Idle mode keeps
            // the request-opening age; streaming mode stops at this boundary.
            // A dropped, undriven Run is instead cancelled by RunSessionGuard.
            let warm_usage_uncertainty_count = session.usage_uncertainty_records().len();
            if let Err(error) = cache_warmer.on_agent_settled(session) {
                reason = FinishReason::Failed(error.into());
            }
            if session.usage_uncertainty_records().len() > warm_usage_uncertainty_count {
                let event = AgentEvent::ProviderUsageUncertain;
                notify_observers(&observers, &event);
                yield event;
            }
            // Capacity checks use the incremental total-only cache. Refresh the
            // detailed snapshot once at the settled boundary so observers retain
            // an accurate final breakdown without paying for it on every turn.
            let _ = observe_context_tracker(&stream_context, session, &model, &system, &tool_defs);
            let checkpoint_usage = (completed_turns > 0).then_some(run_usage);
            let checkpoint_cost = model
                .spec
                .pricing
                .as_ref()
                .filter(|_| run_cost.unpriced_operations == 0)
                .map(|_| run_cost.microdollars);
            if let Err(error) = session.checkpoint_with_telemetry(
                first_entry.clone(),
                checkpoint_usage,
                checkpoint_cost,
            ) {
                reason = FinishReason::Failed(error.into());
            }
            let head = session.head().unwrap_or(first_entry);
            stream_context.run_finished(&reason);
            // Row 3.5: the run span settles at the durable run boundary, after
            // every recovery and checkpoint path has finalized `reason`.
            run_guard.finish(matches!(reason, FinishReason::Failed(_)));
            stream_lifecycle.finished.store(true, Ordering::Release);
            let ev = AgentEvent::RunFinished { head, reason };
            notify_observers(&observers, &ev);
            yield ev;
        };

        Ok(Run {
            stream: Box::pin(stream),
            control,
            lifecycle,
            context,
            delegation: run_delegation,
        })
    }
}
