//! Effective model-visible context preparation, before planning or admission.
use super::*;

#[cfg(test)]
mod tests;
use crate::extension::{
    ProviderContextHook, ProviderContextHookError, ProviderContextProjectionContext,
};

// Matches the existing encoded payload-hook ceiling. Count serialization
// without allocating a second body, stopping as soon as the ceiling is crossed.
const MAX_CONTEXT_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const CONTEXT_HOOK_DEADLINE: Duration = Duration::from_secs(5);
// Beyond this cap, still warn rather than silently hiding a failure.
const MAX_CONTEXT_FAILURE_NOTICES: usize = 1024;

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
    AgentError::ProviderContextPreparation(reason.to_owned())
}

fn refused_owned(reason: String) -> AgentError {
    AgentError::ProviderContextPreparation(reason)
}

/// Model-visible tool names are extension/API vocabulary, but bound them so a
/// hostile manifest cannot turn a refusal into a wall of text.
fn bounded_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_graphic())
        .take(64)
        .collect()
}

fn kib(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

/// Counts serialized bytes without retaining the encoded body.
#[derive(Default)]
struct ByteCount(usize);
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Bounded, redaction-safe description of the request shape: counts, byte
/// sizes and the largest tool schemas. Never message text, arguments, session
/// IDs or paths, so this can be shown to the user as-is.
fn request_descriptor(request: &Request) -> String {
    let mut tools = request
        .tools
        .iter()
        .map(|tool| {
            let bytes = serde_json::to_string(tool).map_or(0, |text| text.len());
            (bounded_name(&tool.name), bytes)
        })
        .collect::<Vec<_>>();
    tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let schema_bytes = tools.iter().map(|(_, bytes)| *bytes).sum::<usize>();
    let largest = tools
        .iter()
        .take(3)
        .map(|(name, bytes)| format!("{name} {}", kib(*bytes)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut counter = ByteCount::default();
    let request_bytes = serde_json::to_writer(&mut counter, request).map_or(0, |()| counter.0);
    format!(
        "{} messages, {} tools, {} tool schema (largest: {}), request {}",
        request.messages.len(),
        request.tools.len(),
        kib(schema_bytes),
        if largest.is_empty() {
            "none".to_owned()
        } else {
            largest
        },
        kib(request_bytes),
    )
}

/// Stops serializing as soon as the context ceiling is crossed.
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

fn note_context_failure(
    hook: &dyn ProviderContextHook,
    session: &mut Session,
    notices: &mut Vec<String>,
) {
    let should_notice = match hook.failure_notice_id() {
        Some(extension) => {
            let seen = &mut session.context_failure_notices;
            if seen.contains(extension) {
                false
            } else {
                if seen.len() < MAX_CONTEXT_FAILURE_NOTICES {
                    seen.insert(extension.to_owned());
                }
                true
            }
        }
        None => true,
    };
    if should_notice {
        notices.push("Extension context hook failed or timed out; its changes were skipped. Disable the extension if this keeps happening.".to_owned());
    }
}

pub(super) async fn project_provider_context(
    request: Request,
    hooks: &[Arc<dyn ProviderContextHook>],
    context: &ProviderContextProjectionContext,
    model: &Model,
    abort: &AbortFlag,
    session: &mut Session,
    notices: &mut Vec<String>,
) -> Result<Request, AgentError> {
    // Every refusal names the shape that caused it: an unactionable "hook
    // refused preparation" cost a whole run and could not be diagnosed.
    let descriptor = request_descriptor(&request);
    match project_context_hooks(request, hooks, context, model, abort, session, notices).await {
        Err(AgentError::ProviderContextPreparation(reason)) => {
            Err(refused_owned(format!("{reason}; request {descriptor}")))
        }
        other => other,
    }
}

async fn project_context_hooks(
    mut request: Request,
    hooks: &[Arc<dyn ProviderContextHook>],
    context: &ProviderContextProjectionContext,
    model: &Model,
    abort: &AbortFlag,
    session: &mut Session,
    notices: &mut Vec<String>,
) -> Result<Request, AgentError> {
    validate_size(&request)?;
    let canonical = request.clone();
    // Keep only currently registered instances; dropping/reopening the Session
    // releases/resets the ledger. Never persist extension IDs in the transcript.
    session.context_failure_notices.retain(|id| {
        hooks
            .iter()
            .any(|hook| hook.failure_notice_id() == Some(id.as_str()))
    });
    let budget = model.endpoint.timeout.min(CONTEXT_HOOK_DEADLINE);
    // Extension hooks share one finite budget. Authority guards must still run
    // and fail closed after an extension consumes that budget, so each trusted
    // guard receives its own bounded deadline.
    let extension_deadline = tokio::time::Instant::now() + budget;
    for hook in hooks {
        if abort.is_set() {
            return Err(AgentError::Cancelled);
        }
        let recoverable = hook.continues_on_failure();
        let deadline = if recoverable {
            extension_deadline
        } else {
            tokio::time::Instant::now() + budget
        };
        if recoverable && tokio::time::Instant::now() >= deadline {
            note_context_failure(hook.as_ref(), session, notices);
            continue;
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
                None => Box::pin(async {
                    hook.project_context(&request, context)
                        .await
                        .map_err(ProviderContextHookError::Failed)
                }),
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
                    // A ready remote -32800 must win a same-poll deadline;
                    // successful results are still checked against the deadline below.
                    result = &mut future => break result,
                    _ = tokio::time::sleep_until(deadline) => break Err(ProviderContextHookError::Failed("context hook deadline exceeded".into())),
                    _ = ready => {
                        // Polling the hook above may itself queue an append and
                        // cancel the run in the same select iteration. Check
                        // again before granting synchronous persistence.
                        if abort.is_set() {
                            return Err(AgentError::Cancelled);
                        }
                        service.as_mut().expect("only an active service becomes ready")
                            .consume_next(session, context)
                            .map_err(|_| refused("session append service failed"))?;
                    },
                }
            }
        };
        drop(service);
        if abort.is_set() {
            return Err(AgentError::Cancelled);
        }
        let projection = match projection {
            Ok(_) if recoverable && tokio::time::Instant::now() >= deadline => {
                note_context_failure(hook.as_ref(), session, notices);
                continue;
            }
            Ok(projection) => projection,
            Err(ProviderContextHookError::Cancelled) => return Err(AgentError::Cancelled),
            // Never fall back after a host-authority or native validation
            // refusal, whatever the hook's isolation policy says.
            Err(ProviderContextHookError::Refused) => {
                return Err(refused("host authority refused preparation"))
            }
            Err(ProviderContextHookError::Failed(_)) if recoverable => {
                note_context_failure(hook.as_ref(), session, notices);
                continue;
            }
            Err(ProviderContextHookError::Failed(_)) => return Err(refused("hook failed")),
        };
        // The request that reached this hook. If the hook's projection produces
        // something the provider cannot represent, that projection is the
        // suspect — not the session — so it can be dropped and the run continues
        // on the validated request, exactly like any other handler failure.
        let accepted_before_hook = request.clone();
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
                        return Err(refused_owned(format!(
                            "loadout introduced unregistered tool {}",
                            bounded_name(&tool.name)
                        )));
                    };
                    let mut unchanged = tool.clone();
                    unchanged.description = original.description.clone();
                    if !seen.insert(tool.name.clone())
                        || serde_json::to_value(&unchanged).expect("canonical tool serializes")
                            != serde_json::to_value(original).expect("canonical tool serializes")
                    {
                        return Err(refused_owned(format!(
                            "loadout changed the authority of tool {}",
                            bounded_name(&tool.name)
                        )));
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
        // Name the failing validation. The alternative — a bare "invalid
        // canonical provider context" — leaves an operator with a 116-tool
        // request and no way to tell which tool or field is unacceptable.
        if let Some(path) = std::env::var_os("OCTET_CONTEXT_DUMP") {
            // Operator-supplied diagnostic: one line per preparation, so a
            // refusal can be traced to the exact fleet and validator reason.
            let mut tools = request
                .tools
                .iter()
                .map(|t| {
                    (
                        t.name.clone(),
                        serde_json::to_string(t).map_or(0, |s| s.len()),
                    )
                })
                .collect::<Vec<_>>();
            tools.sort_by_key(|tool| std::cmp::Reverse(tool.1));
            let record = serde_json::json!({
                "messages": request.messages.len(),
                "tools": request.tools.len(),
                "schema_bytes": tools.iter().map(|(_, n)| n).sum::<usize>(),
                "largest": tools.iter().take(5).map(|(n, s)| format!("{n}={s}")).collect::<Vec<_>>(),
                "validation": octet_ai::validate_provider_request(model, &request)
                    .err()
                    .map(|e| e.to_string()),
            });
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(file, "{record}");
            }
        }
        // A projection the provider cannot represent is a handler fault, not a
        // reason to lose the session: Pi isolates a failing context handler, and
        // a 116-tool Pi fleet must never be able to end a run. Fall back only to
        // the request that reached this hook, and only when it still validates,
        // so identity, loadout and host-authority checks above stay fatal.
        if let Err(error) = octet_ai::validate_provider_request(model, &request) {
            let detail: String = error.to_string().chars().take(240).collect();
            if octet_ai::validate_provider_request(model, &accepted_before_hook).is_ok() {
                request = accepted_before_hook;
                // One precise notice beats the generic hook-failure line plus a
                // detail line. Deduplicate on the hook's own identity, the same
                // key the caller prunes to live hooks, so the notice survives
                // pruning and is reported once per session and extension.
                let notice_id = hook.failure_notice_id().map(str::to_owned);
                let already_noticed = notice_id
                    .as_ref()
                    .is_some_and(|id| session.context_failure_notices.contains(id));
                if !already_noticed {
                    if let Some(id) = notice_id {
                        if session.context_failure_notices.len() < MAX_CONTEXT_FAILURE_NOTICES {
                            session.context_failure_notices.insert(id);
                        }
                    }
                    notices.push(format!(
                        "Extension context projection was invalid and was skipped: {detail}"
                    ));
                }
                continue;
            }
            return Err(refused_owned(format!(
                "invalid canonical provider context: {detail}"
            )));
        }
        if abort.is_set() {
            return Err(AgentError::Cancelled);
        }
        if !recoverable && tokio::time::Instant::now() >= deadline {
            return Err(refused("hook deadline exceeded"));
        }
    }
    if abort.is_set() {
        return Err(AgentError::Cancelled);
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
