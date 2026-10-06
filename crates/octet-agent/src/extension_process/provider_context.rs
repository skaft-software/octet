//! Process-backed canonical projection with one authoritative Session consumer.
use super::session_leaf::SessionLeafProcessLease;
use super::*;
use crate::extension::{
    ProviderContextHook, ProviderContextProjection, ProviderContextProjectionContext,
    ProviderContextSessionWait,
};
use crate::session::Session;
use crate::session_leaf::{SessionLeafBinding, SessionLeafConsumer};

static NEXT_CONTEXT_ACTIVATION: AtomicU64 = AtomicU64::new(1);

fn binding_activation_refusal(error: ExtensionRuntimeError) -> String {
    match error {
        ExtensionRuntimeError::Closed(_) => "session_append_process_retired",
        ExtensionRuntimeError::Protocol(ref message)
            if message == "private session leaf activation already bound" =>
        {
            "session_append_lease_busy"
        }
        _ => "session_append_authority_unavailable",
    }
    .to_owned()
}

fn snapshot_activation_refusal(error: ExtensionRuntimeError) -> String {
    match error {
        ExtensionRuntimeError::Closed(_) => "session_append_process_retired",
        ExtensionRuntimeError::MessageTooLarge { .. } => "session_snapshot_too_large",
        ExtensionRuntimeError::Protocol(ref message)
            if matches!(
                message.as_str(),
                "session snapshot exceeds wire bound; no entries truncated"
                    | "session snapshot exceeds entry bound; no entries truncated"
                    | "session metadata exceeds namespace limit"
                    | "session metadata exceeds durable bounds"
            ) =>
        {
            "session_snapshot_too_large"
        }
        ExtensionRuntimeError::Protocol(ref message)
            if message == "session snapshot owner changed" =>
        {
            "session_append_authority_unavailable"
        }
        _ => "session_snapshot_unavailable",
    }
    .to_owned()
}

struct ProcessContextWait {
    process: ExtensionProcess,
    consumer: SessionLeafConsumer,
    binding: SessionLeafBinding,
    preparation: ProviderContextProjectionContext,
    lease: Option<SessionLeafProcessLease>,
}

impl ProviderContextSessionWait for ProcessContextWait {
    fn projection_future(
        &mut self,
        request: &octet_ai::Request,
        context: &ProviderContextProjectionContext,
    ) -> Option<
        std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ProviderContextProjection>, String>>
                    + Send
                    + 'static,
            >,
        >,
    > {
        let lease = self.lease.take().expect("one future per preparation guard");
        let payload = serde_json::json!({
            "request": request,
            "preparation": {
                "resource_owner": context.resource_owner,
                "session_id": context.session_id,
                "head": context.head,
                "tool_generation": context.tool_generation,
            },
        });
        let mut execution = self.process.execution_context();
        execution.resource_owner = Some(self.binding.owner.clone());
        execution.host.session_id = Some(context.session_id.clone());
        let future = lease.run_hook(ExtensionHook::ProviderContext, payload, execution);
        Some(Box::pin(async move {
            let output = future
                .await
                .map_err(|_| "process context hook failed".to_owned())?;
            if output.disposition != ExtensionHookDisposition::Continue {
                return Err("process context hook refused preparation".into());
            }
            output
                .provider_context
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "invalid canonical process context projection".into())
        }))
    }

    fn ready(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let ready = self.consumer.ready();
        // Terminal readiness wakes the owner once; consume_next refuses it,
        // rather than spinning or starting another hook on a closed lane.
        Box::pin(async move {
            let _ = ready.await;
        })
    }

    fn consume_next(
        &mut self,
        session: &mut Session,
        context: &ProviderContextProjectionContext,
    ) -> Result<(), String> {
        let current = self
            .process
            .current_context_for_resource_owner(session.resource_owner_key());
        if context != &self.preparation
            || current.resource_owner.as_ref() != Some(&self.binding.owner)
        {
            self.consumer.revoker().revoke();
            return Err("process context session authority changed".into());
        }
        match self.consumer.consume_next(session, &self.binding) {
            Ok(true) => Ok(()),
            Ok(false) => Err("process context session lane closed".into()),
            Err(_) => Err("process context session append refused".into()),
        }
    }
}

