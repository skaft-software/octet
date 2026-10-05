//! ExecutableExtensions turn hooks and prompt context.

use super::*;

/// Input accepted by Pi's early raw-input hook, before prompt composition.
pub struct ExtensionInput {
    pub text: String,
    pub images: Option<Vec<octet_ai::Media>>,
    pub transformed: bool,
}

#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum InputEventResult {
    Continue,
    Handled,
    Transform { text: String, #[serde(default)] images: Option<Vec<octet_ai::Media>> },
}

impl ExecutableExtensions {
    /// Run ordered early input handlers. `None` means handled: do not persist
    /// a prompt or start a provider request. Images omitted by a transform stay.
    pub async fn process_input(
        &mut self,
        text: String,
        images: Option<Vec<octet_ai::Media>>,
        source: &str,
        streaming_behavior: Option<&str>,
    ) -> anyhow::Result<Option<ExtensionInput>> {
        anyhow::ensure!(["interactive", "rpc", "extension"].contains(&source), "invalid input source");
        anyhow::ensure!(streaming_behavior.is_none_or(|value| ["steer", "followUp"].contains(&value)), "invalid input delivery");
        let mut input = ExtensionInput { text, images, transformed: false };
        for process in &self.processes {
            if !process.supports_feature("input_transform_v1")
                || !process.contributions().hooks.contains(&ExtensionHook::BeforePrompt) {
                continue;
            }
            let output = tokio::time::timeout(PROMPT_RPC_DEADLINE, process.run_hook(
                ExtensionHook::BeforePrompt,
                serde_json::json!({"phase":"input", "text":input.text, "images":input.images,
                    "source":source, "streaming_behavior":streaming_behavior}),
                extension_execution_context(process, self.resource_owner.as_deref()),
            )).await.map_err(|_| anyhow::anyhow!("extension input hook timed out"))??;
            anyhow::ensure!(output.disposition == ExtensionHookDisposition::Continue, "extension input hook refused input");
            let result = output.input_event.map(serde_json::from_value::<InputEventResult>).transpose()?;
            match result.unwrap_or(InputEventResult::Continue) {
                InputEventResult::Continue => {}
                InputEventResult::Handled => return Ok(None),
                InputEventResult::Transform { text, images } => {
                    anyhow::ensure!(text.len() <= 262144 && !text.contains('\0'), "transformed input exceeds bounds");
                    if let Some(images) = images {
                        anyhow::ensure!(images.len() <= 256, "transformed input image count exceeds bounds");
                        input.images = Some(images);
                    }
                    input.text = text;
                    input.transformed = true;
                }
            }
        }
        Ok(Some(input))
    }

    pub async fn begin_turn(&self) -> ExtensionTurnLifecycle {
        let sequence = NEXT_EXTENSION_RUN_ID.fetch_add(1, Ordering::Relaxed);
        let session_id = self
            .processes
            .first()
            .and_then(|process| process.current_context().host.session_id)
            .or_else(|| self.session_id.clone())
            .unwrap_or_else(|| "unknown-session".into());
        let run_id = format!("extension-run-{sequence}");
        let turn_id = format!("extension-turn-{sequence}");
        let resource_owner = self
            .resource_owner
            .clone()
            .unwrap_or_else(|| session_id.clone());
        let started_at = Instant::now();
        let processes = self.processes.clone();
        for process in &processes {
            process.set_active_lifecycle_turn(
                resource_owner.clone(),
                session_id.clone(),
                run_id.clone(),
                turn_id.clone(),
            );
        }
        let (started_tx, started_delivery) = watch::channel(false);
        let start_processes = processes.clone();
        let start_event = ExtensionLifecycleEvent::TurnStarted {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            turn_id: turn_id.clone(),
        };
        #[cfg(test)]
        let lifecycle_delivery_test_control = self.lifecycle_delivery_test_control.clone();
        tokio::spawn(async move {
            #[cfg(test)]
            if let Some(control) = lifecycle_delivery_test_control {
                control.wait_before_delivery(&start_event).await;
            }
            let _ = notify_lifecycle_all(&start_processes, start_event).await;
            let _ = started_tx.send(true);
        });
        let mut turn = ExtensionTurnLifecycle {
            processes,
            resource_owner,
            session_id,
            run_id,
            turn_id,
            started_at,
            started_delivery,
            settled: false,
            #[cfg(test)]
            lifecycle_delivery_test_control: self.lifecycle_delivery_test_control.clone(),
        };
        let _ = turn.started_delivery.wait_for(|started| *started).await;
        turn
    }

