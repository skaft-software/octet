//! The `AiClient`: one entry point for every provider request.
//!
//! `AiClient` is the whole public surface of the request path. It resolves a
//! [`Model`] into a dispatched request and is the only place that knows which
//! of several transports an endpoint asked for. The transports themselves live
//! in sibling modules, one per boundary:
//!
//! - `hooks` — the host payload-hook and host-transport seams.
//! - `transport` — reqwest failure classification and the body-read clocks.
//! - `diagnostics` — redaction and size bounds for anything a provider sent.
//! - `stream` — opening a streaming request, decoding SSE and Bedrock frames.
//! - `websocket` — the steerable Responses WebSocket transport and its resume.
//! - `batch` — the OpenRouter batch HTTP surface.
//!
//! What stays here is the part that is genuinely a client: the public entry
//! points, and the decisions about which transport an endpoint supports. That
//! list is short on purpose — every helper beneath it can now be read without
//! first knowing how a response is dispatched.

mod batch;
mod diagnostics;
mod hooks;
mod stream;
mod transport;
mod websocket;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use async_stream::try_stream;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::auth::CredentialRedactor;
use crate::catalog::Model;
use crate::deferred::DeferredHandle;
use crate::error::{
    AiError, DecodeError, HttpError, StreamProtocolError, TransportError, TransportPhase,
};
use crate::host_transport::{HostStreamModel, HostStreamTransport};
use crate::responses_ws::{ResponsesWsLiveness, ResponsesWsPool};
use crate::runtime::{HookModelContext, HostRequestOptions, ProviderRequestHook};
use crate::stream::{ResponseStream, StreamEvent};
use crate::types::{EndpointId, Protocol, Request, Response};
use crate::{ResponsesCompactRequest, ResponsesCompactResponse};

struct FirstRequestActions {
    observed: bool,
    actions: Vec<Box<dyn FnOnce() + Send + 'static>>,
}

static FIRST_REQUEST_ACTIONS: OnceLock<StdMutex<FirstRequestActions>> = OnceLock::new();

fn first_request_actions() -> &'static StdMutex<FirstRequestActions> {
    FIRST_REQUEST_ACTIONS.get_or_init(|| {
        StdMutex::new(FirstRequestActions {
            observed: false,
            actions: Vec::new(),
        })
    })
}

/// Defer startup work until this process has opened its first inference request.
///
/// Hosts use this for optional background work that must not add a provider
/// request ahead of the user's first turn. If inference has already started,
/// `action` runs immediately.
pub fn defer_until_first_request(action: impl FnOnce() + Send + 'static) {
    let action: Box<dyn FnOnce() + Send + 'static> = Box::new(action);
    let action = {
        let mut state = first_request_actions()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.observed {
            Some(action)
        } else {
            state.actions.push(action);
            None
        }
    };
    if let Some(action) = action {
        action();
    }
}

fn notify_first_request_opened() {
    let actions = {
        let mut state = first_request_actions()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.observed {
            return;
        }
        state.observed = true;
        std::mem::take(&mut state.actions)
    };
    for action in actions {
        action();
    }
}

use self::batch::{
    batch_http_request, openrouter_batch_item_url, openrouter_batch_url, MAX_BATCH_BODY_BYTES,
};
// Re-exported rather than re-homed: both are crate-internal and are already
// addressed as `crate::client::` from `images` and `responses_ws`.
pub(crate) use self::diagnostics::sanitize_ai_error;
use self::hooks::{
    apply_payload_hook, merge_preset_headers, prepare_host_request, validate_hook_headers,
    ProviderRequestAttempt,
};
use self::stream::{stream_http, websocket_open_failure_is_replay_safe, HttpStreamRequest};
pub(crate) use self::transport::DEFAULT_CONNECT_TIMEOUT;
use self::transport::{
    next_body_chunk, request_open_transport_error, DEFAULT_STREAM_DEADLINE,
    DEFAULT_STREAM_IDLE_TIMEOUT, DEFAULT_STREAM_INITIAL_TIMEOUT, MAX_COMPLETED_BODY_BYTES,
    MAX_ERROR_BODY_DEADLINE, MAX_ERROR_BODY_IDLE_TIMEOUT,
};
use self::websocket::{
    responses_websocket_key, responses_websocket_stream, steering_event_stream, ResponsesResume,
};
// Request-group scoped only: never installed back into the catalog or agent.
struct SettledCredential(crate::auth::ResolvedCredential);

#[async_trait::async_trait]
impl crate::auth::CredentialResolver for SettledCredential {
    async fn resolve(&self) -> Result<crate::auth::ResolvedCredential, crate::AuthError> {
        Ok(crate::auth::ResolvedCredential {
            scheme: match &self.0.scheme {
                crate::auth::CredentialScheme::Bearer => crate::auth::CredentialScheme::Bearer,
                crate::auth::CredentialScheme::Header(name) => {
                    crate::auth::CredentialScheme::Header(name.clone())
                }
            },
            value: self.0.value.clone(),
            extra_headers: self.0.extra_headers.clone(),
        })
    }
}

/// A native compact response whose HTTP headers have actually arrived.
///
/// Both successful and non-success statuses reach this boundary before body or
/// optional error-snippet reads. Dropping this value cancels the body; neither
/// opening nor completion retries the request or resolves credentials again.
#[must_use = "complete the response body or drop it to cancel"]
pub struct PendingResponsesCompact {
    response: reqwest::Response,
    diagnostic_redactor: CredentialRedactor,
    stream_idle_timeout: Duration,
    stream_initial_timeout: Duration,
    stream_deadline: Duration,
    opened_at: Instant,
}

impl std::fmt::Debug for PendingResponsesCompact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingResponsesCompact")
            .finish_non_exhaustive()
    }
}