#[async_trait::async_trait]
impl ProviderContextHook for ExtensionProcess {
    fn begin_session_wait(
        &self,
        session: &Session,
        context: &ProviderContextProjectionContext,
    ) -> Result<Option<Box<dyn ProviderContextSessionWait>>, String> {
        if self.api_version() != EXTENSION_API_VERSION_0_4
            || !self.supports_feature(EXTENSION_FEATURE_SESSION_ENTRIES)
            || !self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::ProviderContext)
            || context.resource_owner != session.resource_owner_key()
        {
            return Err("session_append_authority_unavailable".into());
        }
        let execution = self.current_context_for_resource_owner(session.resource_owner_key());
        let owner = execution
            .resource_owner
            .expect("current process context has an owner");
        let epoch = NEXT_CONTEXT_ACTIVATION.fetch_add(1, Ordering::Relaxed);
        let binding = SessionLeafBinding {
            activation_epoch: epoch,
            owner,
            namespace: self.descriptor().manifest.name.clone(),
            operation_id: format!("provider-context:{epoch}"),
        };
        let (consumer, producer, grant) = SessionLeafConsumer::new(session, binding.clone())
            .map_err(|_| "session_append_authority_unavailable".to_owned())?;
        let lease = self
            .bind_session_leaf(producer, consumer.revoker(), grant)
            .map_err(binding_activation_refusal)?
            .with_session_snapshot(session)
            .map_err(snapshot_activation_refusal)?;
        Ok(Some(Box::new(ProcessContextWait {
            process: self.clone(),
            consumer,
            binding,
            preparation: context.clone(),
            lease: Some(lease),
        })))
    }

    async fn project_context(
        &self,
        _: &octet_ai::Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        // Direct process projection has no authoritative consumer. Only the
        // owning driver's guard-bound, generation-pinned future may dispatch it.
        Err("process context hook requires the owning Session driver".into())
    }
}

// The wire pipeline is separate from canonical context preparation. It carries
// the codec-produced body and the actual HTTP response, never a reconstruction.
const PIPELINE_HOOKS_FEATURE: &str = "pipeline_hooks_v1";
const MAX_PIPELINE_HEADER_BYTES: usize = 64 * 1024;
const MAX_PIPELINE_HEADERS: usize = 256;

struct ProcessProviderRequestHook {
    process: ExtensionProcess,
    connection: Arc<ProcessConnection>,
    context: ExtensionExecutionContext,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderPipelineReply {
    #[serde(default)]
    disposition: ExtensionHookDisposition,
    #[serde(default)]
    provider_payload: Option<serde_json::Value>,
    #[serde(default)]
    provider_headers: Option<BTreeMap<String, Option<PipelineHeaderValue>>>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PipelineHeaderValue {
    One(String),
    Many(Vec<String>),
}

fn pipeline_refused(reason: &'static str) -> AiError {
    // Never surface a remote exception, payload/header contents or validation
    // value through normal provider errors, traces, session records or UI.
    octet_ai::ConfigError::Parse(reason.to_owned()).into()
}

fn pipeline_header_projection(
    headers: &reqwest::header::HeaderMap,
) -> Result<BTreeMap<String, PipelineHeaderValue>, AiError> {
    let mut projected = BTreeMap::new();
    let mut bytes = 0usize;
    for name in headers.keys() {
        let mut values = Vec::new();
        for value in headers.get_all(name) {
            let value = value.to_str().map_err(|_| {
                pipeline_refused("provider hook cannot represent a non-text header")
            })?;
            bytes = bytes
                .saturating_add(name.as_str().len())
                .saturating_add(value.len());
            if bytes > MAX_PIPELINE_HEADER_BYTES || values.len() >= MAX_PIPELINE_HEADERS {
                return Err(pipeline_refused(
                    "provider hook header byte/count limit exceeded",
                ));
            }
            values.push(value.to_owned());
        }
        let value = if values.len() == 1 {
            PipelineHeaderValue::One(values.remove(0))
        } else {
            PipelineHeaderValue::Many(values)
        };
        projected.insert(name.as_str().to_owned(), value);
        if projected.len() > MAX_PIPELINE_HEADERS {
            return Err(pipeline_refused(
                "provider hook header count limit exceeded",
            ));
        }
    }
    Ok(projected)
}

fn apply_pipeline_headers(
    headers: &mut reqwest::header::HeaderMap,
    patch: BTreeMap<String, Option<PipelineHeaderValue>>,
) -> Result<(), AiError> {
    if patch.len() > MAX_PIPELINE_HEADERS {
        return Err(pipeline_refused(
            "provider hook header count limit exceeded",
        ));
    }
    let mut names = HashSet::new();
    for (name, values) in patch {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| pipeline_refused("invalid provider hook header name"))?;
        if !names.insert(name.clone()) {
            return Err(pipeline_refused("duplicate provider hook header name"));
        }
        headers.remove(&name);
        let values = match values {
            None => continue,
            Some(PipelineHeaderValue::One(value)) => vec![value],
            Some(PipelineHeaderValue::Many(values)) => values,
        };
        if values.is_empty() || values.len() > MAX_PIPELINE_HEADERS {
            return Err(pipeline_refused("invalid provider hook header values"));
        }
        for value in values {
            let mut value = reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| pipeline_refused("invalid provider hook header value"))?;
            value.set_sensitive(true);
            headers.append(&name, value);
        }
    }
    pipeline_header_projection(headers)?;
    Ok(())
}

