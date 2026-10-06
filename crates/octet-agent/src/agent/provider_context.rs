//! Effective model-visible context preparation, before planning or admission.
use super::*;

#[cfg(test)]
mod tests;
use crate::extension::{ProviderContextHook, ProviderContextProjectionContext};

// Matches the existing encoded payload-hook ceiling. Count serialization
// without allocating a second body, stopping as soon as the ceiling is crossed.
const MAX_CONTEXT_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const CONTEXT_HOOK_DEADLINE: Duration = Duration::from_secs(5);

pub(super) struct ProviderContextPreparation<'a> {
    pub(super) hooks: &'a [Arc<dyn ProviderContextHook>],
    pub(super) tool_choice: ToolChoice,
    pub(super) output_modalities: OutputModalities,
    pub(super) service_tier: Option<ServiceTier>,
    pub(super) replay_mode: AgentCompactionMode,
}

/// Bind once from the live run owner, then use the resulting client for every
/// main/auxiliary attempt in that run. AI assigns a fresh operation ID per call.
/// Do not store this owner-bound clone back into a shared catalog/client.
pub(super) fn provider_request_client(
    client: &octet_ai::AiClient,
    factories: &[Arc<dyn crate::extension_provider::ProviderRequestHookFactory>],
    resource_owner: &str,
) -> Result<octet_ai::AiClient, AgentError> {
    if factories.is_empty() {
        return Ok(client.clone());
    }
    let hooks = factories
        .iter()
        .map(|factory| factory.bind_provider_request_hook(resource_owner))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(client.with_provider_request_hooks(hooks))
}

fn refused(reason: &'static str) -> AgentError {
    AgentError::ProviderContextPreparation(reason)
}

