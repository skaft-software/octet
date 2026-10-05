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

// This local cap is further shortened by the Agent's aggregate hook/refresh
// deadline. Advice never receives its own extension of a refresh deadline.
const CACHE_WARMING_DECISION_TIMEOUT: Duration = Duration::from_millis(200);

#[async_trait::async_trait]
impl CacheWarmingDecisionHook for ExtensionProcess {
    async fn cache_warming_decision(
        &self,
        context: &CacheWarmingDecisionContext,
    ) -> Option<CacheWarmingAction> {
        if self.api_version() != EXTENSION_API_VERSION_0_4
            || !self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::CacheWarmingDecision)
        {
            return None;
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        if !read_std_lock(&connection.protocol)
            .features
            .contains(EXTENSION_FEATURE_CACHE_WARMING_DECISION)
        {
            return None;
        }
        let generation = connection.generation;
        let mut execution = self.execution_context();
        execution.resource_owner = Some(ExtensionResourceOwner {
            session_id: context.resource_owner.clone(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: generation,
        });
        let resource_owner = execution.resource_owner.clone();
        let decision = serde_json::to_value(&context.decision).ok()?;
        let params = serde_json::to_value(HookRequest {
            hook: ExtensionHook::CacheWarmingDecision,
            payload: serde_json::json!({"decision": decision, "model": context.model}),
            context: execution,
        })
        .ok()?;
        let response = connection
            .request_with_resource_owner(
                methods::HOOK_RUN,
                params,
                self.inner
                    .config
                    .request_timeout
                    .min(CACHE_WARMING_DECISION_TIMEOUT),
                resource_owner,
            )
            .await
            .ok()?;
        // Keep arbitrary remote errors and malformed response values out of
        // diagnostics, and discard advice from a replaced process generation.
        let output: ExtensionHookOutput = serde_json::from_value(response).ok()?;
        if read_std_lock(&self.inner.connection).generation != generation {
            return None;
        }
        output.cache_warming_decision
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
            && self.supports_feature(EXTENSION_FEATURE_SESSION_ENTRIES)
            && self
                .inner
                .contributions
                .hooks
                .iter()
                .any(|hook| hook.is_session_operation())
        {
            host.session_operation_hook(self.clone());
        }
        if self.has_provider_pipeline_hooks() {
            host.provider_request_hook(self.clone());
        }
        if self.api_version() == EXTENSION_API_VERSION_0_4
            && self.supports_feature(EXTENSION_FEATURE_SESSION_ENTRIES)
            && self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::ProviderContext)
        {
            host.provider_context_hook(self.clone());
        }
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
        if self.api_version() == EXTENSION_API_VERSION_0_4
            && self.supports_feature(EXTENSION_FEATURE_CACHE_WARMING_DECISION)
            && self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::CacheWarmingDecision)
        {
            host.cache_warming_decision_hook(self.clone());
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
                if read_std_lock(&connection.protocol).supports(EXTENSION_FEATURE_RESOURCE_REFS_V1)
                {
                    registration.resource_source(self.clone());
                    if read_std_lock(&connection.protocol)
                        .supports(EXTENSION_FEATURE_OPERATION_DESCRIPTORS_V1)
                    {
                        host.enable_operation_discovery();
                    }
                }
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

impl ExtensionProcess {
    async fn run_before_tool_call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        context: &ToolContext<'_>,
    ) -> Result<Option<serde_json::Value>, ToolError> {
        let output = self
            .run_hook(
                ExtensionHook::BeforeToolCall,
                serde_json::json!({ "name": name, "arguments": arguments,
                    "tool_call_id": context.progress.tool_call_identity().0,
                    "parent_tool_call_id": context.progress.tool_call_identity().1 }),
                self.tool_execution_context(context, {
                    let connection = read_std_lock(&self.inner.connection);
                    connection.generation
                }),
            )
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        self.publish_hook_output(&output);
        if let ExtensionHookDisposition::Deny { reason } = output.disposition {
            let error = ToolError::policy_denied(
                crate::effect::ToolPolicyDenialCode::SecondaryHookDenied,
                reason.clone(),
            );
            return Err(if output.terminate == Some(true) {
                error.with_output(ToolOutput::new(reason).requesting_termination())
            } else { error });
        }
        Ok(output.arguments)
    }

    fn has_hook(&self, hook: ExtensionHook) -> bool {
        self.inner.contributions.hooks.contains(&hook)
    }

    async fn run_after_tool_call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        result: &Result<ToolOutput, ToolError>,
        context: &ToolContext<'_>,
    ) -> Option<ExtensionHookOutput> {
        let mut payload = serde_json::json!({ "name": name, "arguments": arguments,
            "tool_call_id": context.progress.tool_call_identity().0,
            "parent_tool_call_id": context.progress.tool_call_identity().1 });
        let output = match result { Ok(output) => Some(output), Err(error) => error.output() };
        payload["output"] = match result {
            Ok(output) => output.text.clone(), Err(error) => error.message.clone(),
        }.into();
        payload["is_error"] = (result.is_err() || output.is_some_and(ToolOutput::is_error)).into();
        if let Some(output) = output {
            let mut content = Vec::new();
            for part in output.content_parts() {
                match part {
                    ToolOutputContentPart::Text(text) => content.push(serde_json::json!({"type":"text", "text":text})),
                    ToolOutputContentPart::Media(Media::Image(image)) => {
                        // Pi ImageContent is inline bytes, never a URL or a local path.
                        let (octet_ai::ImageSource::Inline(data), Some(mime)) = (&image.source, &image.media_type) else {
                            let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                                message: "tool_result cannot map a non-inline image to Pi ImageContent".into(),
                            });
                            return None;
                        };
                        content.push(serde_json::json!({"type":"image", "mimeType":mime.to_string(),
                            "data":base64::engine::general_purpose::STANDARD.encode(data)}));
                    }
                    ToolOutputContentPart::Media(Media::Audio(_)) => {
                        let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                            message: "Pi tool_result does not support audio content".into(),
                        });
                        return None;
                    }
                }
            }
            payload["pi_content"] = content.into();
            if let Some(value) = output.structured_content() { payload["structured_content"] = value.clone(); }
            if let Some(value) = output.metadata() { payload["metadata"] = value.clone(); }
            if let Some(value) = output.usage() { payload["usage"] = serde_json::to_value(value).expect("Usage serializes"); }
        }
        let context = self.tool_execution_context(context, {
            let connection = read_std_lock(&self.inner.connection);
            connection.generation
        });
        match self
            .run_hook(ExtensionHook::AfterToolCall, payload, context)
            .await
        {
            Ok(output) => {
                self.publish_hook_output(&output);
                Some(output)
            }
            Err(error) => {
                let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                    message: format!("after_tool_call hook failed: {error}"),
                });
                None
            }
        }
    }
}

