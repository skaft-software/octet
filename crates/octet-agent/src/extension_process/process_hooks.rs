//! Agent hook and trait implementations backed by an extension process.

use super::*;

#[async_trait::async_trait]
impl ProviderRetryHook for ExtensionProcess {
    async fn provider_retry(&self, context: &ProviderRetryContext) -> ProviderRetryAdvice {
        if !is_stateful_api(self.api_version())
            || !self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::ProviderRetry)
        {
            return ProviderRetryAdvice::NoOpinion;
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let generation = connection.generation;
        let mut execution = self.execution_context();
        execution.resource_owner = Some(ExtensionResourceOwner {
            session_id: context.resource_owner.clone(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: generation,
        });
        let payload = serde_json::json!({
            "run_id": context.run_id,
            "attempt": context.attempt,
            "max_attempts": context.max_attempts,
            "operation": context.operation,
            "host_delay_ms": context.host_delay.as_millis(),
            "kind": match context.kind {
                crate::extension::ProviderRetryKind::BeforeGeneration => "before_generation",
                crate::extension::ProviderRetryKind::StreamStart => "stream_start",
                crate::extension::ProviderRetryKind::InterruptedInference => "interrupted_inference",
                crate::extension::ProviderRetryKind::WaitingForNetwork => "waiting_for_network",
            },
        });
        let Ok(output) = self
            .run_hook(ExtensionHook::ProviderRetry, payload, execution)
            .await
        else {
            return ProviderRetryAdvice::NoOpinion;
        };
        if read_std_lock(&self.inner.connection).generation != generation {
            return ProviderRetryAdvice::NoOpinion;
        }
        match output.provider_retry {
            Some(ExtensionProviderRetryAdvice::Retry) => ProviderRetryAdvice::Retry,
            Some(ExtensionProviderRetryAdvice::Delay {
                additional_delay_ms,
            }) => ProviderRetryAdvice::Delay {
                additional: Duration::from_millis(additional_delay_ms),
            },
            Some(ExtensionProviderRetryAdvice::Stop) => ProviderRetryAdvice::Stop,
            None => ProviderRetryAdvice::NoOpinion,
        }
    }
}

#[async_trait::async_trait]
impl PersistenceMetadataHook for ExtensionProcess {
    async fn before_assistant_persist(
        &self,
        context: &AssistantPersistenceContext,
    ) -> Option<PersistenceMetadataProposal> {
        if !is_stateful_api(self.api_version())
            || !self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::BeforePersistence)
        {
            return None;
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let generation = connection.generation;
        let mut execution = self.execution_context();
        execution.resource_owner = Some(ExtensionResourceOwner {
            session_id: context.resource_owner.clone(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: generation,
        });
        let payload = serde_json::json!({
            "run_id": context.run_id,
            "model": context.model,
            "protocol": context.protocol,
            "stop_reason": context.stop_reason,
            "text_bytes": context.text_bytes,
            "tool_call_count": context.tool_call_count,
            "reasoning_part_count": context.reasoning_part_count,
            "media_part_count": context.media_part_count,
        });
        let output = self
            .run_hook(ExtensionHook::BeforePersistence, payload, execution)
            .await
            .ok()?;
        if read_std_lock(&self.inner.connection).generation != generation {
            return None;
        }
        let metadata = output.persistence_metadata?;
        Some(PersistenceMetadataProposal::from_process(
            metadata.public,
            metadata.value,
            generation,
        ))
    }
}

#[async_trait::async_trait]
impl CompactionStrategy for ExtensionProcess {
    async fn render(
        &self,
        model_id: &str,
        text: &str,
        owner: &str,
    ) -> Result<Vec<Vec<u8>>, String> {
        if self.api_version() != EXTENSION_API_VERSION_0_4
            || !self.supports_feature(EXTENSION_FEATURE_COMPACTION_STRATEGY)
        {
            return Err("compaction_strategy was not negotiated".into());
        }
        let generation = read_std_lock(&self.inner.connection).generation;
        let mut context = self.execution_context();
        context.resource_owner = Some(ExtensionResourceOwner {
            session_id: owner.to_owned(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: generation,
        });
        let output = self
            .run_hook(
                ExtensionHook::CompactionStrategy,
                serde_json::json!({ "model_id": model_id, "text": text }),
                context,
            )
            .await
            .map_err(|error| error.to_string())?;
        if read_std_lock(&self.inner.connection).generation != generation {
            return Err("compaction strategy changed generation during render".into());
        }
        let encoded = output
            .compaction_frames
            .ok_or("compaction strategy returned no frames")?;
        if encoded.is_empty() || encoded.len() > 32 {
            return Err("compaction strategy returned an invalid frame count".into());
        }
        let mut frames = Vec::with_capacity(encoded.len());
        for item in encoded {
            if item.len() > 512 * 1024 {
                return Err("compaction frame exceeds 512 KiB".into());
            }
            frames.push(
                base64::engine::general_purpose::STANDARD
                    .decode(item)
                    .map_err(|_| "compaction frame is not base64")?,
            );
        }
        Ok(frames)
    }
}

impl Extension for ExtensionProcess {
    fn register(&self, host: &mut ExtensionHost) {
        self.register_dynamic_tool_catalog(host);
        host.observe(self.clone());
        if self.api_version() == EXTENSION_API_VERSION_0_4
            && self.supports_feature(EXTENSION_FEATURE_COMPACTION_STRATEGY)
            && self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::CompactionStrategy)
        {
            host.compaction_strategy(self.clone());
        }
        if self.inner.contributions.hooks.iter().any(|hook| {
            matches!(
                hook,
                ExtensionHook::BeforeToolCall | ExtensionHook::AfterToolCall
            )
        }) {
            host.tool_call_hook(self.clone());
        }
        if uses_api_0_2_capabilities(self.api_version())
            && self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::ProviderRetry)
        {
            host.provider_retry_hook(self.clone());
        }
        if uses_api_0_2_capabilities(self.api_version())
            && self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::BeforePersistence)
        {
            host.persistence_metadata_hook(
                self.inner.descriptor.manifest.name.clone(),
                self.clone(),
            );
        }
    }
}