    pub async fn settle_turn(
        &mut self,
        turn: ExtensionTurnLifecycle,
        outcome: &crate::modes::HostRunOutcome,
    ) {
        let lifecycle_outcome = outcome.extension_lifecycle_outcome();
        let reason = outcome
            .failure_message()
            .map(|reason| clip_lifecycle_reason(reason, 4 * 1024));
        let diagnostics = turn.settle(lifecycle_outcome, reason).await;
        self.diagnostics.extend(diagnostics);
        self.last_lifecycle_outcome = Some(lifecycle_outcome);
    }

    /// Settles the old observational session boundary, updates every live
    /// process snapshot, starts observation for the replacement session, and
    /// fences queued active-session mutations from the previous snapshot. The
    /// active agent has already changed by the time this is called.
    pub fn transition_active_session(
        &mut self,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
    ) {
        self.transition_active_session_with_setup(session, model, reasoning, sessions, None);
    }

    pub fn transition_active_session_with_setup(
        &mut self, session: &Session, model: &Model, reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        setup: Option<(u64, octet_agent::extension_process::ExtensionResourceOwner)>,
    ) {
        self.pending_session_setup = setup;
        self.retire_active_resources();
        self.cancel_session_hook_starts();
        if self.session_lifecycle_started {
            let outcome = ExtensionLifecycleOutcome::Completed;
            if let Some(resource_owner) = self.resource_owner.clone() {
                let processes = self.processes.clone();
                match block_on_runtime(async move {
                    settle_session_hooks_all(&processes, &resource_owner, outcome).await
                }) {
                    Ok(messages) => self.diagnostics.extend(messages),
                    Err(error) => self.diagnostics.push(format!(
                        "warning: extension session hooks could not settle: {error}"
                    )),
                }
            }
            if let Some(session_id) = self.session_id.clone() {
                let processes = self.processes.clone();
                let event = ExtensionLifecycleEvent::SessionSettled {
                    session_id,
                    run_id: None,
                    outcome,
                    duration_ms: duration_millis(self.session_started_at.elapsed()),
                    reason: Some("active session changed".into()),
                };
                match block_on_runtime(async move { notify_lifecycle_all(&processes, event).await })
                {
                    Ok(messages) => self.diagnostics.extend(messages),
                    Err(error) => self.diagnostics.push(format!(
                        "warning: extension session lifecycle could not settle: {error}"
                    )),
                }
            }
            self.session_lifecycle_started = false;
        }
        self.last_lifecycle_outcome = None;
        // Contributions and owner-scoped presentation are observations of the
        // old active session and must not bleed into the replacement.
        self.pending_context = PendingContext::default();
        self.pending_post_mutation_rescans.clear();
        self.mutation_family_generations.clear();
        if let Some(bus) = &self.event_bus {
            bus.reset();
        }
        self.resource_owner = Some(session.resource_owner_key());
        let active_owner = self.resource_owner.as_deref();
        self.presentations.retain(|_, view| {
            view.resource_owner.is_none() || view.resource_owner.as_deref() == active_owner
        });
        self.refresh_host_state(session, model, reasoning, sessions);
        if self.pending_session_setup.is_none() { self.start_session_lifecycle(); }
        // The session changed in place, so requests admitted against the old
        // snapshot must not run against this replacement.
        self.activate_session_lifecycle_driver();
    }

    /// Validate every setup mutation against its original parent and live owner.
    pub fn session_setup_is_current(&self, parent: u64, owner: &octet_agent::extension_process::ExtensionResourceOwner) -> bool {
        self.resource_owner.as_deref() == Some(owner.session_id.as_str()) && self.pending_session_setup.as_ref().is_some_and(|(id, admitted)|
            *id == parent && admitted.extension_instance_id == owner.extension_instance_id && admitted.process_generation == owner.process_generation)
    }

    /// Start only after setup writes are complete; return the exact process
    /// handles whose deferred session_start callbacks the caller must await.
    pub fn complete_session_setup(&mut self) -> Vec<(ExtensionProcess, String)> {
        self.pending_session_setup = None;
        self.start_session_lifecycle();
        std::mem::take(&mut self.pending_session_hook_starts)
    }

