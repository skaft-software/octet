//! Host-owned per-request runtime hooks.
//!
//! Pi's provider request options include credentials, a custom `fetch`, and
//! lifecycle callbacks (`onPayload`, `onResponse`, `transformHeaders`) that a
//! host may use for embedding, auditing, or request shaping. These are runtime
//! objects, not serializable request data: they must never travel through
//! canonical [`Request`](crate::Request) values, extension payloads, or
//! session records.
//!
//! HostRequestOptions carries those hooks for exactly one request attempt. The
//! client applies them on the built-in HTTP path only:
//!
//! * [`HostRequestOptions::api_key`] replaces an environment-backed credential for
//!   the declared auth scheme (never a fixed or signing credential);
//! * [`HostRequestOptions::transform_headers`] runs after endpoint/model/codec
//!   headers and before authentication, so request-aware signers still cover
//!   the final header set; reserved authentication/routing names are refused;
//! * [`HostRequestOptions::on_payload`] may inspect or replace the encoded JSON
//!   body before send;
//! * [`HostRequestOptions::on_response`] observes the provider's status and headers
//!   before the body is consumed;
//! * [`HostRequestOptions::fetch`] substitutes a host transport for this one
//!   request, exactly like a registered endpoint transport.
//!
//! The original hooks are synchronous. [`ProviderRequestHook`] adds optional,
//! cancellable async mediation at the real encoded HTTP boundaries. Neither lane
//! retries: a hook error fails the attempt. Private wire values never appear in
//! hook diagnostics or `Debug`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::auth::Secret;
use crate::error::{AiError, ConfigError};
use crate::host_transport::HostStreamTransport;

/// Maximum metadata entries accepted per request.
pub const MAX_RUNTIME_METADATA_ENTRIES: usize = 64;
/// Maximum serialized metadata size accepted per request.
pub const MAX_RUNTIME_METADATA_BYTES: usize = 8 * 1024;

/// Secret-free model context supplied to one hook invocation.
///
/// It is available for both catalog chat models and image-generation models, so
/// a hook can never depend on credentials, headers, or a base URL.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HookModelContext {
    /// Canonical selected model identifier.
    pub id: String,
    /// Provider/endpoint identifier.
    pub provider: String,
    /// Wire api identifier (for example `openai-chat` or
    /// `openrouter-images`).
    pub api: String,
}

impl HookModelContext {
    /// Builds the context for a catalog chat model.
    pub fn from_model(model: &crate::catalog::Model) -> Self {
        Self {
            id: model.spec.id.0.clone(),
            provider: model.endpoint.id.0.clone(),
            api: protocol_api_id(model.spec.protocol).to_owned(),
        }
    }
}

fn protocol_api_id(protocol: crate::types::Protocol) -> &'static str {
    match protocol {
        crate::types::Protocol::OpenAiResponses => "openai-responses",
        crate::types::Protocol::OpenAiChat => "openai-chat",
        crate::types::Protocol::AnthropicMessages => "anthropic-messages",
        crate::types::Protocol::BedrockConverse => "bedrock-converse-stream",
        crate::types::Protocol::GoogleGenerativeAi => "google-generative-ai",
        crate::types::Protocol::MistralConversations => "mistral-conversations",
        crate::types::Protocol::PiMessages => "pi-messages",
    }
}

/// Rewrites outbound headers for one request.
///
/// The hook runs after endpoint, model-preset, and codec headers and before
/// authentication, so signed requests cover the transformed set. Return an
/// error to fail the attempt; there is no implicit retry.
pub trait HeaderTransform: Send + Sync {
    /// Transforms `headers` in place.
    ///
    /// Reserved names (`authorization`, `host`, `content-length`, and any
    /// `x-amz-*` signing header) are rejected by the client before dispatch.
    fn transform_headers(
        &self,
        headers: &mut http::HeaderMap,
        model: &HookModelContext,
    ) -> Result<(), AiError>;
}

/// Inspects or replaces one encoded JSON request body.
pub trait PayloadHook: Send + Sync {
    /// Receives the decoded payload and the secret-free model view.
    ///
    /// `Ok(None)` keeps the payload unchanged; `Ok(Some(value))` replaces it.
    /// The replacement must remain a bounded JSON object or array; the client
    /// rejects a non-JSON or oversized result before dispatch.
    fn on_payload(
        &self,
        payload: serde_json::Value,
        model: &HookModelContext,
    ) -> Result<Option<serde_json::Value>, AiError>;
}

/// Observes one provider HTTP response before its body is consumed.
///
/// The hook is advisory: it cannot replace the response, cannot retry, and any
/// secret in provider headers remains the caller's responsibility. It is not
/// invoked for WebSocket transports, which have no HTTP response.
pub trait ResponseHook: Send + Sync {
    /// Observes the provider's status and response headers.
    fn on_response(
        &self,
        status: http::StatusCode,
        headers: &http::HeaderMap,
        model: &HookModelContext,
    );
}

/// Secret-free identity shared by the three callbacks of one HTTP attempt.
///
/// The operation ID changes on every call, including a host-authorized retry.
/// It is correlation, not authority to replay or change routes/credentials.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProviderRequestContext {
    /// Host-generated, process-local attempt identity.
    pub operation_id: String,
    /// Selected model without endpoint URLs, headers or credentials.
    pub model: HookModelContext,
}

