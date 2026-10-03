//! Private wire protocol codecs. Nothing here is part of the public API.

use serde::Serialize;

use crate::error::{AiError, ConfigError};
use crate::stream::{ResponseBuilder, StreamEvent};
use crate::types::{CacheCompatibility, CacheRetention, Request};

/// Wire cache-control marker shared by Anthropic and compatible endpoints.
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct CacheControl {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ttl: Option<&'static str>,
}

pub(crate) fn cache_session_id(req: &Request) -> Option<&str> {
    cache_session_id_for(req.cache_retention, req.session_id.as_deref())
}

/// OpenCode's routing header is not a prompt-cache control: even a request
/// without explicit cache retention must stay on the same session route.
pub(crate) fn is_opencode_session_route(model: &crate::catalog::Model) -> bool {
    model.spec.cache.send_session_affinity_headers
        && matches!(
            model.endpoint.id.0.as_str(),
            "opencode" | "opencode-go" | "opencode-anthropic" | "opencode-google"
        )
}

pub(crate) fn add_opencode_session_header(
    model: &crate::catalog::Model,
    req: &Request,
    headers: &mut http::HeaderMap,
) -> Result<(), AiError> {
    const NAME: &str = "x-opencode-session";
    if !is_opencode_session_route(model)
        || model.endpoint.default_headers.contains_key(NAME)
        || model
            .spec
            .preset
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case(NAME))
    {
        return Ok(());
    }
    if let Some(session_id) = req.session_id.as_deref().filter(|id| !id.is_empty()) {
        let value = http::HeaderValue::from_str(session_id)
            .map_err(|_| ConfigError::InvalidHeader(NAME.into()))?;
        headers.insert(http::HeaderName::from_static(NAME), value);
    }
    Ok(())
}

pub(crate) fn cache_session_id_for(
    retention: CacheRetention,
    session_id: Option<&str>,
) -> Option<&str> {
    (retention != CacheRetention::None)
        .then_some(session_id)
        .flatten()
        .filter(|id| !id.is_empty())
}

pub(crate) fn prompt_cache_key(req: &Request) -> Option<String> {
    prompt_cache_key_for(req.cache_retention, req.session_id.as_deref())
}

pub(crate) fn prompt_cache_key_for(
    retention: CacheRetention,
    session_id: Option<&str>,
) -> Option<String> {
    let id = cache_session_id_for(retention, session_id)?;
    let key: String = id.chars().take(64).collect();
    (!key.is_empty()).then_some(key)
}

pub(crate) fn cache_control(
    req: &Request,
    compatibility: &CacheCompatibility,
) -> Option<CacheControl> {
    if req.cache_retention == CacheRetention::None {
        return None;
    }
    Some(CacheControl {
        kind: "ephemeral",
        ttl: (req.cache_retention == CacheRetention::Long && compatibility.supports_long_retention)
            .then_some("1h"),
    })
}

pub(crate) mod anthropic;
pub(crate) mod bedrock;
pub(crate) mod google;
pub(crate) mod grammar;
pub(crate) mod mistral_conversations;
pub(crate) mod openai_chat;
pub(crate) mod openai_responses;
pub(crate) mod pi_messages;
pub(crate) mod preset;

pub(crate) mod sse;

/// Pi's per-API `supportsStrictMode` default with a model's explicit
/// declaration applied.
///
/// Responses defaults off except Azure/Codex; Google and Mistral default on;
/// Anthropic and Bedrock default off. Public OpenAI Chat accepts strict tools,
/// but unknown compatible Chat endpoints do not inherit that guarantee. A model
/// preset may override the route default in either direction.
pub(crate) fn strict_mode_for(model: &crate::catalog::Model) -> bool {
    use crate::types::Protocol;
    if let Some(declared) = model.spec.preset.supports_strict_mode {
        return declared;
    }
    // Only the Chat-compatible fallback changes here; the other API defaults
    // remain unchanged.
    if model.spec.protocol == Protocol::OpenAiResponses {
        return matches!(
            model.endpoint.runtime.responses_profile,
            crate::types::ResponsesRuntimeProfile::Azure
                | crate::types::ResponsesRuntimeProfile::Codex
        );
    }
    if model.spec.protocol == Protocol::OpenAiChat {
        let url = &model.endpoint.base_url;
        return url.scheme() == "https"
            && url.host_str() == Some("api.openai.com")
            && url.path() == "/v1/";
    }
    if model.spec.protocol == Protocol::AnthropicMessages {
        return anthropic_strict_tools_for(model);
    }
    if model.spec.protocol == Protocol::BedrockConverse {
        return false;
    }
    // Google, Mistral and future routes retain their existing defaults.
    true
}

/// Pi's `AnthropicMessagesCompat.supportsStrictTools`: default `false`.
pub(crate) fn anthropic_strict_tools_for(model: &crate::catalog::Model) -> bool {
    model
        .spec
        .preset
        .anthropic_compat
        .as_ref()
        .and_then(|compat| compat.supports_strict_tools)
        .unwrap_or(false)
}

/// Pi's `supportsOpenAIGrammarTools`: off unless the model declares it.
pub(crate) fn grammar_tools_for(model: &crate::catalog::Model) -> bool {
    model
        .spec
        .preset
        .supports_openai_grammar_tools
        .unwrap_or(false)
}

/// Resolves a protocol path while preserving the narrowly allowed version query
/// attached to an endpoint base URL. Azure's versioned API uses this shape;
/// the catalog validates that the query cannot contain credentials.
pub(crate) fn endpoint_url(base_url: &url::Url, path: &str) -> Result<url::Url, ConfigError> {
    let query = base_url.query().map(str::to_owned);
    let mut url = base_url
        .join(path)
        .map_err(|error| ConfigError::Parse(error.to_string()))?;
    url.set_query(query.as_deref());
    Ok(url)
}