impl PendingResponsesCompact {
    /// Reads the bounded body under its own phase-specific timeouts.
    /// Non-success responses retain their HTTP status and bounded diagnostics.
    pub async fn complete(self) -> Result<ResponsesCompactResponse, AiError> {
        let response = self.response;
        let diagnostic_redactor = self.diagnostic_redactor;
        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .or_else(|| response.headers().get("request-id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
        if !status.is_success() {
            let mut body = Vec::with_capacity(4096);
            let mut body_stream = response.bytes_stream();
            let started_at = self.opened_at;
            while body.len() < 4096 {
                match next_body_chunk(
                    &mut body_stream,
                    self.stream_idle_timeout.min(MAX_ERROR_BODY_IDLE_TIMEOUT),
                    self.stream_idle_timeout.min(MAX_ERROR_BODY_IDLE_TIMEOUT),
                    false,
                    started_at,
                    self.stream_deadline.min(MAX_ERROR_BODY_DEADLINE),
                    "compact HTTP error response body",
                )
                .await
                {
                    Ok(Some(chunk)) => {
                        let remaining = 4096 - body.len();
                        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            let snippet = String::from_utf8_lossy(&body).into_owned();
            let provider_code = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("error")
                        .and_then(|error| error.get("code"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
            let retryable = matches!(
                status,
                http::StatusCode::REQUEST_TIMEOUT
                    | http::StatusCode::TOO_MANY_REQUESTS
                    | http::StatusCode::BAD_GATEWAY
                    | http::StatusCode::SERVICE_UNAVAILABLE
                    | http::StatusCode::GATEWAY_TIMEOUT
            );
            return Err(sanitize_ai_error(
                &diagnostic_redactor,
                HttpError {
                    status,
                    request_id,
                    retry_after,
                    provider_code,
                    body_snippet: (!snippet.is_empty()).then_some(snippet),
                    retryable,
                }
                .into(),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_COMPLETED_BODY_BYTES as u64)
        {
            return Err(DecodeError::BodyTooLarge.into());
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or_default()
                .min(MAX_COMPLETED_BODY_BYTES as u64) as usize,
        );
        let mut body_stream = response.bytes_stream();
        let mut first_body_chunk = true;
        let started_at = self.opened_at;
        while let Some(chunk) = next_body_chunk(
            &mut body_stream,
            self.stream_idle_timeout,
            self.stream_initial_timeout,
            first_body_chunk,
            started_at,
            self.stream_deadline,
            "compact response body",
        )
        .await
        .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?
        {
            first_body_chunk = false;
            if body
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size > MAX_COMPLETED_BODY_BYTES)
            {
                return Err(DecodeError::BodyTooLarge.into());
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|error| {
            sanitize_ai_error(
                &diagnostic_redactor,
                AiError::Decode(DecodeError::Json(error.to_string())),
            )
        })
    }
}
/// Client wrapper for executing AI service requests.
#[derive(Clone)]
pub struct AiClient {
    http: reqwest::Client,
    proxy_environment: Option<Arc<crate::declarations::proxy::ProxyEnvironment>>,
    responses_ws: ResponsesWsPool,
    host_stream_transports: Arc<StdMutex<HashMap<EndpointId, Arc<dyn HostStreamTransport>>>>,
    stream_initial_timeout: Duration,
    stream_idle_timeout: Duration,
    stream_deadline: Duration,
    request_dispatch: Option<Arc<std::sync::atomic::AtomicBool>>,
    provider_request_hooks: Vec<Arc<dyn ProviderRequestHook>>,
}

impl Default for AiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AiClient {
    /// Clones this client, appending an owner-bound async HTTP hook chain.
    ///
    /// Existing clients and their shared transport registry remain unchanged.
    /// Hooks apply to conversational requests and native HTTP compaction, not
    /// batch/image/deferred services. Preferred WebSockets use HTTP while hooks
    /// are installed; opaque host transports/native steering fail explicitly.
    pub fn with_provider_request_hooks(&self, hooks: Vec<Arc<dyn ProviderRequestHook>>) -> Self {
        let mut client = self.clone();
        client.provider_request_hooks.extend(hooks);
        client
    }

    /// Whether this client has an async provider-boundary subscriber.
    pub fn has_provider_request_hooks(&self) -> bool {
        !self.provider_request_hooks.is_empty()
    }

    /// Clones this client with fresh, sticky dispatch tracking for one attempt.
    /// The original client is unaffected. Do not reuse this clone for a new attempt.
    pub fn track_request_dispatch(&self) -> Self {
        let mut client = self.clone();
        client.request_dispatch = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
        client
    }

    /// Whether a tracked attempt may have reached a provider or opaque host transport.
    /// False is meaningful only on a fresh tracked clone. True is conservative:
    /// it establishes neither actual transmission nor acceptance or billing.
    pub fn request_may_have_been_sent(&self) -> bool {
        self.request_dispatch
            .as_ref()
            .is_some_and(|state| state.load(std::sync::atomic::Ordering::Acquire))
    }

    pub(crate) fn mark_request_dispatch(&self) {
        if let Some(state) = &self.request_dispatch {
            state.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Creates a new AiClient using the default reqwest client.
    ///
    /// [`Self::try_new`] is available to callers that need to handle client
    /// construction errors. This convenience constructor fails loudly rather
    /// than silently replacing the explicit no-redirect policy with reqwest's
    /// redirect-following default.
    pub fn new() -> Self {
        Self::try_new().expect("failed to initialize the octet HTTP client")
    }

    /// Creates a new AiClient, preserving octet's no-redirect transport policy.
    ///
    /// Reqwest has no useful generation deadline by itself. octet applies the
    /// endpoint timeout while waiting for headers, then allows a generous
    /// first body chunk before enforcing inter-chunk idle and overall body
    /// deadlines in [`Self::stream`].
    pub fn try_new() -> Result<Self, reqwest::Error> {
        let env = crate::declarations::proxy::ProxyEnvironment::NAMES
            .into_iter()
            .filter_map(|name| {
                std::env::var_os(name)
                    .map(|value| (name.to_owned(), value.to_string_lossy().into_owned()))
            })
            .collect();
        Self::try_with_proxy_environment(env)
    }

    /// Creates the ordinary no-redirect client with an explicit proxy environment
    /// instead of process/OS proxy settings. The eight HTTP(S)/ALL/NO_PROXY names
    /// are snapshotted; an empty map disables proxying. Malformed/unsupported
    /// proxies fail before credentials or dispatch, never fall through to direct
    /// egress. Preferred WebSocket routes use HTTP when a proxy is selected.
    pub fn try_with_proxy_environment(
        env: std::collections::BTreeMap<String, String>,
    ) -> Result<Self, reqwest::Error> {
        let proxy_environment = Arc::new(crate::declarations::proxy::ProxyEnvironment::new(env));
        Ok(Self {
            http: proxy_environment
                .clone()
                .configure(
                    reqwest::Client::builder()
                        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
                        .redirect(reqwest::redirect::Policy::none()),
                )
                .build()?,
            proxy_environment: Some(proxy_environment),
            responses_ws: ResponsesWsPool::default(),
            host_stream_transports: Arc::new(StdMutex::new(HashMap::new())),
            stream_initial_timeout: DEFAULT_STREAM_INITIAL_TIMEOUT,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            stream_deadline: DEFAULT_STREAM_DEADLINE,
            request_dispatch: None,
            provider_request_hooks: Vec::new(),
        })
    }

    /// Creates an AiClient wrapping a custom reqwest HTTP client.
    pub fn with_http_client(http: reqwest::Client) -> Self {
        Self {
            http,
            proxy_environment: None,
            responses_ws: ResponsesWsPool::default(),
            host_stream_transports: Arc::new(StdMutex::new(HashMap::new())),
            stream_initial_timeout: DEFAULT_STREAM_INITIAL_TIMEOUT,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            stream_deadline: DEFAULT_STREAM_DEADLINE,
            request_dispatch: None,
            provider_request_hooks: Vec::new(),
        }
    }

    fn request_proxy(&self, target: &url::Url) -> Result<Option<url::Url>, AiError> {
        self.proxy_environment
            .as_ref()
            .map(|env| env.resolve(target))
            .transpose()
            .map(Option::flatten)
    }

    /// Registers a host-owned stream transport for one catalog endpoint.
    ///
    /// The registration is shared by clones of this client. The transport sees
    /// only canonical request data and [`crate::HostStreamModel`]; resolved
    /// credentials, endpoint URLs, and headers are deliberately not exposed.
    /// Replacing a transport is an explicit host authority action.
    pub fn register_host_stream_transport(
        &self,
        endpoint: EndpointId,
        transport: Arc<dyn HostStreamTransport>,
    ) {
        self.host_stream_transports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(endpoint, transport);
    }

    /// Removes a host-owned stream transport for one endpoint.
    pub fn remove_host_stream_transport(&self, endpoint: &EndpointId) {
        self.host_stream_transports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(endpoint);
    }

    /// Sets the maximum quiet interval and absolute lifetime of response bodies,
    /// including SSE and completed JSON. Bounded HTTP error snippets also obey
    /// these values but retain shorter internal ceilings so an already-known
    /// status is surfaced promptly.
    /// The idle value also bounds the first body chunk; callers that need a
    /// longer time-to-first-body-byte can override it with
    /// [`Self::with_initial_stream_timeout`].
    /// Callers can use shorter values in tests or batch workers.
    pub fn with_stream_timeouts(mut self, idle_timeout: Duration, deadline: Duration) -> Self {
        let idle_timeout = idle_timeout.max(Duration::from_millis(1));
        self.stream_initial_timeout = idle_timeout;
        self.stream_idle_timeout = idle_timeout;
        self.stream_deadline = deadline.max(Duration::from_millis(1));
        self
    }

    /// Sets the maximum time allowed for the first successful response-body
    /// chunk after headers arrive, or the first decoded WebSocket event. This is
    /// independent from the shorter inter-chunk idle timeout and is useful for
    /// large prompts or cold local model servers. Bounded error bodies use a
    /// separate short ceiling so their already-known HTTP status is surfaced
    /// promptly.
    pub fn with_initial_stream_timeout(mut self, timeout: Duration) -> Self {
        self.stream_initial_timeout = timeout.max(Duration::from_millis(1));
        self
    }

    /// Executes one provider request and returns a pinned stream of events.
    ///
    /// This transport deliberately performs no automatic retries. Callers own
    /// retry count, backoff, cancellation, and idempotency policy; structured
    /// HTTP errors retain `retry_after` and `retryable` metadata for that use.
    pub async fn stream(&self, model: &Model, req: Request) -> Result<ResponseStream, AiError> {
        self.stream_with_overrides(model, req, crate::RequestOverrides::default())
            .await
    }

    /// Executes one inference attempt with private request-local configuration.
    ///
    /// Sampling overrides replace model defaults (explicit canonical stop wins).
    /// Header precedence is endpoint < model < caller < codec < authoritative auth.
    /// Environment values overlay only this request; process state and the
    /// catalog remain unchanged. Nonzero retry controls are explicitly refused.
    /// `timeout_ms` bounds opening plus body lifetime, including credential waits.
    /// Transport-specific overrides cannot cross a host/extension transport.
    pub async fn stream_with_overrides(
        &self,
        model: &Model,
        req: Request,
        overrides: crate::RequestOverrides,
    ) -> Result<ResponseStream, AiError> {
        self.stream_with_host_options(model, req, overrides, HostRequestOptions::default())
            .await
    }

    /// Executes one inference attempt with private request-local configuration
    /// and host-owned runtime options.
    ///
    /// [`HostRequestOptions`] carries per-request hooks and a credential
    /// override that must never become canonical request data. The built-in
    /// HTTP path applies them; a host stream transport refuses them so a hook
    /// can never observe or rewrite wire material it does not own.
    pub async fn stream_with_host_options(
        &self,
        model: &Model,
        mut req: Request,
        overrides: crate::RequestOverrides,
        host_options: HostRequestOptions,
    ) -> Result<ResponseStream, AiError> {
        let started = Instant::now();
        overrides
            .validate()
            .map_err(|error| crate::ConfigError::Parse(error.to_string()))?;
        host_options.validate()?;
        if !host_options.metadata.is_empty() {
            return Err(crate::ConfigError::Parse(
                "per-request metadata is unsupported until the selected codec declares a wire field"
                    .into(),
            )
            .into());
        }
        if overrides.max_retries.unwrap_or(0) != 0 || overrides.max_retry_delay_ms.unwrap_or(0) != 0
        {
            return Err(crate::ConfigError::Parse(
                "client retries are host-owned; nonzero retry overrides are unsupported".into(),
            )
            .into());
        }
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if host_options.api_key.is_some()
            && matches!(&model.endpoint.auth, crate::auth::Auth::RequestSigner(_))
        {
            return Err(crate::ConfigError::Parse(
                "a per-request api key override cannot be applied to a request-aware signer".into(),
            )
            .into());
        }
        let host_transport = host_options.fetch.is_some()
            || self
                .host_stream_transports
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains_key(&model.endpoint.id);
        if host_transport
            && (!overrides.headers.is_empty()
                || !overrides.env.is_empty()
                || !overrides.sampling_params.is_empty()
                || overrides.azure.is_some()
                || overrides.codex_transport.is_some()
                || overrides.codex_connect_timeout_ms.is_some())
        {
            return Err(crate::ConfigError::Parse(
                "wire overrides are unsupported by a host stream transport".into(),
            )
            .into());
        }
        if host_transport && (host_options.has_wire_hooks() || self.has_provider_request_hooks()) {
            return Err(crate::ConfigError::Parse(
                "per-request api key, metadata, and payload/header/response hooks are unsupported by a host stream transport".into(),
            )
            .into());
        }
        let mut model = model.clone();
        if !overrides.sampling_params.is_empty() || !overrides.headers.is_empty() {
            let preset = &mut Arc::make_mut(&mut model.spec).preset;
            preset
                .sampling_params
                .extend(overrides.sampling_params.clone());
            for (name, value) in &overrides.headers {
                preset
                    .headers
                    .retain(|old, _| !old.eq_ignore_ascii_case(name));
                preset.headers.insert(name.clone(), value.clone());
            }
            // Explicit sampling temperature is a per-call control, not a model
            // default; materialize it before ordinary canonical validation.
            if let Some(value) = overrides.sampling_params.get("temperature") {
                req.temperature = value.as_f64().map(|value| value as f32);
            }
        }
        if !host_transport {
            crate::declarations::azure::apply(
                &mut model,
                overrides.azure.as_ref(),
                &overrides.env,
            )?;
        }
        let mut client = self.with_environment_overlay(&overrides.env)?;
        let deadline = if let Some(timeout) = overrides.timeout_ms {
            let timeout = Duration::from_millis(timeout);
            let deadline = tokio::time::Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| {
                    crate::ConfigError::Parse("request timeout is not representable".into())
                })?;
            Arc::make_mut(&mut model.endpoint).timeout = timeout;
            client.stream_initial_timeout = client.stream_initial_timeout.min(timeout);
            client.stream_idle_timeout = client.stream_idle_timeout.min(timeout);
            client.stream_deadline = client.stream_deadline.min(timeout);
            Some(deadline)
        } else {
            None
        };
        let open = client.stream_once(&model, req, &overrides, &host_options);
        let Some(deadline) = deadline else {
            let stream = open.await?;
            notify_first_request_opened();
            return Ok(crate::inference::measured_stream(
                stream,
                started,
                crate::inference::ClientTimingScope::Request,
            ));
        };
        let stream = tokio::time::timeout_at(deadline, open)
            .await
            .map_err(|_| {
                AiError::Transport(TransportError {
                    phase: TransportPhase::ResponseHeaders,
                    timeout: true,
                    message: "request-local opening deadline exceeded".into(),
                })
            })??;
        notify_first_request_opened();
        let mut stream = crate::inference::measured_stream(
            stream,
            started,
            crate::inference::ClientTimingScope::Request,
        );
        Ok(Box::pin(try_stream! {
            loop {
                let item = tokio::time::timeout_at(deadline, stream.next()).await.map_err(|_| {
                    AiError::Transport(TransportError { phase: TransportPhase::Body, timeout: true, message: "request-local response deadline exceeded".into() })
                })?;
                let Some(item) = item else { break; };
                yield item?;
            }
        }))
    }

    /// Opens an explicitly multi-response, in-flight steering operation.
    ///
    /// Both model and endpoint must advertise steering and select native
    /// WebSockets. There is no HTTP fallback, reconnect or inference retry.
    /// Proxies, request signers and opaque host transports fail closed rather
    /// than bypassing their transport/authentication requirements.
    pub async fn steerable_responses(
        &self,
        model: &Model,
        mut req: Request,
    ) -> Result<crate::steering::SteeringSession, AiError> {
        use crate::steering::{SteeringControl, SteeringSession};
        if self.has_provider_request_hooks() {
            return Err(crate::ConfigError::Parse(
                "async HTTP provider hooks are unsupported by native steering".into(),
            )
            .into());
        }
        crate::steering::validate_request(&req)?;
        let started_at = Instant::now();
        let mut prepared = model.clone();
        crate::declarations::azure::apply(&mut prepared, None, &Default::default())?;
        let model = &prepared;
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if model.spec.endpoint != model.endpoint.id {
            return Err(crate::ConfigError::UnknownEndpoint(model.spec.endpoint.clone()).into());
        }
        if !model.responses_features().steering
            || model.spec.protocol != Protocol::OpenAiResponses
            || model.endpoint.transport != crate::EndpointTransport::WebSocketPreferred
            || matches!(&model.endpoint.auth, crate::Auth::RequestSigner(_))
            || self
                .host_stream_transports
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains_key(&model.endpoint.id)
        {
            return Err(crate::steering::invalid(
                "route is not qualified for native Responses steering",
            ));
        }
        req.messages = crate::transform::transform_request_messages_owned(req.messages, model);
        crate::json_repair::validate_tool_definitions(&req.tools).map_err(AiError::Decode)?;
        let parts = crate::protocol::openai_responses::build_request(model, &req)?;
        if parts.body.len() > 64 * 1024 * 1024 {
            return Err(DecodeError::ResponseTooLarge.into());
        }
        if self.request_proxy(&parts.url)?.is_some() || !parts.streaming {
            return Err(crate::steering::invalid(
                "native steering requires an unproxied streaming WebSocket route",
            ));
        }
        let mut headers = model.endpoint.default_headers.clone();
        merge_preset_headers(&mut headers, &model.spec.preset.headers)?;
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        for (key, value) in &parts.headers {
            headers.insert(key.clone(), value.clone());
        }
        let resolved = crate::auth::resolve_headers(&model.endpoint.auth)
            .await
            .map_err(AiError::Auth)?;
        let mut redactor = resolved.redactor;
        let mut current_key = None;
        for (key, value) in resolved.headers {
            if let Some(key) = key {
                current_key = Some(key.clone());
                headers.insert(key, value);
            } else if let Some(key) = &current_key {
                headers.append(key.clone(), value);
            }
        }
        redactor.include_header_values(&headers);
        if model
            .endpoint
            .runtime
            .responses_profile
            .sends_websocket_beta_header()
        {
            headers.insert(
                http::HeaderName::from_static("openai-beta"),
                http::HeaderValue::from_static(ResponsesWsPool::beta_header_value()),
            );
        }
        let mut body: serde_json::Value = serde_json::from_slice(&parts.body)
            .map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;
        if let Some(object) = body.as_object_mut() {
            object.remove("stream");
            object.remove("background");
        }
        let key = req
            .session_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| responses_websocket_key(model, id, &parts.url, &headers));
        let (sender, commands) = mpsc::channel(crate::steering::MAX_STEERS);
        let ledger = Arc::new(StdMutex::new(Vec::new()));
        let request = Arc::new(StdMutex::new(req));
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let operation = crate::responses_ws::SteeringOperation {
            commands,
            ledger: ledger.clone(),
            request: request.clone(),
            initial_timeout: self.stream_initial_timeout,
            idle_timeout: self.stream_idle_timeout,
            deadline: self.stream_deadline,
            cancel: cancelled,
            redactor: redactor.clone(),
        };
        self.mark_request_dispatch();
        let events = self
            .responses_ws
            .request_operation(
                key.as_deref(),
                parts.url,
                headers,
                body,
                ResponsesWsLiveness::for_response_idle(self.stream_idle_timeout),
                model.endpoint.timeout,
                Some(Duration::from_millis(
                    crate::declarations::codex::DEFAULT_CODEX_WEBSOCKET_CONNECT_TIMEOUT_MS,
                )),
                None,
                Some(operation),
            )
            .await
            .map_err(|e| sanitize_ai_error(&redactor, e))?;
        let completed = Arc::new(StdMutex::new(None));
        let control = SteeringControl {
            sender,
            ledger: ledger.clone(),
            model: model.clone(),
            completed: completed.clone(),
        };
        Ok(SteeringSession {
            control,
            cancel: Some(cancel),
            events: steering_event_stream(
                self.responses_ws.clone(),
                key,
                model.clone(),
                events,
                request,
                ledger,
                completed,
                parts.diagnostics,
                redactor,
                started_at,
            ),
        })
    }

    /// Open one inference after optional Responses setup, resolving a dynamic
    /// credential exactly once for both operations. Credential failures are
    /// inference opening failures, not ignorable setup failures. The optional
    /// socket timeout starts only after credential settlement.
    pub async fn stream_with_responses_prewarm(
        &self,
        model: &Model,
        request: Request,
        warm_request: Request,
    ) -> Result<ResponseStream, AiError> {
        if self.has_provider_request_hooks() {
            return self.stream(model, request).await;
        }
        let mut prepared = model.clone();
        crate::catalog::validate_endpoint(&prepared.endpoint)?;
        crate::catalog::validate_model_spec(&prepared.spec)?;
        // Validate before a potentially rotating credential exchange.
        crate::protocol::openai_responses::build_request(&prepared, &request)?;
        crate::protocol::openai_responses::build_request(&prepared, &warm_request)?;
        if let crate::Auth::Dynamic(resolver) = &prepared.endpoint.auth {
            let credential = resolver.resolve().await.map_err(AiError::Auth)?;
            Arc::make_mut(&mut prepared.endpoint).auth =
                crate::Auth::Dynamic(Arc::new(SettledCredential(credential)));
        }
        let timeout = prepared.endpoint.timeout.min(Duration::from_secs(30));
        let _ =
            tokio::time::timeout(timeout, self.prewarm_responses(&prepared, warm_request)).await;
        self.stream(&prepared, request).await
    }

    /// Best-effort prewarms a cached OpenAI Responses WebSocket.
    ///
    /// The request is sent with the provider-specific `generate=false` flag,
    /// so this establishes connection/continuation state without consuming a
    /// model turn. Callers can invoke it from an input/composer task and ignore
    /// the result; ordinary [`Self::stream`] calls always retain HTTP/SSE
    /// fallback behavior.
    pub async fn prewarm_responses(&self, model: &Model, req: Request) -> Result<(), AiError> {
        if self.has_provider_request_hooks() {
            // The hooked request uses HTTP; do not create an unobserved socket.
            return Ok(());
        }
        let mut prepared = model.clone();
        crate::declarations::azure::apply(&mut prepared, None, &Default::default())?;
        let model = &prepared;
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if model.spec.endpoint != model.endpoint.id {
            return Err(crate::ConfigError::UnknownEndpoint(model.spec.endpoint.clone()).into());
        }
        if !matches!(
            model.endpoint.transport,
            crate::types::EndpointTransport::WebSocketPreferred
        ) || model.spec.protocol != Protocol::OpenAiResponses
        {
            return Ok(());
        }
        let session_id = req.session_id.clone().filter(|id| !id.is_empty());
        let Some(session_id) = session_id else {
            return Ok(());
        };
        let mut req = req;
        req.messages = crate::transform::transform_request_messages_owned(req.messages, model);
        crate::json_repair::validate_tool_definitions(&req.tools).map_err(AiError::Decode)?;
        let parts = crate::protocol::openai_responses::build_request(model, &req)?;
        if self.request_proxy(&parts.url)?.is_some() {
            return Ok(());
        }
        let mut headers = http::HeaderMap::new();
        for (key, value) in &model.endpoint.default_headers {
            headers.insert(key.clone(), value.clone());
        }
        merge_preset_headers(&mut headers, &model.spec.preset.headers)?;
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        for (key, value) in &parts.headers {
            headers.insert(key.clone(), value.clone());
        }
        let resolved_headers = crate::auth::resolve_headers(&model.endpoint.auth)
            .await
            .map_err(AiError::Auth)?;
        let mut diagnostic_redactor = resolved_headers.redactor;
        diagnostic_redactor.include_header_values(&headers);
        let mut current_key = None;
        for (key, value) in resolved_headers.headers {
            if let Some(key) = key {
                current_key = Some(key.clone());
                headers.insert(key, value);
            } else if let Some(key) = &current_key {
                headers.append(key.clone(), value);
            }
        }
        if model
            .endpoint
            .runtime
            .responses_profile
            .sends_websocket_beta_header()
        {
            headers.insert(
                http::HeaderName::from_static("openai-beta"),
                http::HeaderValue::from_static(ResponsesWsPool::beta_header_value()),
            );
        }
        let body = serde_json::from_slice::<serde_json::Value>(&parts.body)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
        let key = responses_websocket_key(model, &session_id, &parts.url, &headers);
        let result = self
            .responses_ws
            .prewarm(
                &key,
                parts.url,
                headers,
                body,
                ResponsesWsLiveness::for_response_idle(self.stream_idle_timeout),
                model.endpoint.timeout.min(DEFAULT_CONNECT_TIMEOUT),
            )
            .await;
        result.map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))
    }

    async fn stream_once(
        &self,
        model: &Model,
        req: Request,
        overrides: &crate::RequestOverrides,
        host_options: &HostRequestOptions,
    ) -> Result<ResponseStream, AiError> {
        let environment = &overrides.env;
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if model.spec.endpoint != model.endpoint.id {
            return Err(crate::ConfigError::UnknownEndpoint(model.spec.endpoint.clone()).into());
        }

        let host_transport = host_options.fetch.clone().or_else(|| {
            self.host_stream_transports
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&model.endpoint.id)
                .cloned()
        });
        if let Some(transport) = host_transport {
            // Keep host-mediated transports on the canonical side of the same
            // replay-history and capability boundary as HTTP codecs. Unlike a
            // protocol codec they cannot safely perform lossy wire-specific
            // degradation, so validate strictly rather than exposing an
            // unsupported canonical feature to an extension transport.
            let (request, diagnostics) = prepare_host_request(model, req)?;
            self.mark_request_dispatch();
            let stream = transport
                .stream(HostStreamModel::from(model), request, diagnostics)
                .await?;
            return Ok(crate::stream::guard(stream));
        }

        // Derive target-compatible replay history without mutating the caller's
        // canonical conversation. This must happen before strict validation:
        // cross-model reasoning, unsupported historical media, and interrupted
        // tool turns are normalized into valid canonical messages first.
        let mut req = req;
        req.messages = crate::transform::transform_request_messages_owned(req.messages, model);
        let tool_definitions = req.tools.clone();
        // Reject malformed schemas before a provider request can consume them.
        // The same immutable snapshot is retained by response assembly below.
        crate::json_repair::validate_tool_definitions(&tool_definitions)
            .map_err(AiError::Decode)?;
        // Ambiguous bare JSON must remain visible in the default strict stream.
        // Lossy mode is the explicit opt-in for holding it to EOF and
        // interpreting a provider's text as compatibility tool syntax.
        let buffer_ambiguous_compatibility_content =
            req.compatibility == crate::CompatibilityMode::Lossy;

        let requested_audio_format = match &req.output_modalities {
            crate::types::OutputModalities::TextAndAudio(options) => Some(options.format),
            crate::types::OutputModalities::Text => None,
        };
        // 1. Build the HTTP request parts via the protocol codec
        let mut parts = match model.spec.protocol {
            Protocol::OpenAiChat => crate::protocol::openai_chat::build_request(model, &req)?,
            Protocol::AnthropicMessages => crate::protocol::anthropic::build_request(model, &req)?,
            Protocol::OpenAiResponses => {
                crate::protocol::openai_responses::build_request(model, &req)?
            }
            Protocol::BedrockConverse => crate::protocol::bedrock::build_request(model, &req)?,
            Protocol::GoogleGenerativeAi => crate::protocol::google::build_request(model, &req)?,
            Protocol::MistralConversations => {
                crate::protocol::mistral_conversations::build_request(model, &req)?
            }
            Protocol::PiMessages => crate::protocol::pi_messages::build_request(model, &req)?,
        };

        // A host payload hook sees and may replace exactly the encoded JSON the
        // codec produced, before any credential is attached or a byte is sent.
        if let Some(hook) = &host_options.on_payload {
            parts.body = apply_payload_hook(hook, model, parts.body)?;
        }
        let provider_hooks = ProviderRequestAttempt::new(
            model,
            &self.provider_request_hooks,
            &host_options.provider_hooks,
        );
        if let Some(hooks) = &provider_hooks {
            parts.body = hooks.payload(parts.body).await?;
        }

        let proxy = self.request_proxy(&parts.url)?;

        // Pre-send Lossy diagnostics (capability drops computed in `build_request`)
        // must reach the terminal `Finished` response (design §7). Capture them
        // here and seed the assembly with them below.
        let pre_send_diagnostics = parts.diagnostics.clone();

        // 2. Compose headers in precedence order:
        //    a. Endpoint default headers
        //    b. Request-specific/codec headers
        //    c. Dynamic/Resolved auth headers
        let mut headers = http::HeaderMap::new();

        for (k, v) in &model.endpoint.default_headers {
            headers.insert(k.clone(), v.clone());
        }
        merge_preset_headers(&mut headers, &model.spec.preset.headers)?;

        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        for (k, v) in &parts.headers {
            headers.insert(k.clone(), v.clone());
        }

        // A host header transform runs after endpoint/model/codec headers and
        // before authentication, so request-aware signers still cover the
        // final set. It cannot add or change authentication, host, framing, or
        // signing headers.
        if let Some(transform) = &host_options.transform_headers {
            let before = headers.clone();
            transform.transform_headers(&mut headers, &HookModelContext::from_model(model))?;
            validate_hook_headers(&before, &headers)?;
        }
        if let Some(hooks) = &provider_hooks {
            hooks.headers(&mut headers).await?;
        }

        // Request-aware signers (SigV4) must run after body encoding, so the
        // exact body and final header set are covered. Ordinary auth remains
        // resolved here so the Responses WebSocket path can use it directly.
        let request_aware_signer =
            matches!(&model.endpoint.auth, crate::auth::Auth::RequestSigner(_));
        let mut diagnostic_redactor = CredentialRedactor::default();
        if !request_aware_signer {
            let resolved_headers = crate::auth::resolve_headers_with_api_key(
                &model.endpoint.auth,
                environment,
                host_options.api_key.as_ref(),
            )
            .await
            .map_err(AiError::Auth)?;
            diagnostic_redactor = resolved_headers.redactor;
            let mut current_key = None;
            for (key, value) in resolved_headers.headers {
                if let Some(key) = key {
                    current_key = Some(key.clone());
                    headers.insert(key, value);
                } else if let Some(key) = &current_key {
                    headers.append(key.clone(), value);
                }
            }
        }
        diagnostic_redactor.include_header_values(&headers);
        if let Some(proxy) = &proxy {
            diagnostic_redactor.include_proxy_url(proxy);
        }

        let fallback_request = HttpStreamRequest {
            model: model.clone(),
            compatibility: req.compatibility,
            parts,
            headers,
            requested_audio_format,
            requested_service_tier: req
                .responses
                .as_ref()
                .and_then(|options| options.service_tier),
            tool_definitions,
            pre_send_diagnostics,
            buffer_ambiguous_compatibility_content,
            diagnostic_redactor: diagnostic_redactor.clone(),
            on_response: host_options.on_response.clone(),
            provider_hooks,
        };

        // Responses WebSockets are deliberately opt-in per endpoint. A
        // connection/handshake failure is replay-safe and falls back to the
        // ordinary HTTP/SSE request below. Once the generation frame may have
        // been sent, every timeout or disconnect is terminal: silently replaying
        // the POST could duplicate provider work and billing.
        let session_key = req.session_id.as_deref().filter(|id| !id.is_empty());
        let transport = crate::declarations::codex::resolve_codex_transport(
            overrides.codex_transport.unwrap_or_default(),
            model.endpoint.transport,
            false, // The pool owns its per-key fallback latch.
            session_key.is_some(),
        );
        if transport.uses_websocket()
            && fallback_request.provider_hooks.is_none()
            && model.spec.protocol == Protocol::OpenAiResponses
            && !request_aware_signer
            && proxy.is_none()
            && fallback_request.parts.streaming
        {
            let session_key = session_key.filter(|_| transport.cached_context);
            let connect_timeout = crate::declarations::codex::effective_codex_connect_timeout_ms(
                overrides.codex_connect_timeout_ms,
            )
            .map_err(|error| crate::ConfigError::Parse(error.to_string()))?
            .map(Duration::from_millis);
            let mut ws_headers = fallback_request.headers.clone();
            if model
                .endpoint
                .runtime
                .responses_profile
                .sends_websocket_beta_header()
            {
                ws_headers.insert(
                    http::HeaderName::from_static("openai-beta"),
                    http::HeaderValue::from_static(ResponsesWsPool::beta_header_value()),
                );
            }
            let websocket_key = session_key.map(|session| {
                responses_websocket_key(model, session, &fallback_request.parts.url, &ws_headers)
            });
            if let Ok(body) =
                serde_json::from_slice::<serde_json::Value>(&fallback_request.parts.body)
            {
                // A retained response can be resumed by cursor after a drop; a
                // non-retained one (octet's durable-replay `store: false`) has
                // nothing to resume, so the actor fails closed instead.
                let resumer = crate::responses_ws::body_requests_storage(&body).then(|| {
                    Arc::new(ResponsesResume {
                        http: self.http.clone(),
                        endpoint: fallback_request.parts.url.clone(),
                        headers: fallback_request.headers.clone(),
                    })
                    .resumer()
                });
                self.mark_request_dispatch();
                let result = self
                    .responses_ws
                    .request(
                        websocket_key.as_deref(),
                        fallback_request.parts.url.clone(),
                        ws_headers,
                        body,
                        ResponsesWsLiveness::for_response_idle(self.stream_idle_timeout),
                        model.endpoint.timeout,
                        connect_timeout,
                        resumer,
                    )
                    .await;
                match result {
                    Ok(events) => {
                        return Ok(responses_websocket_stream(
                            self.responses_ws.clone(),
                            websocket_key.clone(),
                            model.clone(),
                            fallback_request.requested_service_tier,
                            events,
                            fallback_request.pre_send_diagnostics.clone(),
                            fallback_request.tool_definitions.clone(),
                            buffer_ambiguous_compatibility_content,
                            diagnostic_redactor,
                            self.stream_initial_timeout,
                            self.stream_idle_timeout,
                            self.stream_deadline,
                        ));
                    }
                    Err(error) if websocket_open_failure_is_replay_safe(&error) => {}
                    Err(error) => {
                        return Err(sanitize_ai_error(&diagnostic_redactor, error));
                    }
                }
            }
        }

        stream_http(
            self.http.clone(),
            fallback_request,
            self.request_dispatch.clone(),
            self.stream_initial_timeout,
            self.stream_idle_timeout,
            self.stream_deadline,
        )
        .await
    }

    /// Calls OpenAI's native `POST /responses/compact` endpoint.
    ///
    /// The returned output is opaque and intentionally unpruned; callers may
    /// use [`crate::ResponsesCompactResponse::output`] directly as the next
    /// full replay window.
    pub async fn compact_responses(
        &self,
        model: &Model,
        request: ResponsesCompactRequest,
    ) -> Result<ResponsesCompactResponse, AiError> {
        self.open_compact_responses(model, request)
            .await?
            .complete()
            .await
    }

    /// Sends one native compact request and returns at actual HTTP header arrival.
    ///
    /// This includes non-2xx responses: their optional diagnostic body is read
    /// only by [`PendingResponsesCompact::complete`]. Callers can bound opening
    /// independently without cancelling a healthy body at an outage deadline.
    pub async fn open_compact_responses(
        &self,
        model: &Model,
        mut request: ResponsesCompactRequest,
    ) -> Result<PendingResponsesCompact, AiError> {
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if model.spec.endpoint != model.endpoint.id {
            return Err(crate::ConfigError::UnknownEndpoint(model.spec.endpoint.clone()).into());
        }
        if model.spec.protocol != Protocol::OpenAiResponses {
            return Err(crate::error::UnsupportedError::ResponsesOptions.into());
        }
        if request.model != model.spec.api_name {
            return Err(crate::ConfigError::Parse(format!(
                "compact request model {:?} does not match selected model {:?}",
                request.model, model.spec.api_name
            ))
            .into());
        }
        crate::protocol::openai_responses::validate_compact_reasoning(
            model,
            request.reasoning.as_ref(),
        )?;
        // Raw compact DTOs must cross the same replay-update authority boundary
        // as ResponsesCompactRequest::for_model, before credentials or dispatch.
        let baseline = request
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.get("effort"))
            .and_then(serde_json::Value::as_str)
            .and_then(crate::ReasoningConfig::from_provider_value)
            .or_else(|| {
                model
                    .spec
                    .capabilities
                    .reasoning
                    .as_ref()
                    .and_then(|capability| capability.default_selection())
            })
            .unwrap_or(crate::ReasoningConfig::Off);
        crate::responses::validate_responses_input(model, &request.input, &baseline, true)?;
        let rich_codex_schema = model
            .endpoint
            .runtime
            .responses_profile
            .supports_rich_compact_schema()
            || model.spec.cache.session_affinity_format
                == Some(crate::types::SessionAffinityFormat::Codex)
            || model.spec.capabilities.responses_lite;
        if !rich_codex_schema {
            // Public OpenAI compact has a narrower body than the private Codex
            // and Responses Lite contracts. Fail closed at the transport
            // boundary even for callers that constructed the public DTO
            // manually.
            request.tools = None;
            request.parallel_tool_calls = None;
            request.reasoning = None;
            request.text = None;
        }
        let url = model
            .endpoint
            .base_url
            .join("responses/compact")
            .map_err(|error| crate::error::ConfigError::Parse(error.to_string()))?;
        let proxy = self.request_proxy(&url)?;
        let body = serde_json::to_vec(&request)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
        let mut headers = model.endpoint.default_headers.clone();
        merge_preset_headers(&mut headers, &model.spec.preset.headers)?;
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        for (key, value) in crate::protocol::openai_responses::responses_affinity_headers(
            model,
            request.session_id.as_deref(),
        )? {
            if let Some(key) = key {
                headers.insert(key, value);
            }
        }
        // Codex compresses ordinary streaming Responses requests, but its
        // compact endpoint contract is plain JSON. Do not apply the normal
        // Responses transport compression policy here.
        let mut body = bytes::Bytes::from(body);
        let provider_hooks = ProviderRequestAttempt::new(model, &self.provider_request_hooks, &[]);
        if let Some(hooks) = &provider_hooks {
            body = hooks.payload(body).await?;
            hooks.headers(&mut headers).await?;
        }
        let resolved_headers = crate::auth::resolve_headers(&model.endpoint.auth)
            .await
            .map_err(AiError::Auth)?;
        let mut diagnostic_redactor = resolved_headers.redactor;
        diagnostic_redactor.include_header_values(&headers);
        let mut current_key = None;
        for (key, value) in resolved_headers.headers {
            if let Some(key) = key {
                current_key = Some(key.clone());
                headers.insert(key, value);
            } else if let Some(key) = &current_key {
                headers.append(key.clone(), value);
            }
        }
        self.mark_request_dispatch();
        if let Some(proxy) = &proxy {
            diagnostic_redactor.include_proxy_url(proxy);
        }
        let response = tokio::time::timeout(
            model.endpoint.timeout,
            self.http.post(url).headers(headers).body(body).send(),
        )
        .await
        .map_err(|_| {
            AiError::Transport(TransportError {
                phase: TransportPhase::ResponseHeaders,
                timeout: true,
                message: "compact request timed out waiting for response headers".to_owned(),
            })
        })?
        .map_err(|error| request_open_transport_error(error, "compact request"))
        .map_err(|error| sanitize_ai_error(&diagnostic_redactor, error))?;
        let opened_at = Instant::now();
        if let Some(hooks) = &provider_hooks {
            hooks
                .response(response.status(), response.headers())
                .await?;
        }
        Ok(PendingResponsesCompact {
            response,
            diagnostic_redactor,
            stream_idle_timeout: self.stream_idle_timeout,
            stream_initial_timeout: self.stream_initial_timeout,
            stream_deadline: self.stream_deadline,
            opened_at,
        })
    }

    /// Submits one asynchronous OpenRouter Batch API job.
    ///
    /// The batch is deliberately not part of [`Self::stream`]: OpenRouter
    /// completes it asynchronously and returns results only after processing.
    /// `endpoint` must be the catalog's `openrouter` endpoint, and all request
    /// bodies must already use the selected batch endpoint's native JSON shape.
    pub async fn submit_openrouter_batch(
        &self,
        endpoint: &crate::types::Endpoint,
        request: crate::batch::OpenRouterBatchRequest,
    ) -> Result<crate::batch::OpenRouterBatch, AiError> {
        request.validate()?;
        let body = serde_json::to_vec(&request)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
        if body.len() > MAX_BATCH_BODY_BYTES {
            return Err(DecodeError::BodyTooLarge.into());
        }
        let value = batch_http_request(
            self,
            endpoint,
            http::Method::POST,
            openrouter_batch_url(endpoint)?,
            Some(bytes::Bytes::from(body)),
            "batch submission",
        )
        .await?;
        serde_json::from_value(value).map_err(|error| {
            AiError::Decode(DecodeError::Json(format!(
                "invalid OpenRouter batch response: {error}"
            )))
        })
    }

    /// Retrieves one OpenRouter Batch API job and its inline results, if ready.
    pub async fn get_openrouter_batch(
        &self,
        endpoint: &crate::types::Endpoint,
        id: &str,
    ) -> Result<crate::batch::OpenRouterBatch, AiError> {
        let value = batch_http_request(
            self,
            endpoint,
            http::Method::GET,
            openrouter_batch_item_url(endpoint, id)?,
            None,
            "batch retrieval",
        )
        .await?;
        serde_json::from_value(value).map_err(|error| {
            AiError::Decode(DecodeError::Json(format!(
                "invalid OpenRouter batch response: {error}"
            )))
        })
    }

    /// Lists OpenRouter Batch API jobs using cursor and status filters.
    pub async fn list_openrouter_batches(
        &self,
        endpoint: &crate::types::Endpoint,
        options: &crate::batch::OpenRouterBatchListOptions,
    ) -> Result<crate::batch::OpenRouterBatchList, AiError> {
        options.validate()?;
        let mut url = openrouter_batch_url(endpoint)?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(limit) = options.limit {
                query.append_pair("limit", &limit.to_string());
            }
            if let Some(after) = &options.after {
                query.append_pair("after", after);
            }
            for status in &options.statuses {
                query.append_pair("status", status);
            }
            if let Some(created_after) = &options.created_after {
                query.append_pair("created_after", created_after);
            }
            if let Some(created_before) = &options.created_before {
                query.append_pair("created_before", created_before);
            }
        }
        let value = batch_http_request(
            self,
            endpoint,
            http::Method::GET,
            url,
            None,
            "batch listing",
        )
        .await?;
        serde_json::from_value(value).map_err(|error| {
            AiError::Decode(DecodeError::Json(format!(
                "invalid OpenRouter batch list response: {error}"
            )))
        })
    }

    /// Executes a request and drives the stream to completion, returning the final Response.
    pub async fn complete(&self, model: &Model, req: Request) -> Result<Response, AiError> {
        self.complete_with_overrides(model, req, crate::RequestOverrides::default())
            .await
    }

    /// Executes and collects one request using the same private overrides and
    /// no-retry contract as [`Self::stream_with_overrides`].
    pub async fn complete_with_overrides(
        &self,
        model: &Model,
        req: Request,
        overrides: crate::RequestOverrides,
    ) -> Result<Response, AiError> {
        self.complete_with_host_options(model, req, overrides, HostRequestOptions::default())
            .await
    }

    /// Executes and collects one request with host-owned runtime options.
    pub async fn complete_with_host_options(
        &self,
        model: &Model,
        req: Request,
        overrides: crate::RequestOverrides,
        host_options: HostRequestOptions,
    ) -> Result<Response, AiError> {
        let mut stream = self
            .stream_with_host_options(model, req, overrides, host_options)
            .await?;
        let mut final_response = None;

        while let Some(ev_res) = stream.next().await {
            let ev = ev_res?;
            if let StreamEvent::Finished(resp) = ev {
                final_response = Some(resp);
            }
        }

        final_response.ok_or_else(|| AiError::StreamProtocol(StreamProtocolError::MissingFinish))
    }

    /// The reqwest transport owned by this client, for crate-internal auxiliary
    /// APIs (the image-generation adapter) that share the same proxy snapshot.
    pub(crate) fn http_transport(&self) -> &reqwest::Client {
        &self.http
    }

    /// Resolves the proxy for `target` under this client's snapshot.
    pub(crate) fn proxy_for(&self, target: &url::Url) -> Result<Option<url::Url>, AiError> {
        self.request_proxy(target)
    }

    /// Clones this client with a request-local proxy overlay when `env` selects
    /// one of the proxy variables. The overlay never mutates process state or
    /// the original client; malformed values fail closed.
    pub(crate) fn with_environment_overlay(
        &self,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<AiClient, AiError> {
        if !crate::declarations::proxy::ProxyEnvironment::NAMES
            .iter()
            .any(|name| env.contains_key(*name))
        {
            return Ok(self.clone());
        }
        let current = self.proxy_environment.as_ref().ok_or_else(|| {
            crate::ConfigError::Parse("proxy overrides require the built-in HTTP transport".into())
        })?;
        let proxy = Arc::new(current.overlay(env));
        let mut client = self.clone();
        client.http = proxy
            .clone()
            .configure(
                reqwest::Client::builder()
                    .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::none()),
            )
            .build()
            .map_err(|_| {
                crate::ConfigError::Parse("could not configure request-local HTTP transport".into())
            })?;
        client.proxy_environment = Some(proxy);
        Ok(client)
    }

    fn deferred_endpoint_transport(
        &self,
        model: &Model,
    ) -> Result<Arc<dyn HostStreamTransport>, AiError> {
        self.host_stream_transports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&model.endpoint.id)
            .cloned()
            .ok_or_else(|| crate::error::UnsupportedError::Deferred.into())
    }

    fn validate_deferred_overrides(overrides: &crate::RequestOverrides) -> Result<(), AiError> {
        overrides
            .validate()
            .map_err(|error| crate::ConfigError::Parse(error.to_string()))?;
        if !overrides.headers.is_empty()
            || !overrides.sampling_params.is_empty()
            || overrides.azure.is_some()
            || overrides.max_retries.unwrap_or(0) != 0
            || overrides.max_retry_delay_ms.unwrap_or(0) != 0
        {
            return Err(crate::ConfigError::Parse(
                "deferred requests accept only request-local environment and timeout overrides"
                    .into(),
            )
            .into());
        }
        Ok(())
    }

    /// Submits one request that the provider may park instead of completing.
    ///
    /// A parked turn finishes with [`crate::StopReason::Deferred`] and a
    /// [`DeferredHandle`] on the response; it is never silently retried. Only a
    /// transport that implements
    /// [`HostStreamTransport::submit_deferred`] can park a request.
    /// `poll_after_ms` is the caller's request-local minimum delay before the
    /// next poll.
    pub async fn submit_deferred(
        &self,
        model: &Model,
        req: Request,
        overrides: crate::RequestOverrides,
        poll_after_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        let started = Instant::now();
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        Self::validate_deferred_overrides(&overrides)?;
        let transport = self.deferred_endpoint_transport(model)?;
        let (request, diagnostics) = prepare_host_request(model, req)?;
        self.mark_request_dispatch();
        let stream = transport
            .submit_deferred(
                HostStreamModel::from(model),
                request,
                diagnostics,
                poll_after_ms,
            )
            .await?;
        Ok(crate::inference::measured_stream(
            crate::stream::guard(stream),
            started,
            crate::inference::ClientTimingScope::DeferredSubmit,
        ))
    }

    /// Polls one deferred handle under a one-shot, generation-bound permit.
    ///
    /// The permit is consumed before any provider work: a missing, already
    /// consumed, or stale permit fails closed, so one driving pass can never
    /// admit two billable polls. The caller owns the durable leaf generation it
    /// minted the permit for. `wait_ms` bounds the provider long-poll; `Some(0)`
    /// performs one status check.
    pub async fn fetch_deferred(
        &self,
        model: &Model,
        handle: DeferredHandle,
        mut permit: crate::deferred::DeferredPollPermit,
        leaf_generation: u64,
        wait_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        let started = Instant::now();
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        permit.consume(leaf_generation)?;
        if handle.id.is_empty() {
            return Err(crate::ConfigError::Parse(
                "deferred handle has an empty provider id".into(),
            )
            .into());
        }
        if handle.model_id != model.spec.id.0 {
            return Err(crate::ConfigError::Parse(
                "deferred handle belongs to a different model".into(),
            )
            .into());
        }
        let transport = self.deferred_endpoint_transport(model)?;
        self.mark_request_dispatch();
        let stream = transport
            .fetch_deferred(HostStreamModel::from(model), handle, wait_ms)
            .await?;
        Ok(crate::inference::measured_stream(
            crate::stream::guard(stream),
            started,
            crate::inference::ClientTimingScope::DeferredPoll,
        ))
    }

    /// Best-effort cancellation of one deferred handle.
    ///
    /// Cancellation does not un-send provider work or erase usage uncertainty;
    /// it only asks the owning transport to release the parked response.
    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: DeferredHandle,
    ) -> Result<(), AiError> {
        crate::catalog::validate_endpoint(&model.endpoint)?;
        crate::catalog::validate_model_spec(&model.spec)?;
        if handle.model_id != model.spec.id.0 {
            return Err(crate::ConfigError::Parse(
                "deferred handle belongs to a different model".into(),
            )
            .into());
        }
        let transport = self.deferred_endpoint_transport(model)?;
        self.mark_request_dispatch();
        transport
            .cancel_deferred(HostStreamModel::from(model), handle)
            .await
    }
}
#[cfg(test)]
mod tests;