impl ExtensionProcess {
    /// Removes this process's live tool group from its current host.
    ///
    /// A host-level runtime manager calls this at an idle App/session rebuild
    /// before attaching the still-running process to the replacement host. It
    /// never stops the process and therefore cannot cancel independently owned
    /// workspace-service work.
    pub fn detach_dynamic_tool_catalog(&self) {
        if let Some(registration) = lock_std_mutex(&self.inner.dynamic_tool_registration).take() {
            registration.remove();
        }
    }

    /// Attaches the live tool catalog before the process's ordered observer and
    /// hook registration. Product startup uses this narrow first phase so a
    /// fast child can publish while a slower sibling is still initializing;
    /// the later full [`Extension`] registration remains deterministic.
    pub fn register_dynamic_tool_catalog(&self, host: &mut ExtensionHost) {
        if lock_std_mutex(&self.inner.dynamic_tool_registration).is_some() {
            return;
        }
        let definitions = self.tool_definitions();
        let owner = format!(
            "{}@{}",
            self.inner.descriptor.manifest.name,
            self.inner.descriptor.manifest_path.display()
        );
        let connection = read_std_lock(&self.inner.connection).clone();
        let process_tools = self.process_tools(Arc::clone(&connection), &definitions);
        let published_connection = Arc::clone(&connection);
        match host.dynamic_tools_with(owner, process_tools.tools, move |_, published| {
            let _catalog = write_std_lock(&published_connection.catalog_guard);
            write_std_lock(&published_connection.tool_catalog)
                .retain(|definition| published.contains(&definition.name));
            published_connection
                .catalog_revision
                .store(0, Ordering::Release);
        }) {
            Ok(registration) => {
                *lock_std_mutex(&self.inner.dynamic_tool_registration) = Some(registration);
                self.inner.dynamic_tool_registration_ready.notify_waiters();
            }
            Err(error) => {
                host.duplicate_tools.push(error);
            }
        }
    }
}