impl ExtensionProcess {
    /// Whether this API 0.4 process has a negotiated real HTTP pipeline binding.
    pub fn has_provider_pipeline_hooks(&self) -> bool {
        self.api_version() == EXTENSION_API_VERSION_0_4
            && self.supports_feature(PIPELINE_HOOKS_FEATURE)
            && self.inner.contributions.hooks.iter().any(|hook| {
                matches!(
                    hook,
                    ExtensionHook::BeforeProviderRequest
                        | ExtensionHook::BeforeProviderHeaders
                        | ExtensionHook::AfterProviderResponse
                )
            })
    }
}

impl crate::extension_provider::ProviderRequestHookFactory for ExtensionProcess {
    fn bind_provider_request_hook(
        &self,
        resource_owner: &str,
    ) -> Result<Arc<dyn octet_ai::ProviderRequestHook>, AiError> {
        if !self.has_provider_pipeline_hooks() || resource_owner.is_empty() {
            return Err(pipeline_refused("provider pipeline binding unavailable"));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let mut context = self.execution_context();
        context.resource_owner = Some(ExtensionResourceOwner {
            session_id: resource_owner.to_owned(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let hook = ProcessProviderRequestHook {
            process: self.clone(),
            connection,
            context,
        };
        hook.validate_binding()?;
        Ok(Arc::new(hook))
    }
}

impl ProcessProviderRequestHook {
    fn validate_binding(&self) -> Result<(), AiError> {
        if !connection_is_usable(&self.connection)
            || self.connection.draining.load(Ordering::Acquire)
            || !Arc::ptr_eq(
                &read_std_lock(&self.process.inner.connection),
                &self.connection,
            )
        {
            return Err(pipeline_refused("provider hook process generation retired"));
        }
        Ok(())
    }

    async fn dispatch(
        &self,
        hook: ExtensionHook,
        payload: serde_json::Value,
    ) -> Result<Option<ProviderPipelineReply>, AiError> {
        self.validate_binding()?;
        if !self.process.inner.contributions.hooks.contains(&hook) {
            return Ok(None);
        }
        // Pin the exact connection; run_hook intentionally follows current
        // generations for other callers and is not the right boundary here.
        let result: ProviderPipelineReply = self
            .process
            .request_typed_on_connection(
                self.connection.clone(),
                methods::HOOK_RUN,
                &HookRequest {
                    hook,
                    payload,
                    context: self.context.clone(),
                },
                self.context.resource_owner.clone(),
            )
            .await
            .map_err(|_| pipeline_refused("provider pipeline hook failed"))?;
        self.validate_binding()?;
        if result.disposition != ExtensionHookDisposition::Continue {
            return Err(pipeline_refused(
                "provider pipeline hook denied the attempt",
            ));
        }
        Ok(Some(result))
    }
}

#[async_trait::async_trait]
impl octet_ai::ProviderRequestHook for ProcessProviderRequestHook {
    async fn before_request(
        &self,
        context: &octet_ai::ProviderRequestContext,
        payload: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, AiError> {
        let Some(result) = self.dispatch(ExtensionHook::BeforeProviderRequest,
            serde_json::json!({"operation_id":context.operation_id, "model":context.model, "payload":payload}),
        ).await? else { return Ok(None) };
        if result.provider_headers.is_some() {
            return Err(pipeline_refused(
                "provider payload hook returned a header transformation",
            ));
        }
        Ok(result.provider_payload)
    }

    async fn before_headers(
        &self,
        context: &octet_ai::ProviderRequestContext,
        headers: &mut reqwest::header::HeaderMap,
    ) -> Result<(), AiError> {
        if !self
            .process
            .inner
            .contributions
            .hooks
            .contains(&ExtensionHook::BeforeProviderHeaders)
        {
            return self.validate_binding();
        }
        let Some(result) = self
            .dispatch(
                ExtensionHook::BeforeProviderHeaders,
                serde_json::json!({"operation_id":context.operation_id, "model":context.model,
                "headers":pipeline_header_projection(headers)?}),
            )
            .await?
        else {
            return Ok(());
        };
        if result.provider_payload.is_some() {
            return Err(pipeline_refused(
                "provider header hook returned a payload transformation",
            ));
        }
        if let Some(patch) = result.provider_headers {
            apply_pipeline_headers(headers, patch)?;
        }
        Ok(())
    }

    async fn after_response(
        &self,
        context: &octet_ai::ProviderRequestContext,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<(), AiError> {
        if !self
            .process
            .inner
            .contributions
            .hooks
            .contains(&ExtensionHook::AfterProviderResponse)
        {
            return self.validate_binding();
        }
        if let Some(result) = self
            .dispatch(
                ExtensionHook::AfterProviderResponse,
                serde_json::json!({"operation_id":context.operation_id, "model":context.model,
                "status":status.as_u16(), "headers":pipeline_header_projection(headers)?}),
            )
            .await?
        {
            if result.provider_payload.is_some() || result.provider_headers.is_some() {
                return Err(pipeline_refused(
                    "provider response observation cannot transform the request",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod large_history_tests;