#[cfg(test)]
mod endpoint_url_tests;

#[cfg(test)]
mod cross_protocol_tests;

#[cfg(test)]
mod normalize_tool_call_id_tests;

/// Maximum length of a tool-call ID on the wire, matching the canonical
/// `[A-Za-z0-9_-]{1,64}` shape every provider accepts (design §11, §7).
const MAX_TOOL_CALL_ID_LEN: usize = 64;

fn tool_call_id_is_wire_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TOOL_CALL_ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Deterministically coerce a tool-call ID into the target charset/length.
///
/// Design §11 requires temporary tool IDs to be normalized when a protocol
/// constrains the format — a stable per-request transform (truncate + hash)
/// applied identically to a `ToolCall.id` and its `ToolResult.tool_call_id`, so
/// the call/result pairing is never broken. Because this is a pure function of
/// the ID string, both sides map to the same output automatically.
///
/// IDs already matching `[A-Za-z0-9_-]{1,64}` are returned untouched (canonical
/// IDs are left alone). Otherwise a short FNV-1a hash of the full original is
/// appended to a sanitized, truncated prefix. The hash is a self-contained,
/// version-stable implementation (not [`std::hash`], whose output is not
/// guaranteed stable) so results are reproducible and testable. Cross-protocol
/// replay of long OpenAI Responses IDs (`call_…|item_…`, 450+ chars) is the
/// motivating case (Pi `pi-ai.md` §"Tool Call ID Normalization"). The
/// 64-bit hash makes collisions extremely unlikely for provider-sized IDs but
/// cannot make them impossible; this transform is a wire-format compatibility
/// aid, not a cryptographic uniqueness guarantee.
pub(crate) fn normalize_tool_call_id(id: &str) -> String {
    if tool_call_id_is_wire_valid(id) {
        return id.to_string();
    }

    normalize_invalid_tool_call_id(id)
}

/// Owned normalization avoids copying the overwhelmingly common already-valid
/// ID when a consumed request is prepared for the wire.
pub(crate) fn normalize_tool_call_id_owned(id: String) -> String {
    if tool_call_id_is_wire_valid(&id) {
        return id;
    }
    normalize_invalid_tool_call_id(&id)
}

/// Records `ev` on the response builder and appends it to the canonical event
/// list. Every streaming codec funnels provider events through this so builder
/// invariants are enforced exactly once.
pub(crate) fn emit_event(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    mut ev: StreamEvent,
) -> Result<(), AiError> {
    builder.on_event(&ev)?;
    if let StreamEvent::ToolCallEnd {
        index,
        argument_error,
    } = &mut ev
    {
        *argument_error = builder.tool_call_argument_error(*index);
    }
    events.push(ev);
    Ok(())
}

/// Canonical index for a provider-side key, allocating a fresh slot on first
/// sight. Callers that re-key after closing a segment must not rely on this
/// `.len()`-based allocation; see [`ResponseBuilder::next_canonical_index`].
pub(crate) fn get_canonical_index(builder: &mut ResponseBuilder, key: &str) -> usize {
    if let Some(&idx) = builder.provider_to_canonical_indices.get(key) {
        idx
    } else {
        let idx = builder.provider_to_canonical_indices.len();
        builder
            .provider_to_canonical_indices
            .insert(key.to_string(), idx);
        idx
    }
}

fn normalize_invalid_tool_call_id(id: &str) -> String {
    let is_valid_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';

    // FNV-1a 64-bit → 16 lowercase hex chars (all in the target charset).
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let hash_hex = format!("{hash:016x}");

    // Reserve room for `_<hash>`; fill the rest with the original's valid chars.
    let prefix_budget = MAX_TOOL_CALL_ID_LEN - 1 - hash_hex.len();
    let prefix: String = id
        .bytes()
        .filter(|&b| is_valid_char(b))
        .take(prefix_budget)
        .map(char::from)
        .collect();
    format!("{prefix}_{hash_hex}")
}

pub(crate) struct Base64Bytes(bytes::Bytes);

impl From<&bytes::Bytes> for Base64Bytes {
    fn from(value: &bytes::Bytes) -> Self {
        Self(value.clone())
    }
}

impl serde::Serialize for Base64Bytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&base64::display::Base64Display::new(
            &self.0,
            &base64::engine::general_purpose::STANDARD,
        ))
    }
}

#[derive(Clone)]
pub(crate) enum WireImageUrl {
    Url(String),
    Inline {
        media_type: String,
        data: bytes::Bytes,
    },
}

impl std::fmt::Display for WireImageUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(url) => formatter.write_str(url),
            Self::Inline { media_type, data } => write!(
                formatter,
                "data:{media_type};base64,{}",
                base64::display::Base64Display::new(
                    data,
                    &base64::engine::general_purpose::STANDARD,
                )
            ),
        }
    }
}

impl serde::Serialize for WireImageUrl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

/// Protocol-agnostic HTTP request components prepared by a codec.
pub(crate) struct HttpRequestParts {
    /// Target endpoint URL (fully resolved).
    pub url: url::Url,
    /// Request-specific HTTP headers.
    pub headers: http::HeaderMap,
    /// Serialized request body bytes.
    pub body: bytes::Bytes,
    /// Whether the request uses SSE streaming.
    pub streaming: bool,
    /// Pre-send validation diagnostics generated during building.
    pub diagnostics: Vec<crate::error::Diagnostic>,
}

/// Shared, offline test harness for the codec fixture suites (design §19).
#[cfg(test)]
mod harness;

#[cfg(test)]
mod inference_tests;