impl ExtensionProcess {
    pub(super) fn observe_agent_event(&self, event: &AgentEvent, resource_owner: Option<&str>) {
        match event {
            AgentEvent::ToolStarted { id, name, .. } => {
                let mut lifecycle = lock_std_mutex(&self.inner.lifecycle);
                let owner = resource_owner.map(str::to_owned).or_else(|| {
                    (lifecycle.turns.len() == 1)
                        .then(|| lifecycle.turns.keys().next().cloned())
                        .flatten()
                });
                let Some(owner) = owner else {
                    return;
                };
                let Some(turn) = lifecycle.turns.get(&owner).cloned() else {
                    return;
                };
                let active = ActiveLifecycleTool {
                    name: name.clone(),
                    started_at: Instant::now(),
                    context: turn.context.clone(),
                    endpoint: turn.endpoint.clone(),
                };
                let _ = Self::queue_lifecycle_observation(
                    &active.endpoint,
                    ExtensionLifecycleEvent::ToolStarted {
                        session_id: active.context.session_id.clone(),
                        run_id: active.context.run_id.clone(),
                        turn_id: active.context.turn_id.clone(),
                        tool_call_id: id.0.clone(),
                        tool_name: name.clone(),
                    },
                );
                lifecycle.tools.insert((owner, id.0.clone()), active);
            }
            AgentEvent::ToolFinished { id, result, .. } => {
                let mut lifecycle = lock_std_mutex(&self.inner.lifecycle);
                let owner = resource_owner.map(str::to_owned).or_else(|| {
                    let mut owners = lifecycle
                        .tools
                        .keys()
                        .filter(|(_, tool_call_id)| tool_call_id == &id.0)
                        .map(|(owner, _)| owner.clone());
                    let first = owners.next();
                    (owners.next().is_none()).then_some(first).flatten()
                });
                let Some(owner) = owner else {
                    return;
                };
                let active = lifecycle.tools.remove(&(owner, id.0.clone()));
                let Some(active) = active else {
                    return;
                };
                let (outcome, reason) = match result {
                    Ok(output) if output.is_error() => {
                        (ExtensionLifecycleOutcome::Failed, Some(output.text.clone()))
                    }
                    Ok(_) => (ExtensionLifecycleOutcome::Completed, None),
                    Err(error) => (
                        ExtensionLifecycleOutcome::Failed,
                        Some(error.message.clone()),
                    ),
                };
                let reason = reason.map(|mut reason| {
                    truncate_utf8(&mut reason, MAX_LIFECYCLE_REASON_BYTES);
                    reason
                });
                let duration_ms =
                    u64::try_from(active.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                let _ = Self::queue_lifecycle_observation(
                    &active.endpoint,
                    ExtensionLifecycleEvent::ToolSettled {
                        session_id: active.context.session_id,
                        run_id: active.context.run_id,
                        turn_id: active.context.turn_id,
                        tool_call_id: id.0.clone(),
                        tool_name: active.name,
                        outcome,
                        duration_ms,
                        reason,
                    },
                );
            }
            _ => {}
        }
    }
}

impl EventObserver for ExtensionProcess {
    fn on_event(&self, event: &AgentEvent) {
        self.observe_agent_event(event, None);
    }