struct BoundedRequestSize(usize);
impl Write for BoundedRequestSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_CONTEXT_REQUEST_BYTES.saturating_sub(self.0) {
            return Err(io::Error::other("provider context size limit"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn validate_size(request: &Request) -> Result<(), AgentError> {
    serde_json::to_writer(BoundedRequestSize(0), request)
        .map_err(|_| refused("request exceeds the context byte limit"))
}

fn same_call(a: &ToolCall, b: &ToolCall) -> bool {
    a.id == b.id
        && a.name == b.name
        && a.arguments_json == b.arguments_json
        && a.async_execution == b.async_execution
        && a.argument_error == b.argument_error
}

/// A projection may omit completed pairs or change model-visible result text,
/// but cannot invent/rename/reorder calls, change their arguments or authority,
/// create orphan/duplicate results, or forget a still-owned async call.
fn validate_identities(
    canonical: &Request,
    projected: &Request,
    model: &Model,
) -> Result<(), AgentError> {
    let calls: Vec<_> = canonical
        .messages
        .iter()
        .flat_map(|message| match message {
            Message::Assistant(assistant) => assistant
                .content
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::ToolCall(call) => Some((assistant, call)),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Message::User(_) => Vec::new(),
        })
        .collect();
    let mut canonical_pending = HashMap::new();
    let mut original_results = HashMap::new();
    let mut index = 0;
    for message in &canonical.messages {
        match message {
            Message::Assistant(assistant) => {
                for part in &assistant.content {
                    if let AssistantPart::ToolCall(call) = part {
                        canonical_pending.insert(&call.id, index);
                        index += 1;
                    }
                }
            }
            Message::User(user) => {
                for part in &user.content {
                    if let UserPart::ToolResult(result) = part {
                        if let Some(index) = canonical_pending.remove(&result.tool_call_id) {
                            original_results.insert(index, result);
                        }
                    }
                }
            }
        }
    }
    let mut next_call = 0;
    let mut retained = HashSet::new();
    let mut pending: HashMap<&octet_ai::ToolCallId, (&ToolCall, usize)> = HashMap::new();
    for message in &projected.messages {
        match message {
            Message::Assistant(assistant) => {
                if pending.values().any(|(call, _)| !call.async_execution) {
                    return Err(refused("missing tool result before assistant message"));
                }
                for part in &assistant.content {
                    if let AssistantPart::ToolCall(call) = part {
                        let Some(index) = calls[next_call..]
                            .iter()
                            .position(|(source, original)| {
                                source.model == assistant.model
                                    && source.protocol == assistant.protocol
                                    && same_call(original, call)
                            })
                            .map(|index| next_call + index)
                        else {
                            return Err(refused("tool call identity or authority changed"));
                        };
                        if pending.insert(&call.id, (call, index)).is_some() {
                            return Err(refused("duplicate pending tool call"));
                        }
                        retained.insert(index);
                        next_call = index + 1;
                    }
                }
            }
            Message::User(user) => {
                for part in &user.content {
                    if let UserPart::ToolResult(result) = part {
                        let Some((_, index)) = pending.remove(&result.tool_call_id) else {
                            return Err(refused("orphan or duplicate tool result"));
                        };
                        let Some(original) = original_results.get(&index) else {
                            return Err(refused("projection invented a tool result"));
                        };
                        if result.is_error != original.is_error
                            || result.added_tool_names != original.added_tool_names
                        {
                            return Err(refused("tool result identity or status changed"));
                        }
                    }
                }
            }
        }
    }
    for (call, _) in pending.values() {
        if !call.async_execution
            || !model.responses_features().async_tools
            || !projected
                .tools
                .iter()
                .any(|tool| tool.name == call.name && tool.async_execution)
        {
            return Err(refused("missing tool result"));
        }
    }
    // Unpaired canonical calls remain run-owned. Do not allow a projection to
    // hide them and leave their eventual result without its provider identity.
    if canonical_pending
        .values()
        .any(|index| !retained.contains(index))
    {
        return Err(refused("projection omitted an unresolved tool call"));
    }
    Ok(())
}

pub(super) async fn project_provider_context(
    mut request: Request,
    hooks: &[Arc<dyn ProviderContextHook>],
    context: &ProviderContextProjectionContext,
    model: &Model,
    abort: &AbortFlag,
    session: &mut Session,
) -> Result<Request, AgentError> {
    validate_size(&request)?;
    let canonical = request.clone();
    let deadline = tokio::time::Instant::now() + model.endpoint.timeout.min(CONTEXT_HOOK_DEADLINE);
    for hook in hooks {
        if abort.is_set() {
            return Err(AgentError::Cancelled);
        }
        let mut service = hook.begin_session_wait(session, context).map_err(|error| {
            // Activation implementations historically return String. Expose
            // only the finite native refusal codes, never arbitrary hook
            // errors, credentials, paths or session content.
            refused(match error.as_str() {
                "session_append_authority_unavailable" => "session_append_authority_unavailable",
                "session_append_lease_busy" => "session_append_lease_busy",
                "session_append_process_retired" => "session_append_process_retired",
                "session_snapshot_too_large" => "session_snapshot_too_large",
                "session_snapshot_unavailable" => "session_snapshot_unavailable",
                _ => "session append service refused activation",
            })
        })?;
        // Neither this hook future nor readiness borrows Session. Only the
        // owning driver below receives mutable persistence authority.
        let projection = {
            let future = match service
                .as_mut()
                .and_then(|service| service.projection_future(&request, context))
            {
                Some(future) => future,
                None => hook.project_context(&request, context),
            };
            tokio::pin!(future);
            loop {
                let ready = service
                    .as_ref()
                    .map(|service| service.ready())
                    .unwrap_or_else(|| Box::pin(std::future::pending()));
                tokio::select! {
                    biased;
                    _ = abort.wait() => return Err(AgentError::Cancelled),
                    _ = tokio::time::sleep_until(deadline) => return Err(refused("hook deadline exceeded")),
                    _ = ready => {
                        service.as_mut().expect("only an active service becomes ready")
                            .consume_next(session, context)
                            .map_err(|_| refused("session append service failed"))?;
                    },
                    result = &mut future => break result,
                }
            }
        };
        drop(service);
        if abort.is_set() {
            return Err(AgentError::Cancelled);
        }
        let projection = projection.map_err(|_| refused("hook failed"))?;
        if let Some(projection) = projection {
            request.messages = projection.messages;
            request.system = projection.system;
            if let Some(tools) = projection.tools {
                let mut seen = HashSet::new();
                for tool in &tools {
                    let Some(original) = canonical
                        .tools
                        .iter()
                        .find(|original| original.name == tool.name)
                    else {
                        return Err(refused("loadout introduced an unregistered tool"));
                    };
                    let mut unchanged = tool.clone();
                    unchanged.description = original.description.clone();
                    if !seen.insert(tool.name.clone())
                        || serde_json::to_value(&unchanged).expect("canonical tool serializes")
                            != serde_json::to_value(original).expect("canonical tool serializes")
                    {
                        return Err(refused("loadout changed tool authority"));
                    }
                }
                request.tools = tools;
            }
        }
        validate_size(&request)?;
        // Never accept a change that opaque replay would ignore, or discard
        // provider continuation state in order to make a projection work.
        if request
            .responses
            .as_ref()
            .is_some_and(|options| options.input.is_some())
        {
            let unchanged = canonical.system == request.system
                && serde_json::to_vec(&canonical.messages).expect("messages serialize")
                    == serde_json::to_vec(&request.messages).expect("messages serialize");
            if !unchanged {
                return Err(refused(
                    "opaque Responses replay requires an authoritative projection seam",
                ));
            }
        }
        validate_identities(&canonical, &request, model)?;
        octet_ai::validate_provider_request(model, &request)
            .map_err(|_| refused("invalid canonical provider context"))?;
    }
    if abort.is_set() {
        return Err(AgentError::Cancelled);
    }
    if tokio::time::Instant::now() >= deadline {
        return Err(refused("hook deadline exceeded"));
    }
    Ok(request)
}

impl CompactionContext<'_> {
    pub(super) fn canonical_provider_request(
        &self,
        system: &str,
        tools: &[ToolDef],
        output_ceiling: u64,
        preparation: &ProviderContextPreparation<'_>,
    ) -> Result<Request, AgentError> {
        let responses = if preparation.replay_mode == AgentCompactionMode::NativeResponses {
            Some(native_responses_options(
                self.session,
                self.model,
                system,
                preparation.service_tier,
            )?)
        } else {
            durable_responses_options(self.session, self.model, system, preparation.service_tier)?
        };
        let request = Request {
            system: (!system.is_empty()).then(|| system.to_owned()),
            messages: self.session.context()?,
            tools: tools.to_vec(),
            tool_choice: preparation.tool_choice.clone(),
            max_output_tokens: Some(output_ceiling),
            temperature: None,
            stop: Vec::new(),
            reasoning: request_reasoning_for_replay(
                self.session,
                self.model,
                responses.as_ref(),
                self.reasoning,
            )?,
            reasoning_mode: self.reasoning_mode,
            responses,
            output_format: OutputFormat::Text,
            output_modalities: preparation.output_modalities.clone(),
            compatibility: CompatibilityMode::Strict,
            cache_retention: self.cache_retention,
            session_id: Some(self.session_id.to_owned()),
        };
        Ok(request)
    }
}

pub(super) fn effective_request_estimate(request: &Request) -> u64 {
    let canonical = estimate_request_tokens(
        request.system.as_deref().unwrap_or_default(),
        &request.messages,
        &request.tools,
    );
    if let Some(input) = request
        .responses
        .as_ref()
        .and_then(|options| options.input.as_ref())
    {
        let mut bytes = CountingWriter::default();
        serde_json::to_writer(&mut bytes, &(input, &request.tools))
            .expect("provider input serializes");
        canonical.max(bytes.0.div_ceil(4).saturating_add(64))
    } else {
        canonical
    }
}
