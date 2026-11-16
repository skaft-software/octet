//! Encoded provider request hooks, independent of the session driver.
use super::*;
// The wire pipeline is separate from canonical context preparation. It carries
// the codec-produced body and the actual HTTP response, never a reconstruction.
const PIPELINE_HOOKS_FEATURE: &str = "pipeline_hooks_v1";
const MAX_PIPELINE_HEADER_BYTES: usize = 64 * 1024;
const MAX_PIPELINE_HEADERS: usize = 256;

struct ProcessProviderRequestHook {
    process: ExtensionProcess,
    connection: Arc<ProcessConnection>,
    context: ExtensionExecutionContext,
    warnings: crate::extension_provider::ProviderHookWarnings,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderPipelineReply {
    #[serde(default)]
    disposition: ExtensionHookDisposition,
    #[serde(default)]
    provider_payload: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) provider_headers: Option<BTreeMap<String, Option<PipelineHeaderValue>>>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum PipelineHeaderValue {
    One(String),
    Many(Vec<String>),
}

fn pipeline_refused(reason: &'static str) -> AiError {
    // Never surface a remote exception, payload/header contents or validation
    // value through normal provider errors, traces, session records or UI.
    octet_ai::ConfigError::ExtensionHook(reason.to_owned()).into()
}

pub(super) fn pipeline_header_projection(
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

pub(super) fn apply_pipeline_headers(
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
        warnings: &crate::extension_provider::ProviderHookWarnings,
    ) -> Result<Option<Arc<dyn octet_ai::ProviderRequestHook>>, String> {
        if !self.has_provider_pipeline_hooks() || resource_owner.is_empty() {
            return Ok(None);
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
            warnings: warnings.clone(),
        };
        if !hook.binding_is_live() {
            return Err(format!(
                "Extension {} is not running, so its provider hooks were skipped for this run; requests are sent without its changes.",
                display_identity(&self.inner.descriptor.manifest.name),
            ));
        }
        Ok(Some(Arc::new(hook)))
    }
}

/// How a provider hook invocation ended when it did not produce a result.
enum HookFailure {
    /// The extension could not take part (stopped, timed out, malformed
    /// reply). The request continues without its change, as in Pi.
    Skip(&'static str),
    /// The extension explicitly vetoed the request, with its stated reason.
    Deny(String),
}

/// Bounded, control-free identity for host-authored diagnostics. Never used
/// for remote error text or wire data.
fn display_identity(value: &str) -> String {
    format!("\"{}\"", display_bounded(value, 48))
}

/// Strip control and bidi characters and bound to `max` bytes.
fn display_bounded(value: &str, max: usize) -> String {
    let mut result = String::new();
    for c in value.chars().filter(|c| {
        !c.is_control()
            && !matches!(c, '"' | '\\' | '\u{061c}' | '\u{200e}' | '\u{200f}'
                | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }) {
        if result.len() + c.len_utf8() > max {
            result.push('…');
            break;
        }
        result.push(c);
    }
    result
}

fn hook_stage(hook: ExtensionHook) -> &'static str {
    match hook {
        ExtensionHook::BeforeProviderRequest => "before_provider_request",
        ExtensionHook::BeforeProviderHeaders => "before_provider_headers",
        ExtensionHook::AfterProviderResponse => "after_provider_response",
        _ => "provider",
    }
}

impl ProcessProviderRequestHook {
    fn binding_is_live(&self) -> bool {
        connection_is_usable(&self.connection)
            && !self.connection.draining.load(Ordering::Acquire)
            && Arc::ptr_eq(
                &read_std_lock(&self.process.inner.connection),
                &self.connection,
            )
    }

    fn route(context: &octet_ai::ProviderRequestContext) -> String {
        format!(
            "provider {}, model {}",
            display_identity(&context.model.provider),
            display_identity(&context.model.id),
        )
    }

    /// Apply the isolation policy: a skipped hook leaves the request
    /// unchanged and is reported once; only an explicit veto fails it.
    fn settle<T>(
        &self,
        hook: ExtensionHook,
        context: &octet_ai::ProviderRequestContext,
        outcome: Result<T, HookFailure>,
        unchanged: T,
    ) -> Result<T, AiError> {
        let name = display_identity(&self.process.inner.descriptor.manifest.name);
        let stage = hook_stage(hook);
        match outcome {
            Ok(value) => Ok(value),
            Err(HookFailure::Skip(reason)) => {
                let effect = if hook == ExtensionHook::AfterProviderResponse {
                    "the response was not affected"
                } else {
                    "the request was sent without its change"
                };
                self.warnings.push(format!(
                    "Extension {name} {stage} hook skipped: {reason}; {effect}. Update or disable the extension if this keeps happening. ({})",
                    Self::route(context),
                ));
                Ok(unchanged)
            }
            Err(HookFailure::Deny(reason)) => Err(octet_ai::ConfigError::ExtensionHook(format!(
                "Extension {name} blocked this provider request in its {stage} hook{}. Disable the extension or check its settings to continue. ({})",
                match display_bounded(reason.trim(), 240) {
                    reason if reason.is_empty() => String::new(),
                    reason => format!(": {reason}"),
                },
                Self::route(context),
            ))
            .into()),
        }
    }

