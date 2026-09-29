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
use crate::runtime::HookModelContext;
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
    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
    let host_model = HookModelContext::from_model(model);
    let Some(replacement) = hook.on_payload(payload, &host_model)? else {
        return Ok(body);
    };
    let encoded = serde_json::to_vec(&replacement)
        .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
    if encoded.len() > MAX_HOOKED_BODY_BYTES {
        return Err(crate::ConfigError::Parse(format!(
            "payload hook produced a body larger than the {MAX_HOOKED_BODY_BYTES}-byte limit"
        ))
        .into());
    }
    Ok(bytes::Bytes::from(encoded))
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