/// A policy denial retains its typed fact and rich error envelope.
#[allow(clippy::result_large_err)]
fn replace_tool_result(
    result: Result<ToolOutput, ToolError>,
    replacement: ExtensionToolResultReplacement,
    content: Option<Vec<ToolOutputContentPart>>,
) -> Result<
    Result<ToolOutput, ToolError>,
    (Result<ToolOutput, ToolError>, crate::ToolOutputValidationError),
> {
    let original = result.clone();
    let ExtensionToolResultReplacement {
        content: _, structured_content, metadata, is_error, usage,
    } = replacement;
    let (output, denial) = match result {
        Err(error) => {
            let output = error.output().cloned().unwrap_or_else(|| ToolOutput::new(error.message.clone()).with_is_error(true));
            let denial = error.policy_denial_code().is_some().then_some(error);
            (output, denial)
        }
        Ok(output) => (output, None),
    };
    let structured_content = match (structured_content, &content) {
        (Some(value), _) => Some(value),
        (None, Some(_)) => None,
        (None, None) => output.structured_content().cloned(),
    };
    let metadata = metadata.or_else(|| output.metadata().cloned());
    if let Err(error) = ToolOutput::new(String::new())
        .try_with_details(structured_content.clone(), metadata.clone())
    {
        return Err((original, error));
    }
    let mut output = match content {
        Some(parts) => output.with_content_parts(parts), None => output,
    };
    if let Some(value) = is_error { output = output.with_is_error(value); }
    if let Some(value) = usage { output = output.with_usage(value); }
    let output = output.try_with_details(structured_content, metadata)
        .expect("replacement details were validated above");
    Ok(match denial { Some(error) => Err(error.with_output(output)), None => Ok(output) })
}

#[async_trait::async_trait]
impl ToolCallHook for ExtensionProcess {
    async fn transform_tool_call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        context: &ToolContext<'_>,
    ) -> Result<Option<serde_json::Value>, ToolError> {
        if !self.has_hook(ExtensionHook::BeforeToolCall) {
            return Ok(None);
        }
        let replacement = self.run_before_tool_call(name, arguments, context).await?;
        if replacement.as_ref().is_some_and(|value| !value.is_object()) {
            return Err(ToolError::new(format!(
                "extension `{}` replaced the arguments of `{name}` with a non-object",
                self.inner.descriptor.manifest.name
            )));
        }
        Ok(replacement)
    }

    async fn transform_tool_result(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        result: Result<ToolOutput, ToolError>,
        context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        if !self.has_hook(ExtensionHook::AfterToolCall) {
            return result;
        }
        let Some(replacement) = self
            .run_after_tool_call(name, arguments, &result, context)
            .await
            .and_then(|output| output.tool_result)
        else {
            return result;
        };
        let content = if let Some(parts) = &replacement.content {
            // Reuse the negotiated native result decoder and its owner/generation,
            // image sniffing, MIME, byte and part limits. This descriptor has no
            // output schema: a result hook is not a new tool execution.
            let definition: ToolDefinition = serde_json::from_value(serde_json::json!({
                "name":name, "description":"", "parameters":{"type":"object"}
            })).expect("hook codec descriptor is valid");
            let parts: Vec<_> = parts.iter().map(|part| match part {
                serde_json::Value::String(text) => serde_json::json!({"type":"text", "text":text}),
                other => other.clone(),
            }).collect();
            let connection = read_std_lock(&self.inner.connection).clone();
            match decode_tool_call_output(&connection, &definition, Some(context.resource_owner),
                serde_json::json!({"content":parts, "is_error":false, "metadata":{}})) {
                Ok(output) => Some(output.native_output.expect("decoder provides native content").content_parts().to_vec()),
                Err(error) => {
                    let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                        message: format!("after_tool_call content for `{name}` was invalid: {error}"),
                    });
                    return result;
                }
            }
        } else { None };
        match replace_tool_result(result, replacement, content) {
            Ok(result) => result,
            Err((result, error)) => {
                let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                    message: format!("after_tool_call replacement for `{name}` was invalid: {error}"),
                });
                result
            }
        }
    }

    // `transform_tool_call` and `transform_tool_result` run this process's
    // tool hooks with Pi's semantics; there is no separate observer pass.
    async fn before_tool_call(
        &self,
        _name: &str,
        _arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        Ok(())
    }

    async fn after_tool_call(
        &self,
        _name: &str,
        _arguments: &serde_json::Value,
        _output: &str,
        _is_error: bool,
        _context: &ToolContext<'_>,
    ) {
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