/// Optional async mediation at actual HTTP request and response boundaries.
///
/// Callbacks are awaited in registration order, outside executor-blocking or
/// paint code. Dropping an opening future cancels its in-flight callback. Each
/// callback phase has a bounded deadline. Payloads and headers are private: an
/// implementation must not put them in ordinary logs, session records or UI.
/// HTTP is selected instead of preferred WebSockets when this lane is active;
/// opaque host transports and native steering refuse it rather than inventing
/// an encoded payload or HTTP response. There is no automatic retry.
#[async_trait::async_trait]
pub trait ProviderRequestHook: Send + Sync {
    /// Inspect or replace the actual codec-produced JSON body, before signing.
    /// `None` leaves it unchanged; replacements must be bounded objects/arrays.
    async fn before_request(
        &self,
        _context: &ProviderRequestContext,
        _payload: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, AiError> {
        Ok(None)
    }

    /// Mutate final endpoint/model/codec headers before authoritative auth.
    /// Insertions, replacements and deletions are applied. Reserved auth,
    /// routing, framing and signing names cannot be changed or removed.
    async fn before_headers(
        &self,
        _context: &ProviderRequestContext,
        _headers: &mut http::HeaderMap,
    ) -> Result<(), AiError> {
        Ok(())
    }

    /// Observe actual HTTP status/headers, success or error, before body reads.
    /// This cannot replace the response or authorize a retry.
    async fn after_response(
        &self,
        _context: &ProviderRequestContext,
        _status: http::StatusCode,
        _headers: &http::HeaderMap,
    ) -> Result<(), AiError> {
        Ok(())
    }
}

/// Host-owned runtime hooks for exactly one request attempt.
///
/// The default value is inert and preserves the ordinary client behavior.
#[derive(Default)]
pub struct HostRequestOptions {
    /// Per-request credential override for an environment-backed auth scheme.
    ///
    /// It is only accepted when the selected endpoint's auth declares an
    /// environment variable for the header (Bearer/API-key style). A fixed
    /// secret, dynamic resolver, request signer, or unauthenticated endpoint
    /// refuses the override rather than silently ignoring it.
    pub api_key: Option<Secret>,
    /// Request-local provider metadata.
    ///
    /// Built-in codecs currently consume no metadata field, so a non-empty map
    /// on the built-in HTTP path is refused until the declaration/codec owner
    /// adds the wire field; host transports receive no metadata at all.
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// Header transform hook.
    pub transform_headers: Option<Arc<dyn HeaderTransform>>,
    /// Payload hook.
    pub on_payload: Option<Arc<dyn PayloadHook>>,
    /// Response hook.
    pub on_response: Option<Arc<dyn ResponseHook>>,
    /// Async HTTP boundary hooks, after client-bound hooks in each phase.
    pub provider_hooks: Vec<Arc<dyn ProviderRequestHook>>,
    /// Per-request transport override, used instead of the endpoint's
    /// registered host transport for this attempt only.
    pub fetch: Option<Arc<dyn HostStreamTransport>>,
}

impl std::fmt::Debug for HostRequestOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostRequestOptions")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("metadata_entries", &self.metadata.len())
            .field("transform_headers", &self.transform_headers.is_some())
            .field("on_payload", &self.on_payload.is_some())
            .field("on_response", &self.on_response.is_some())
            .field("provider_hook_count", &self.provider_hooks.len())
            .field("fetch", &self.fetch.is_some())
            .finish()
    }
}

impl HostRequestOptions {
    /// Whether this runtime carries no hook, credential, or metadata.
    pub fn is_empty(&self) -> bool {
        self.api_key.is_none()
            && self.metadata.is_empty()
            && self.transform_headers.is_none()
            && self.on_payload.is_none()
            && self.on_response.is_none()
            && self.provider_hooks.is_empty()
            && self.fetch.is_none()
    }

    /// Whether this runtime carries any built-in-wire hook or credential.
    ///
    /// [`Self::fetch`] is deliberately excluded: it selects a host transport
    /// instead of shaping the built-in wire path.
    pub fn has_wire_hooks(&self) -> bool {
        self.api_key.is_some()
            || !self.metadata.is_empty()
            || self.transform_headers.is_some()
            || self.on_payload.is_some()
            || self.on_response.is_some()
            || !self.provider_hooks.is_empty()
    }

    /// Rejects an unbounded, empty, or unserializable runtime.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(api_key) = &self.api_key {
            if api_key.is_empty() {
                return Err(ConfigError::Parse(
                    "per-request api key override must not be empty".to_owned(),
                ));
            }
        }
        if self.metadata.len() > MAX_RUNTIME_METADATA_ENTRIES {
            return Err(ConfigError::Parse(format!(
                "per-request metadata exceeds the {MAX_RUNTIME_METADATA_ENTRIES}-entry limit"
            )));
        }
        if !self.metadata.is_empty() {
            let encoded = serde_json::to_vec(&self.metadata).map_err(|error| {
                ConfigError::Parse(format!("invalid per-request metadata: {error}"))
            })?;
            if encoded.len() > MAX_RUNTIME_METADATA_BYTES {
                return Err(ConfigError::Parse(format!(
                    "per-request metadata exceeds the {MAX_RUNTIME_METADATA_BYTES}-byte limit"
                )));
            }
        }
        Ok(())
    }
}

/// Whether `name` is reserved from hook transformation.
///
/// These names carry authentication, transport framing, or signing authority;
/// a hook must not be able to forge, suppress, or corrupt them.
pub(crate) fn is_reserved_header(name: &http::HeaderName) -> bool {
    name == http::header::AUTHORIZATION
        || name == http::header::HOST
        || name == http::header::CONTENT_LENGTH
        || name.as_str().starts_with("x-amz-")
}
