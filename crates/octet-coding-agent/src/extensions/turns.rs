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
    Transform {
        text: String,
        #[serde(default)]
        images: Option<Vec<octet_ai::Media>>,
    },
}

pub(crate) struct AfterResponseOutput {
    messages: Vec<String>,
    queued_context: Vec<(String, ContextContribution)>,
}

/// Opaque evidence that the current session view was prepared by every live,
/// owner-routed process in the captured extension fleet.
pub(crate) struct PreparedSessionSnapshot {
    session_owner: String,
    process_owners: Vec<octet_agent::extension_process::ExtensionResourceOwner>,
}

const SESSION_SNAPSHOT_READY_DEADLINE: Duration = Duration::from_secs(5);

/// A one-line explanation when a prompt had to wait for an extension that was
/// still starting its session, so a visible pause is never unexplained.
fn waited_note(process: &ExtensionProcess, elapsed: Duration) -> Option<String> {
    (elapsed >= Duration::from_secs(1)).then(|| {
        format!(
            "note: extension {:?} was still starting; this prompt waited {:.1}s for it",
            process.descriptor().manifest.name,
            elapsed.as_secs_f32()
        )
    })
}

/// Isolated processes receive the host facts with the native session view;
/// shared workspace services never hold one session's history.
fn publish_host_state<'a>(
    processes: impl IntoIterator<Item = &'a ExtensionProcess>,
    state: &ExtensionHostState,
    session: &Session,
    diagnostics: &mut BoundedDiagnostics,
) {
    for process in processes {
        if process.descriptor().manifest.runtime.sharing
            == octet_agent::extension_process::ExtensionRuntimeSharing::Isolated
        {
            if let Err(error) = process.set_host_state_with_session(state.clone(), session) {
                diagnostics.push(format!(
                    "warning: extension {:?} session mirror unavailable: {error}",
                    process.descriptor().manifest.name,
                ));
            }
        }
    }
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
        anyhow::ensure!(
            ["interactive", "rpc", "extension"].contains(&source),
            "invalid input source"
        );
        anyhow::ensure!(
            streaming_behavior.is_none_or(|value| ["steer", "followUp"].contains(&value)),
            "invalid input delivery"
        );
        let mut input = ExtensionInput {
            text,
            images,
            transformed: false,
        };
        for process in self.processes.clone() {
            if !process.supports_feature("input_transform_v1")
                || !process
                    .contributions()
                    .hooks
                    .contains(&ExtensionHook::BeforePrompt)
            {
                continue;
            }
            let mut waited = Vec::new();
            let started = std::time::Instant::now();
            let idle = self
                .await_prompt_work(process.wait_until_idle(SESSION_START_WAIT), &mut waited)
                .await;
            // Notices drained while waiting belong to the transcript: keep them
            // for the next drain instead of dropping them with this prompt.
            self.held_messages.append(&mut waited);
            if idle {
                if let Some(note) = waited_note(&process, started.elapsed()) {
                    self.held_messages.push(note);
                }
            }
            if !idle {
                self.diagnostics.push(format!(
                    "warning: extension {:?} is still starting; its input hook was skipped for this prompt",
                    process.descriptor().manifest.name
                ));
                continue;
            }
            let output = tokio::time::timeout(
                PROMPT_RPC_DEADLINE,
                process.run_hook(
                    ExtensionHook::BeforePrompt,
                    serde_json::json!({"phase":"input", "text":input.text, "images":input.images,
                    "source":source, "streaming_behavior":streaming_behavior}),
                    extension_execution_context(&process, self.resource_owner.as_deref()),
                ),
            )
            .await
            .map_err(|_| anyhow::anyhow!("extension input hook timed out"))??;
            anyhow::ensure!(
                output.disposition == ExtensionHookDisposition::Continue,
                "extension input hook refused input"
            );
            let result = output
                .input_event
                .map(serde_json::from_value::<InputEventResult>)
                .transpose()?;
            match result.unwrap_or(InputEventResult::Continue) {
                InputEventResult::Continue => {}
                InputEventResult::Handled => return Ok(None),
                InputEventResult::Transform { text, images } => {
                    anyhow::ensure!(
                        text.len() <= 262144 && !text.contains('\0'),
                        "transformed input exceeds bounds"
                    );
                    if let Some(images) = images {
                        anyhow::ensure!(
                            images.len() <= 256,
                            "transformed input image count exceeds bounds"
                        );
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

    /// Test helper for lifecycle-only tests. Production replacements must use
    /// the prepared-snapshot release methods below.
    #[cfg(test)]
    pub fn transition_active_session(
        &mut self,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
    ) {
        self.transition_active_session_with_setup(
            session, model, reasoning, sessions, "resume", None,
        );
        self.start_session_lifecycle();
    }

    pub fn transition_active_session_with_setup(
        &mut self,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        reason: &'static str,
        setup: Option<(u64, octet_agent::extension_process::ExtensionResourceOwner)>,
    ) {
        self.session_start_reason = reason;
        self.pending_session_setup = setup;
        self.deactivate_session_lifecycle_driver();
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
        // Setup requests are the only lifecycle work allowed before the final
        // snapshot is prepared. Ordinary lifecycle and deferred start hooks are
        // released by start_session_lifecycle_after_snapshot.
        if self.pending_session_setup.is_some() {
            self.activate_session_lifecycle_driver();
        }
    }

    /// Validate every setup mutation against its original parent and live owner.
    pub fn session_setup_is_current(
        &self,
        parent: u64,
        owner: &octet_agent::extension_process::ExtensionResourceOwner,
    ) -> bool {
        self.resource_owner.as_deref() == Some(owner.session_id.as_str())
            && self
                .pending_session_setup
                .as_ref()
                .is_some_and(|(id, admitted)| {
                    *id == parent
                        && admitted.extension_instance_id == owner.extension_instance_id
                        && admitted.process_generation == owner.process_generation
                })
    }

    /// Capture and prepare the current host session view without borrowing the
    /// frontend. The caller supplies the real request cancellation token; all
    /// stateful processes share one bounded deadline.
    pub(crate) fn wait_for_active_session_snapshot_prepared(
        &self,
        cancellation: CancellationToken,
    ) -> impl std::future::Future<Output = anyhow::Result<PreparedSessionSnapshot>> + Send + 'static
    {
        let session_owner = self.resource_owner.clone();
        let routed_processes = session_owner
            .as_deref()
            .map(|owner| {
                self.processes
                    .iter()
                    .filter_map(|process| {
                        process
                            .current_context_for_resource_owner(owner.to_owned())
                            .resource_owner
                            .map(|owner| (process.clone(), owner))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        async move {
            let session_owner = session_owner
                .ok_or_else(|| anyhow::anyhow!("active session owner is unavailable"))?;
            anyhow::ensure!(
                !cancellation.is_cancelled(),
                "session snapshot readiness cancelled"
            );
            let deadline = Instant::now() + SESSION_SNAPSHOT_READY_DEADLINE;
            let mut process_owners = Vec::with_capacity(routed_processes.len());
            for (process, owner) in &routed_processes {
                anyhow::ensure!(
                    !cancellation.is_cancelled(),
                    "session snapshot readiness cancelled"
                );
                let remaining = deadline.saturating_duration_since(Instant::now());
                anyhow::ensure!(
                    !remaining.is_zero(),
                    "session snapshot readiness exceeded {SESSION_SNAPSHOT_READY_DEADLINE:?}"
                );
                process
                    .wait_for_session_snapshot_prepared(owner, remaining, &cancellation)
                    .await
                    .with_context(|| {
                        format!(
                            "extension {:?} session snapshot was not prepared",
                            process.descriptor().manifest.name
                        )
                    })?;
                process_owners.push(owner.clone());
            }
            anyhow::ensure!(
                !cancellation.is_cancelled(),
                "session snapshot readiness cancelled"
            );
            let current_owners = current_session_snapshot_owners(
                routed_processes.iter().map(|(process, _)| process.clone()),
                &session_owner,
            );
            anyhow::ensure!(
                current_owners == process_owners,
                "session snapshot owner or process generation changed while waiting"
            );
            Ok(PreparedSessionSnapshot {
                session_owner,
                process_owners,
            })
        }
    }

    /// Release ordinary replacement lifecycle only after the exact prepared
    /// owner/generation set is still current.
    pub(crate) fn start_session_lifecycle_after_snapshot(
        &mut self,
        prepared: PreparedSessionSnapshot,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.pending_session_setup.is_none(),
            "session lifecycle cannot start before setup completes"
        );
        anyhow::ensure!(
            self.prepared_session_snapshot_is_current(&prepared),
            "prepared session snapshot owner or process generation is no longer current"
        );
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "session lifecycle release cancelled"
        );
        self.start_session_lifecycle();
        self.activate_session_lifecycle_driver();
        Ok(())
    }

    /// Clear the setup fence and release deferred session_start only after the
    /// final setup mutation's exact snapshot is still current.
    pub(crate) fn complete_session_setup_after_snapshot(
        &mut self,
        parent: u64,
        owner: &octet_agent::extension_process::ExtensionResourceOwner,
        prepared: PreparedSessionSnapshot,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<Vec<(ExtensionProcess, String, &'static str)>> {
        anyhow::ensure!(
            self.session_setup_is_current(parent, owner),
            "session setup parent or owner is no longer current"
        );
        anyhow::ensure!(
            prepared.session_owner == owner.session_id
                && prepared.process_owners.contains(owner)
                && self.prepared_session_snapshot_is_current(&prepared),
            "prepared setup snapshot owner or process generation is no longer current"
        );
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "session setup completion cancelled"
        );
        self.pending_session_setup = None;
        self.start_session_lifecycle();
        self.activate_session_lifecycle_driver();
        Ok(std::mem::take(&mut self.pending_session_hook_starts)
            .into_iter()
            .map(|(process, owner)| (process, owner, self.session_start_reason))
            .collect())
    }

    fn prepared_session_snapshot_is_current(&self, prepared: &PreparedSessionSnapshot) -> bool {
        self.resource_owner.as_deref() == Some(prepared.session_owner.as_str())
            && current_session_snapshot_owners(
                self.processes.iter().cloned(),
                &prepared.session_owner,
            ) == prepared.process_owners
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
            && initial.reasoning
                == pi_thinking_level(model, reasoning).map(serde_json::Value::String)
            && initial.context_usage
                == Some(model_control::context_usage(session, model).unwrap_or(Value::Null));
        drop(initial);
        if !unchanged {
            self.refresh_host_state(session, model, reasoning, sessions);
        }
    }

    /// Publish the actual shell manager, including user overrides, to retained UI.
    pub(crate) fn refresh_frontend_keybindings(&self, shell: &InteractiveShell) {
        let keybindings = Some(shell.extension_keybindings());
        let state = {
            let mut state = self
                .host_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.keybindings == keybindings {
                return;
            }
            state.keybindings = keybindings;
            state.clone()
        };
        for process in &self.processes {
            if process.descriptor().manifest.runtime.sharing
                == octet_agent::extension_process::ExtensionRuntimeSharing::Isolated
            {
                process.set_host_state(state.clone());
            }
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
        {
            let previous = self
                .host_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.pi_models = previous.pi_models.clone();
            state.project_trusted = previous.project_trusted;
            state.has_ui = previous.has_ui;
            state.keybindings = previous.keybindings.clone();
            state.theme = previous.theme.clone();
            state.mode = previous.mode.clone();
        }
        self.session_id = state.session_id.clone();
        publish_host_state(&self.processes, &state, session, &mut self.diagnostics);
        // Every session or model boundary refreshes this cache, so a read-only
        // context snapshot never answers from the process-startup projection.
        *self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
    }

    /// Publish the current host facts and native session view to an activation
    /// batch before it is installed. Installing starts each process's session
    /// lifecycle, and a Pi session_start handler may read the session there.
    pub(super) fn publish_host_state_to_starts(
        &mut self,
        starts: &[(String, Result<ExtensionProcess, String>)],
        session: &Session,
    ) {
        let state = self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let processes = starts.iter().filter_map(|(_, start)| start.as_ref().ok());
        publish_host_state(processes, &state, session, &mut self.diagnostics);
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

    /// Keep reverse host requests live while a prompt contributor is awaiting
    /// its response. This drain never borrows a shell: an interactive consumer
    /// queues those requests for its next shell pump, and a host without one
    /// refuses them.
    async fn await_prompt_work<F: std::future::Future>(
        &mut self,
        work: F,
        notifications: &mut Vec<String>,
    ) -> F::Output {
        tokio::pin!(work);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                result = &mut work => return result,
                _ = tick.tick() => notifications.extend(self.drain_events()),
            }
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

        // Process handles are shared; release the fleet borrow so the same
        // bounded event consumer can answer requests made by these hooks.
        for process in self.processes.clone() {
            let execution = extension_execution_context(&process, self.resource_owner.as_deref());
            // A session start still in flight owns the extension's ordered hook
            // queue; wait for it instead of failing the prompt against a deadline
            // that would include that wait.
            if process
                .contributions()
                .hooks
                .contains(&ExtensionHook::BeforePrompt)
                || process.contributions().context
            {
                let started = std::time::Instant::now();
                if self
                    .await_prompt_work(
                        process.wait_until_idle(SESSION_START_WAIT),
                        &mut notifications,
                    )
                    .await
                {
                    notifications.extend(waited_note(&process, started.elapsed()));
                } else {
                    notifications.push(format!(
                        "warning: extension {:?} is still starting; continuing without it for this prompt",
                        process.descriptor().manifest.name
                    ));
                    continue;
                }
            }

            if process
                .contributions()
                .hooks
                .contains(&ExtensionHook::BeforePrompt)
            {
                let output = self.await_prompt_work(
                    tokio::time::timeout(
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
                    ),
                    &mut notifications,
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
                let collected = self
                    .await_prompt_work(
                        tokio::time::timeout(
                            PROMPT_RPC_DEADLINE,
                            process.collect_context(Some(prompt.clone()), execution),
                        ),
                        &mut notifications,
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

    /// Capture ordered hook work without borrowing the frontend consumer.
    pub(crate) fn begin_after_response(
        &self,
        response: &str,
    ) -> impl std::future::Future<Output = AfterResponseOutput> + Send + 'static {
        let processes = self.processes.clone();
        let resource_owner = self.resource_owner.clone();
        let response = response.to_owned();
        async move {
            let mut messages = Vec::new();
            let mut queued_context = Vec::new();
            for process in &processes {
                if !process
                    .contributions()
                    .hooks
                    .contains(&ExtensionHook::AfterResponse)
                {
                    continue;
                }
                let execution = extension_execution_context(process, resource_owner.as_deref());
                match tokio::time::timeout(
                    AFTER_RESPONSE_RPC_DEADLINE,
                    process.run_hook(
                        ExtensionHook::AfterResponse,
                        after_response_hook_payload(&response),
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
                        messages.extend(output.notifications.into_iter().map(|notification| {
                            format_notification(&extension_name, &notification)
                        }));
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
            AfterResponseOutput {
                messages,
                queued_context,
            }
        }
    }

    /// Publish the contributions only after the complete hook boundary settles.
    pub(crate) fn finish_after_response(&mut self, output: AfterResponseOutput) -> Vec<String> {
        for (extension_name, contribution) in output.queued_context {
            self.enqueue_context(&extension_name, contribution);
        }
        output.messages
    }

    pub async fn after_response(&mut self, response: &str) -> Vec<String> {
        let output = self.begin_after_response(response).await;
        let mut messages = self.finish_after_response(output);
        messages.extend(self.drain_events());
        messages
    }
}

fn current_session_snapshot_owners(
    processes: impl IntoIterator<Item = ExtensionProcess>,
    session_owner: &str,
) -> Vec<octet_agent::extension_process::ExtensionResourceOwner> {
    processes
        .into_iter()
        .filter_map(|process| {
            process
                .current_context_for_resource_owner(session_owner.to_owned())
                .resource_owner
        })
        .collect()
}

#[cfg(test)]
mod keybinding_snapshot_tests {
    use super::*;

    #[test]
    fn host_refresh_preserves_native_frontend_keybinding_overrides() {
        let (_directory, app) = crate::compaction::tests::app_for_estimate();
        let mut extensions = ExecutableExtensions::default();
        let keys = BTreeMap::from([("app.tools.expand".to_owned(), vec!["ctrl+x".to_owned()])]);
        {
            let mut state = extensions.host_state.lock().unwrap();
            state.has_ui = Some(true);
            state.keybindings = Some(keys.clone());
        }
        extensions.refresh_host_state(
            app.agent.session(),
            &app.model,
            &app.reasoning,
            &app.sessions,
        );
        assert_eq!(
            extensions.host_state.lock().unwrap().keybindings,
            Some(keys)
        );
    }
}