    async fn dispatch(
        &self,
        hook: ExtensionHook,
        payload: serde_json::Value,
    ) -> Result<Option<ProviderPipelineReply>, HookFailure> {
        if !self.process.inner.contributions.hooks.contains(&hook) {
            return Ok(None);
        }
        if !self.binding_is_live() {
            return Err(HookFailure::Skip(
                "the extension stopped or restarted during this run",
            ));
        }
        // Pin the exact connection; run_hook intentionally follows current
        // generations for other callers and is not the right boundary here.
        let result: serde_json::Value = self
            .process
            .request_typed_on_connection(
                self.connection.clone(),
                "hook/run",
                &HookRequest {
                    hook,
                    payload,
                    context: self.context.clone(),
                },
                self.context.resource_owner.clone(),
            )
            .await
            .map_err(|error| {
                HookFailure::Skip(match error {
                    ExtensionRuntimeError::MessageTooLarge { .. } => {
                        "the hook message exceeded the extension transport size limit"
                    }
                    ExtensionRuntimeError::Timeout { .. } => "the extension did not answer in time",
                    ExtensionRuntimeError::Cancelled { .. }
                    | ExtensionRuntimeError::Remote { code: -32800, .. } => {
                        "the extension cancelled the hook"
                    }
                    ExtensionRuntimeError::Remote { .. } => "the extension reported an error",
                    ExtensionRuntimeError::Closed(_) => "the extension process exited",
                    _ => "the hook request could not be completed",
                })
            })?;
        let result: ProviderPipelineReply = serde_json::from_value(result)
            .map_err(|_| HookFailure::Skip("the extension returned a malformed hook result"))?;
        if !self.binding_is_live() {
            return Err(HookFailure::Skip(
                "the extension stopped or restarted during this run",
            ));
        }
        if let ExtensionHookDisposition::Deny { reason } = &result.disposition {
            return Err(HookFailure::Deny(reason.clone()));
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
        let hook = ExtensionHook::BeforeProviderRequest;
        let outcome = async {
            let Some(result) = self
                .dispatch(
                    hook,
                    serde_json::json!({"operation_id":context.operation_id, "model":context.model, "payload":payload}),
                )
                .await?
            else {
                return Ok(None);
            };
            if result.provider_headers.is_some() {
                return Err(HookFailure::Skip("a payload hook returned a header change"));
            }
            if result
                .provider_payload
                .as_ref()
                .is_some_and(|payload| !payload.is_object() && !payload.is_array())
            {
                return Err(HookFailure::Skip("the replacement payload was not a JSON object"));
            }
            Ok(result.provider_payload)
        }
        .await;
        self.settle(hook, context, outcome, None)
    }

    async fn before_headers(
        &self,
        context: &octet_ai::ProviderRequestContext,
        headers: &mut reqwest::header::HeaderMap,
    ) -> Result<(), AiError> {
        let hook = ExtensionHook::BeforeProviderHeaders;
        let outcome = async {
            if !self.process.inner.contributions.hooks.contains(&hook) {
                return Ok(None);
            }
            let projected = pipeline_header_projection(headers)
                .map_err(|_| HookFailure::Skip("the request headers exceed hook limits"))?;
            let Some(result) = self
                .dispatch(
                    hook,
                    serde_json::json!({"operation_id":context.operation_id, "model":context.model, "headers":projected}),
                )
                .await?
            else {
                return Ok(None);
            };
            if result.provider_payload.is_some() {
                return Err(HookFailure::Skip("a header hook returned a payload change"));
            }
            let Some(patch) = result.provider_headers else {
                return Ok(None);
            };
            // Apply to a copy so a rejected patch never leaves the request
            // with half of an extension's header changes.
            let mut patched = headers.clone();
            apply_pipeline_headers(&mut patched, patch)
                .map_err(|_| HookFailure::Skip("the extension returned an invalid header change"))?;
            Ok(Some(patched))
        }
        .await;
        if let Some(patched) = self.settle(hook, context, outcome, None)? {
            *headers = patched;
        }
        Ok(())
    }

    async fn after_response(
        &self,
        context: &octet_ai::ProviderRequestContext,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<(), AiError> {
        let hook = ExtensionHook::AfterProviderResponse;
        let outcome = async {
            if !self.process.inner.contributions.hooks.contains(&hook) {
                return Ok(());
            }
            let projected = pipeline_header_projection(headers)
                .map_err(|_| HookFailure::Skip("the response headers exceed hook limits"))?;
            // Response observation has no transforming effect; a stray
            // change is ignored rather than failing an answered request.
            self.dispatch(
                hook,
                serde_json::json!({"operation_id":context.operation_id, "model":context.model,
                    "status":status.as_u16(), "headers":projected}),
            )
            .await
            .map(|_| ())
        }
        .await;
        self.settle(hook, context, outcome, ())
    }
}