    /// The launch already projected skills and session metadata for initialize.
    /// Only a changed final provider view or reasoning needs another projection.
    pub(crate) fn refresh_initial_host_state(
        &mut self,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
    ) {
        let initial = self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let unchanged = initial.model.as_deref() == Some(&model.spec.id.0)
            && initial.model_view == extension_model_view(model)
            && initial.reasoning == pi_thinking_level(model, reasoning).map(serde_json::Value::String);
        drop(initial);
        if !unchanged {
            self.refresh_host_state(session, model, reasoning, sessions);
        }
    }

    pub fn refresh_host_state(
        &mut self,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
    ) {
        let mut state = host_state(session, model, reasoning, sessions);
        state.pi_models = self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pi_models
            .clone();
        self.session_id = state.session_id.clone();
        for process in &self.processes {
            if process.descriptor().manifest.runtime.sharing
                == octet_agent::extension_process::ExtensionRuntimeSharing::Isolated
            {
                if let Err(error) = process.set_host_state_with_session(state.clone(), session) {
                    self.diagnostics.push(format!(
                        "warning: extension {:?} session mirror unavailable: {error}",
                        process.descriptor().manifest.name,
                    ));
                }
            }
        }
        // Every session or model boundary refreshes this cache, so a read-only
        // context snapshot never answers from the process-startup projection.
        *self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
    }

    pub(super) fn enqueue_context(
        &mut self,
        source: &str,
        contribution: ContextContribution,
    ) -> bool {
        admit_context(
            &mut self.pending_context,
            &mut self.diagnostics,
            source,
            contribution,
        )
    }

    pub(super) fn enqueue_contexts(
        &mut self,
        source: &str,
        contributions: impl IntoIterator<Item = ContextContribution>,
    ) {
        let mut dropped = 0usize;
        let mut last_error = None;
        for contribution in contributions {
            if let Err(error) = self.pending_context.try_push(contribution) {
                dropped = dropped.saturating_add(1);
                last_error = Some(error);
            }
        }
        if dropped > 0 {
            self.diagnostics.push(format!(
                "warning: {source}: dropped {dropped} extension context contribution(s): {}",
                last_error.unwrap_or_else(|| "context admission failed".into())
            ));
        }
    }

