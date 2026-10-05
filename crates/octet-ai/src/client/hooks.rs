//! Canonical preparation shared by every host-mediated attempt.
//!
//! Two extension seams reach a request before it leaves the process: a host
//! payload hook that may replace the codec's encoded JSON body, and a host
//! transport that may take the round trip over entirely. Both are fed the same
//! prepared request, because replay history is derived and strict validation
//! runs first — a host transport can therefore never observe a request the
//! built-in path would have rejected.
//!
//! This is separate from `client` because it is endpoint-agnostic: no provider
//! name appears in it, and every protocol reaches it through the same two
//! functions. Keeping it small and free of dispatch logic is what makes the host
//! boundary auditable.

use std::sync::Arc;

use crate::catalog::Model;
use crate::error::{AiError, DecodeError};
use crate::runtime::{HookModelContext, ProviderRequestContext, ProviderRequestHook};
use crate::types::Request;
pub(super) fn merge_preset_headers(
    headers: &mut http::HeaderMap,
    values: &std::collections::BTreeMap<String, String>,
) -> Result<(), AiError> {
    for (name, value) in values {
        let name = http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| crate::ConfigError::Parse("invalid request header name".into()))?;
        let mut value = http::HeaderValue::from_str(value)
            .map_err(|_| crate::ConfigError::Parse("invalid request header value".into()))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(())
}

/// Maximum encoded request body a host payload hook may produce.
const MAX_HOOKED_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Applies one host payload hook to an encoded JSON request body.
///
/// The hook sees exactly the codec's payload. A non-JSON body, an oversized
/// replacement, or a hook error fails the attempt before authentication or
/// dispatch; there is no hidden retry.
pub(super) fn apply_payload_hook(
    hook: &Arc<dyn crate::runtime::PayloadHook>,
    model: &Model,
    body: bytes::Bytes,
) -> Result<bytes::Bytes, AiError> {
    if body.is_empty() {
        return Ok(body);
    }
    let payload = decode_hook_body(&body)?;
    let host_model = HookModelContext::from_model(model);
    let Some(replacement) = hook.on_payload(payload, &host_model)? else {
        return Ok(body);
    };
    encode_hook_body(&replacement)
}

/// The reserved check includes removals and every repeated value, not just the
/// first value of names which remain present after a transform.
pub(crate) fn validate_hook_headers(
    before: &http::HeaderMap,
    after: &http::HeaderMap,
) -> Result<(), AiError> {
    for name in before.keys().chain(after.keys()) {
        if crate::runtime::is_reserved_header(name)
            && !before.get_all(name).iter().eq(after.get_all(name).iter())
        {
            return Err(crate::ConfigError::ReservedHeader(name.clone()).into());
        }
    }
    Ok(())
}

fn decode_hook_body(body: &[u8]) -> Result<serde_json::Value, AiError> {
    if body.len() > MAX_HOOKED_BODY_BYTES {
        return Err(
            crate::ConfigError::Parse("provider hook input exceeds byte limit".into()).into(),
        );
    }
    serde_json::from_slice(body)
        .map_err(|_| AiError::Decode(DecodeError::Json("invalid provider hook JSON body".into())))
}

fn encode_hook_body(payload: &serde_json::Value) -> Result<bytes::Bytes, AiError> {
    if !payload.is_object() && !payload.is_array() {
        return Err(crate::ConfigError::Parse(
            "provider hook body must be an object or array".into(),
        )
        .into());
    }
    let encoded = serde_json::to_vec(payload).map_err(|_| {
        AiError::Decode(DecodeError::Json("invalid provider hook JSON body".into()))
    })?;
    if encoded.len() > MAX_HOOKED_BODY_BYTES {
        return Err(
            crate::ConfigError::Parse("provider hook output exceeds byte limit".into()).into(),
        );
    }
    Ok(bytes::Bytes::from(encoded))
}

/// Exists only when an async subscriber is installed. One identity and ordered
/// chain survive all three phases; no wire hooks run on opaque host transports.
pub(super) struct ProviderRequestAttempt {
    context: ProviderRequestContext,
    hooks: Vec<Arc<dyn ProviderRequestHook>>,
    timeout: std::time::Duration,
}

impl ProviderRequestAttempt {
    pub(super) fn new(
        model: &Model,
        client_hooks: &[Arc<dyn ProviderRequestHook>],
        request_hooks: &[Arc<dyn ProviderRequestHook>],
    ) -> Option<Self> {
        if client_hooks.is_empty() && request_hooks.is_empty() {
            return None;
        }
        static NEXT_OPERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let operation = NEXT_OPERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(Self {
            context: ProviderRequestContext {
                operation_id: format!("provider-http:{operation}"),
                model: HookModelContext::from_model(model),
            },
            hooks: client_hooks.iter().chain(request_hooks).cloned().collect(),
            timeout: model
                .endpoint
                .timeout
                .min(std::time::Duration::from_secs(5)),
        })
    }

    pub(super) async fn payload(&self, mut body: bytes::Bytes) -> Result<bytes::Bytes, AiError> {
        tokio::time::timeout(self.timeout, async {
            for hook in &self.hooks {
                if let Some(payload) = hook
                    .before_request(&self.context, decode_hook_body(&body)?)
                    .await?
                {
                    body = encode_hook_body(&payload)?;
                }
            }
            Ok(body)
        })
        .await
        .map_err(|_| hook_deadline())?
    }

    pub(super) async fn headers(&self, headers: &mut http::HeaderMap) -> Result<(), AiError> {
        tokio::time::timeout(self.timeout, async {
            for hook in &self.hooks {
                let before = headers.clone();
                hook.before_headers(&self.context, headers).await?;
                validate_hook_headers(&before, headers)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| hook_deadline())?
    }

    pub(super) async fn response(
        &self,
        status: http::StatusCode,
        headers: &http::HeaderMap,
    ) -> Result<(), AiError> {
        tokio::time::timeout(self.timeout, async {
            for hook in &self.hooks {
                hook.after_response(&self.context, status, headers).await?;
            }
            Ok(())
        })
        .await
        .map_err(|_| hook_deadline())?
    }
}

fn hook_deadline() -> AiError {
    crate::ConfigError::Parse("provider hook deadline exceeded".into()).into()
}

/// Canonical preparation shared by every host-mediated attempt.
///
/// Replay history is derived without mutating the caller's conversation and
/// strict validation runs before the transport sees the request, so a host
/// transport can never observe a request that the built-in path would reject.
pub(super) fn prepare_host_request(
    model: &Model,
    req: Request,
) -> Result<(Request, Vec<crate::error::Diagnostic>), AiError> {
    let mut request = req;
    request.messages = crate::transform::transform_request_messages_owned(request.messages, model);
    let request = crate::validate::normalize_request_reasoning(&request, &model.spec.capabilities)
        .into_owned();
    let diagnostics = crate::validate::validate_request(
        &request,
        &model.spec.capabilities,
        &model.spec.limits,
        model.spec.protocol,
        &model.spec.id,
        crate::CompatibilityMode::Strict,
    )?;
    Ok((request, diagnostics))
}