    fn on_event_for_owner(&self, event: &AgentEvent, resource_owner: &str) {
        self.observe_agent_event(event, Some(resource_owner));
    }
}

#[async_trait::async_trait]
impl ToolCallHook for ExtensionProcess {
    async fn before_tool_call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        context: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        if !self
            .inner
            .contributions
            .hooks
            .contains(&ExtensionHook::BeforeToolCall)
        {
            return Ok(());
        }
        let output = self
            .run_hook(
                ExtensionHook::BeforeToolCall,
                serde_json::json!({ "name": name, "arguments": arguments }),
                self.tool_execution_context(context, {
                    let connection = read_std_lock(&self.inner.connection);
                    connection.generation
                }),
            )
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        self.publish_hook_output(&output);
        match output.disposition {
            ExtensionHookDisposition::Continue => Ok(()),
            ExtensionHookDisposition::Deny { reason } => Err(ToolError::new(format!(
                "extension `{}` denied tool `{name}`: {reason}",
                self.inner.descriptor.manifest.name
            ))),
        }
    }

    async fn after_tool_call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        output: &str,
        is_error: bool,
        context: &ToolContext<'_>,
    ) {
        if !self
            .inner
            .contributions
            .hooks
            .contains(&ExtensionHook::AfterToolCall)
        {
            return;
        }
        match self
            .run_hook(
                ExtensionHook::AfterToolCall,
                serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                    "output": output,
                    "is_error": is_error,
                }),
                self.tool_execution_context(context, {
                    let connection = read_std_lock(&self.inner.connection);
                    connection.generation
                }),
            )
            .await
        {
            Ok(output) => self.publish_hook_output(&output),
            Err(error) => {
                let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                    message: format!("after_tool_call hook failed: {error}"),
                });
            }
        }
    }
}

impl ExtensionProcess {
    pub(super) fn tool_execution_context(
        &self,
        context: &ToolContext<'_>,
        process_generation: u64,
    ) -> ExtensionExecutionContext {
        let mut execution = self.execution_context();
        execution.execution_scope = Some(context.execution_scope.to_owned());
        if uses_api_0_2_capabilities(self.api_version()) {
            execution.resource_owner = Some(ExtensionResourceOwner {
                session_id: context.resource_owner.to_owned(),
                extension_instance_id: self.inner.instance_id.clone(),
                process_generation,
            });
        }
        execution.host.active_skills = context
            .active_skills
            .iter()
            .map(|skill| ExtensionActiveSkill {
                id: skill.descriptor.id.clone(),
                name: skill.descriptor.name.clone(),
                version: skill.descriptor.version.clone(),
            })
            .collect();
        execution
    }

    pub(super) fn publish_hook_output(&self, output: &ExtensionHookOutput) {
        for notification in &output.notifications {
            let _ = self.inner.events.send(ExtensionEvent::Notification {
                notification: notification.clone(),
            });
        }
        for contribution in &output.context {
            let _ = self.inner.events.send(ExtensionEvent::ContextContributed {
                contribution: contribution.clone(),
            });
        }
    }

    pub(super) fn process_tools(
        &self,
        connection: Arc<ProcessConnection>,
        definitions: &[ToolDefinition],
    ) -> ProcessToolSet {
        let revision = Arc::new(AtomicU64::new(0));
        let tools = definitions
            .iter()
            .cloned()
            .map(|definition| {
                Arc::new(ProcessTool {
                    process: self.clone(),
                    connection: Arc::clone(&connection),
                    definition,
                    catalog_revision: Arc::clone(&revision),
                }) as Arc<dyn Tool>
            })
            .collect();
        ProcessToolSet { tools, revision }
    }
}

pub(super) async fn wait_for_dynamic_registration(
    inner: &ExtensionProcessInner,
) -> Result<DynamicToolRegistration, String> {
    let timeout = inner.config.request_timeout;
    let registration = tokio::time::timeout(timeout, async {
        loop {
            let notified = inner.dynamic_tool_registration_ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(registration) = lock_std_mutex(&inner.dynamic_tool_registration).clone() {
                return registration;
            }
            notified.await;
        }
    })
    .await
    .map_err(|_| "dynamic tool process was not registered with its host in time".to_owned())?;
    registration.wait_until_ready(timeout).await?;
    Ok(registration)
}