    pub async fn compose_prompt(
        &mut self,
        base_system: &str,
        prompt: String,
    ) -> anyhow::Result<ExtensionPromptComposition> {
        let mut notifications = self.drain_events();
        let mut effective_system = base_system.to_owned();
        let mut custom_messages = Vec::new();
        // Composition is transactional. Context already queued by an
        // extension remains pending until the complete composed prompt has
        // passed validation and can be submitted durably.
        let pending_count = self.pending_context.len();
        let mut context = PendingContext::default();
        for contribution in self.pending_context.iter().take(pending_count).cloned() {
            context
                .try_push(contribution)
                .expect("admitted pending extension context must remain valid");
        }
        let mut rejected_context = Vec::new();

        for process in &self.processes {
            let execution = extension_execution_context(process, self.resource_owner.as_deref());
            if process
                .contributions()
                .hooks
                .contains(&ExtensionHook::BeforePrompt)
            {
                let output = tokio::time::timeout(
                    PROMPT_RPC_DEADLINE,
                    process.run_hook(
                        ExtensionHook::BeforePrompt,
                        if process.supports_feature(octet_agent::extension_process::EXTENSION_FEATURE_BEFORE_PROMPT_STATE_V1) {
                            serde_json::json!({"prompt": &prompt, "system_prompt": &effective_system})
                        } else {
                            before_prompt_hook_payload(&prompt)
                        },
                        execution.clone(),
                    ),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "extension {:?} before_prompt hook exceeded {:?}",
                        process.descriptor().manifest.name,
                        PROMPT_RPC_DEADLINE,
                    )
                })?
                .with_context(|| {
                    format!(
                        "extension {:?} before_prompt hook failed",
                        process.descriptor().manifest.name
                    )
                })?;
                if let ExtensionHookDisposition::Deny { reason } = output.disposition {
                    anyhow::bail!(
                        "extension {:?} denied the prompt: {reason}",
                        process.descriptor().manifest.name
                    );
                }
                if let Some(system) = output.system_prompt {
                    anyhow::ensure!(process.supports_feature(octet_agent::extension_process::EXTENSION_FEATURE_BEFORE_PROMPT_STATE_V1),
                        "extension returned an unnegotiated before_prompt system replacement");
                    anyhow::ensure!(
                        system.len() <= 256 * 1024 && !system.contains('\0'),
                        "extension before_prompt system replacement exceeds bounds"
                    );
                    effective_system = system;
                }
                for message in output.custom_messages {
                    message.validate()?;
                    custom_messages.push(message);
                }
                let mut dropped = 0usize;
                let mut last_error = None;
                for contribution in output.context {
                    if let Err(error) = context.try_push(contribution) {
                        dropped = dropped.saturating_add(1);
                        last_error = Some(error);
                    }
                }
                if dropped > 0 {
                    rejected_context.push(format!(
                        "warning: extension {:?} dropped {dropped} before_prompt context contribution(s): {}",
                        process.descriptor().manifest.name,
                        last_error.unwrap_or_else(|| "context admission failed".into())
                    ));
                }
                notifications.extend(output.notifications.into_iter().map(|notification| {
                    format_notification(&process.descriptor().manifest.name, &notification)
                }));
            }
            if process.contributions().context {
                let collected = tokio::time::timeout(
                    PROMPT_RPC_DEADLINE,
                    process.collect_context(Some(prompt.clone()), execution),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "extension {:?} context collection exceeded {:?}",
                        process.descriptor().manifest.name,
                        PROMPT_RPC_DEADLINE,
                    )
                })?
                .with_context(|| {
                    format!(
                        "extension {:?} context collection failed",
                        process.descriptor().manifest.name
                    )
                })?;
                let mut dropped = 0usize;
                let mut last_error = None;
                for contribution in collected {
                    if let Err(error) = context.try_push(contribution) {
                        dropped = dropped.saturating_add(1);
                        last_error = Some(error);
                    }
                }
                if dropped > 0 {
                    rejected_context.push(format!(
                        "warning: extension {:?} dropped {dropped} collected context contribution(s): {}",
                        process.descriptor().manifest.name,
                        last_error.unwrap_or_else(|| "context admission failed".into())
                    ));
                }
            }
        }

        notifications.extend(rejected_context.iter().cloned());
        self.diagnostics.extend(rejected_context);
        let (system, prompt) = compose_context(&effective_system, prompt, context.into_vec())?;
        notifications.extend(self.drain_events());
        Ok(ExtensionPromptComposition {
            custom_messages,
            system,
            prompt,
            notifications,
            pending_context_count: pending_count,
        })
    }

    /// Commit the one-shot context captured by a successful prompt
    /// composition. Frontends call this only after the user message has been
    /// appended durably; preflight/append failures leave the context available
    /// for the restored draft's retry.
    pub fn commit_prompt_context(&mut self, pending_context_count: usize) {
        self.pending_context.commit(pending_context_count);
    }

    pub async fn after_response(&mut self, response: &str) -> Vec<String> {
        let mut messages = Vec::new();
        let mut queued_context = Vec::new();
        for process in &self.processes {
            if !process
                .contributions()
                .hooks
                .contains(&ExtensionHook::AfterResponse)
            {
                continue;
            }
            let execution = extension_execution_context(process, self.resource_owner.as_deref());
            match tokio::time::timeout(
                AFTER_RESPONSE_RPC_DEADLINE,
                process.run_hook(
                    ExtensionHook::AfterResponse,
                    after_response_hook_payload(response),
                    execution,
                ),
            )
            .await
            {
                Err(_) => messages.push(format!(
                    "extension {:?} after_response hook exceeded {:?}",
                    process.descriptor().manifest.name,
                    AFTER_RESPONSE_RPC_DEADLINE,
                )),
                Ok(Ok(output)) => {
                    let extension_name = process.descriptor().manifest.name.clone();
                    messages.extend(
                        output.notifications.into_iter().map(|notification| {
                            format_notification(&extension_name, &notification)
                        }),
                    );
                    queued_context.extend(
                        output
                            .context
                            .into_iter()
                            .map(|contribution| (extension_name.clone(), contribution)),
                    );
                }
                Ok(Err(error)) => messages.push(format!(
                    "extension {:?} after_response hook failed: {error}",
                    process.descriptor().manifest.name
                )),
            }
        }
        for (extension_name, contribution) in queued_context {
            self.enqueue_context(&extension_name, contribution);
        }
        messages.extend(self.drain_events());
        messages
    }
}
