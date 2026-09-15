//! Cached OpenAI Responses WebSocket transport.
//!
//! The durable session remains the recovery source of truth. This module only
//! keeps a best-effort live cursor for normal turns: when the current request
//! is a strict extension of the last request plus its terminal output, the
//! wire payload carries `previous_response_id` and the new input suffix. Any
//! mismatch sends the full local replay instead.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};
use url::Url;

use crate::error::{AiError, ConfigError, TransportError, TransportPhase};

const RESPONSES_WEBSOCKETS_BETA: &str = "responses_websockets=2026-02-06";
const EVENT_CHANNEL_CAPACITY: usize = 64;
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONNECTION_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// Normal liveness probes catch a lost route well before the five-minute
/// response-progress deadline without burdening an otherwise idle socket.
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_HEARTBEAT_ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounded reconnect attempts for one generation whose socket dropped before
/// any consumer-visible output was forwarded.
///
/// A dropped socket must not end a long-running turn: the Codex path holds a
/// 272K-context request whose first output token can be minutes away, so a
/// heartbeat or read failure during that wait is far more likely than a
/// failure after output. Retrying is only safe while nothing visible reached
/// the consumer (see [`AttemptOutcome::Interrupted`]).
const MAX_SOCKET_RECONNECT_ATTEMPTS: u32 = 3;
/// First reconnect delay; doubled per attempt and capped by
/// [`RECONNECT_MAX_DELAY`].
const RECONNECT_INITIAL_DELAY: Duration = Duration::from_millis(250);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(2);
/// Hard ceiling on total reconnect waiting for one generation. The attempt
/// budget and this deadline bound both the number of provider requests and the
/// time a caller can be held by an unreachable route.
const RECONNECT_TOTAL_BUDGET: Duration = Duration::from_secs(6);
/// Event types that carry no model output. They are buffered so a reconnect
/// before the first output event can discard the abandoned attempt's prefix
/// instead of publishing a duplicated prelude.
const PRE_OUTPUT_BUFFER_EVENTS: usize = 16;
const PRE_OUTPUT_BUFFER_BYTES: usize = 64 * 1024;
/// Upper bound on one interruption detail carried into a terminal error.
const MAX_INTERRUPTION_DETAIL_BYTES: usize = 512;
/// Inactive heartbeat-ack deadline. The timer is only armed once a probe is
/// outstanding (its reset uses the configured acknowledgement timeout), so the
/// initial value must be effectively never.
const HEARTBEAT_ACK_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);

/// Bounded exponential backoff for one reconnect attempt (1-based).
fn reconnect_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(8);
    RECONNECT_INITIAL_DELAY
        .saturating_mul(1_u32 << shift)
        .min(RECONNECT_MAX_DELAY)
}

/// Whether an event is a pre-output lifecycle event that can be buffered and
/// discarded by a retry. Anything else is consumer-visible content (or a
/// terminal), after which a dropped socket can no longer be retried safely.
fn is_pre_output_event(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.created" | "response.queued" | "response.in_progress")
    )
}

/// Typed terminal for a stream that cannot be resumed.
fn not_resumable(attempts: u32, visible_output: bool, detail: &str) -> AiError {
    AiError::StreamProtocol(crate::error::StreamProtocolError::ResponseNotResumable {
        attempts,
        visible_output,
        detail: detail.to_owned(),
    })
}

/// Dials one socket. Production re-runs the same handshake as the initial
/// connection; tests substitute a dialer that counts and steers attempts.
type DialFuture<S> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<S, AiError>> + Send>>;
pub(crate) type SocketDialer<S> = Arc<dyn Fn(Url, http::HeaderMap) -> DialFuture<S> + Send + Sync>;

/// Production socket type for a Responses WebSocket endpoint.
pub(crate) type ResponsesSocket = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// One in-flight generation's consumer-visible cursor: the provider response id
/// and the highest event `sequence_number` already forwarded downstream.
///
/// A drop after output is only recoverable when the provider still retains the
/// generation, so the cursor is exactly what the Responses retrieve endpoint
/// needs (`starting_after`) to hand back the events the consumer has not seen.
#[derive(Clone, Default)]
pub(crate) struct GenerationProgress {
    response_id: Option<String>,
    last_sequence: Option<u64>,
}

impl GenerationProgress {
    /// Advances the cursor with one forwarded event. The first response id wins
    /// (a later event cannot re-identify the generation) and the sequence is a
    /// running maximum.
    fn observe(&mut self, value: &Value) {
        if self.response_id.is_none() {
            self.response_id = value
                .get("response")
                .and_then(|response| response.get("id"))
                .and_then(Value::as_str)
                .or_else(|| value.get("response_id").and_then(Value::as_str))
                .map(str::to_owned);
        }
        if let Some(sequence) = value.get("sequence_number").and_then(Value::as_u64) {
            self.last_sequence = Some(match self.last_sequence {
                Some(current) => current.max(sequence),
                None => sequence,
            });
        }
    }

    /// Carries a later attempt's cursor forward without ever moving backwards.
    fn absorb(&mut self, later: &GenerationProgress) {
        if self.response_id.is_none() {
            self.response_id = later.response_id.clone();
        }
        if let Some(sequence) = later.last_sequence {
            self.last_sequence = Some(match self.last_sequence {
                Some(current) => current.max(sequence),
                None => sequence,
            });
        }
    }
}

/// Future that opens a resumed read of one retained generation.
pub(crate) type ResumeFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<mpsc::Receiver<Result<Value, AiError>>, AiError>>
            + Send,
    >,
>;

/// Opens the remaining events of an in-flight generation from a cursor.
///
/// The production implementation reads the Responses retrieve endpoint
/// (`GET /responses/{id}?stream=true&starting_after=N`) through the client's
/// HTTP transport. Tests substitute a scripted resumer so a mid-stream drop can
/// be driven deterministically.
pub(crate) type ResponseResumer = Arc<dyn Fn(String, u64) -> ResumeFuture + Send + Sync>;

/// Whether the request asked the provider to retain the generated response.
///
/// Resumption is only possible when the provider still holds the generation.
/// octet's durable-replay callers send `store: false`, in which case no
/// server-side response exists and a resume attempt is skipped in favour of the
/// typed non-resumable failure.
pub(crate) fn body_requests_storage(body: &Value) -> bool {
    body.get("store") == Some(&Value::Bool(true))
}

fn production_dialer() -> SocketDialer<ResponsesSocket> {
    Arc::new(|url: Url, headers: http::HeaderMap| {
        Box::pin(async move {
            let url = websocket_url(url)?;
            let request = connect_request(url, &headers)?;
            match connect_async(request).await {
                Ok((socket, _)) => Ok(socket),
                Err(error) => Err(websocket_connect_error(error)),
            }
        })
    })
}

/// Transport-liveness timing for one active Responses generation.
///
/// This deliberately scales down with a caller's response-idle bound so a
/// shorter configured response timeout is not unexpectedly slower to detect a
/// half-open socket. Pong traffic never reaches the response event channel and
/// therefore cannot extend model-generation progress deadlines.
#[derive(Clone, Copy)]
pub(crate) struct ResponsesWsLiveness {
    interval: Duration,
    acknowledgement_timeout: Duration,
}

impl ResponsesWsLiveness {
    pub(crate) fn for_response_idle(response_idle_timeout: Duration) -> Self {
        let fraction = (response_idle_timeout / 4).max(Duration::from_millis(1));
        Self {
            interval: DEFAULT_HEARTBEAT_INTERVAL.min(fraction),
            acknowledgement_timeout: DEFAULT_HEARTBEAT_ACK_TIMEOUT.min(fraction),
        }
    }
}

/// A post-send liveness failure is deliberately not replayable. The request may
/// already be executing remotely, so the agent's timeout policy must not
/// transparently replay it. The transport now recovers such a failure by
/// reconnecting while nothing consumer-visible was published, and otherwise
/// reports [`crate::error::StreamProtocolError::ResponseNotResumable`] instead
/// of a replay-safe transport timeout.
fn transport_error(phase: TransportPhase, message: impl Into<String>) -> AiError {
    AiError::Transport(TransportError {
        phase,
        timeout: false,
        message: message.into(),
    })
}

fn websocket_connect_error(error: tungstenite::Error) -> AiError {
    let transient = matches!(&error, tungstenite::Error::Io(io)
        if crate::error::transient_connection_io(io));
    let timeout = matches!(&error, tungstenite::Error::Io(io)
        if io.kind() == std::io::ErrorKind::TimedOut);
    let transport = TransportError {
        phase: TransportPhase::Connect,
        timeout,
        message: format!("Responses WebSocket connect: {error}"),
    };
    if transient {
        AiError::NetworkUnavailable(transport)
    } else {
        AiError::Transport(transport)
    }
}

fn websocket_url(mut url: Url) -> Result<Url, AiError> {
    match url.scheme() {
        "http" => url
            .set_scheme("ws")
            .map_err(|_| ConfigError::Parse("could not convert Responses URL to ws".into()))?,
        "https" => url
            .set_scheme("wss")
            .map_err(|_| ConfigError::Parse("could not convert Responses URL to wss".into()))?,
        "ws" | "wss" => {}
        scheme => {
            return Err(ConfigError::Parse(format!(
                "unsupported Responses WebSocket URL scheme {scheme:?}"
            ))
            .into());
        }
    }
    Ok(url)
}

fn connect_request(
    url: Url,
    headers: &http::HeaderMap,
) -> Result<tungstenite::http::Request<()>, AiError> {
    let mut request = url.as_str().into_client_request().map_err(|error| {
        transport_error(
            TransportPhase::Connect,
            format!("websocket request: {error}"),
        )
    })?;
    for (name, value) in headers {
        request.headers_mut().insert(name.clone(), value.clone());
    }
    Ok(request)
}

fn without_continuation_fields(body: &Value) -> Option<Value> {
    let Value::Object(object) = body else {
        return None;
    };
    let mut fixed = object.clone();
    fixed.remove("input");
    fixed.remove("previous_response_id");
    fixed.remove("generate");
    Some(Value::Object(fixed))
}

fn is_prefix<T: PartialEq>(prefix: &[T], values: &[T]) -> bool {
    values.len() >= prefix.len() && values[..prefix.len()] == *prefix
}

#[derive(Clone)]
struct Continuation {
    fixed_body: Value,
    request_input: Vec<Value>,
    response_output: Vec<Value>,
    response_id: String,
}

fn incremental_body(body: &Value, continuation: Option<&Continuation>) -> (Value, bool) {
    let Some(continuation) = continuation else {
        return (body.clone(), false);
    };
    let Some(Value::Array(input)) = body.get("input") else {
        return (body.clone(), false);
    };
    if body.get("previous_response_id").is_some()
        || without_continuation_fields(body) != Some(continuation.fixed_body.clone())
    {
        return (body.clone(), false);
    }

    let baseline_len = continuation
        .request_input
        .len()
        .saturating_add(continuation.response_output.len());
    if input.len() < baseline_len
        || !is_prefix(&continuation.request_input, input)
        || !is_prefix(
            &continuation.response_output,
            &input[continuation.request_input.len()..],
        )
    {
        return (body.clone(), false);
    }

    let mut incremental = body.clone();
    let Some(object) = incremental.as_object_mut() else {
        return (body.clone(), false);
    };
    object.insert(
        "previous_response_id".to_owned(),
        Value::String(continuation.response_id.clone()),
    );
    object.insert(
        "input".to_owned(),
        Value::Array(input[baseline_len..].to_vec()),
    );
    (incremental, true)
}

fn terminal_kind(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str).filter(|kind| {
        matches!(
            *kind,
            "response.completed" | "response.incomplete" | "response.failed" | "response.cancelled"
        )
    })
}

fn update_continuation(full_body: &Value, value: &Value, continuation: &mut Option<Continuation>) {
    let Some(kind) = terminal_kind(value) else {
        return;
    };
    if kind == "response.failed" || kind == "response.cancelled" {
        *continuation = None;
        return;
    }
    let Some(response) = value.get("response") else {
        *continuation = None;
        return;
    };
    let Some(response_id) = response.get("id").and_then(Value::as_str) else {
        *continuation = None;
        return;
    };
    let Some(fixed_body) = without_continuation_fields(full_body) else {
        *continuation = None;
        return;
    };
    let request_input = full_body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let response_output = response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    *continuation = Some(Continuation {
        fixed_body,
        request_input,
        response_output,
        response_id: response_id.to_owned(),
    });
}

struct RequestCommand {
    body: Value,
    /// Endpoint URL and headers retained so a dropped socket can be dialed
    /// again with the same handshake the pool used.
    url: Url,
    headers: http::HeaderMap,
    reply: mpsc::Sender<Result<Value, AiError>>,
    /// Taken by the actor before the generation loop so the command itself
    /// stays borrowable across reconnect attempts.
    started: Option<oneshot::Sender<Result<(), String>>>,
    liveness: ResponsesWsLiveness,
    /// Opens a resumed read of a retained in-flight generation. Present only
    /// when the request asked the provider to store the response (see
    /// [`body_requests_storage`]); otherwise a post-output drop has nothing to
    /// resume and fails closed with the typed non-resumable error.
    resumer: Option<ResponseResumer>,
}

#[derive(Clone)]
struct Connection {
    sender: mpsc::Sender<RequestCommand>,
    alive: Arc<AtomicBool>,
}

#[derive(Default)]
struct PoolState {
    sessions: HashMap<String, Connection>,
    disabled: HashSet<String>,
}

/// A process-local pool of session-affine Responses WebSockets.
#[derive(Clone, Default)]
pub(crate) struct ResponsesWsPool {
    state: Arc<Mutex<PoolState>>,
}

impl ResponsesWsPool {
    /// Protocol/consumer failures must fence the pool before reaching the caller.
    pub(crate) async fn disable(&self, key: Option<&str>) {
        if let Some(key) = key {
            let mut state = self.state.lock().await;
            state.disabled.insert(key.to_owned());
            if let Some(connection) = state.sessions.remove(key) {
                connection.alive.store(false, Ordering::Release);
            }
        }
    }

    async fn connect(
        &self,
        key: Option<&str>,
        url: Url,
        headers: http::HeaderMap,
    ) -> Result<Connection, AiError> {
        if let Some(key) = key {
            let mut state = self.state.lock().await;
            if state.disabled.contains(key) {
                return Err(transport_error(
                    TransportPhase::Connect,
                    "Responses WebSocket disabled after an earlier failure",
                ));
            }
            if let Some(connection) = state.sessions.get(key) {
                if connection.alive.load(Ordering::Acquire) {
                    return Ok(connection.clone());
                }
            }
            state.sessions.remove(key);
        }

        // The handshake is deliberately outside the pool lock. Concurrent
        // opens are reconciled below so one slow endpoint cannot block every
        // cached session.
        let url = websocket_url(url)?;
        let request = connect_request(url, &headers)?;
        let (socket, _) = match connect_async(request).await {
            Ok(connected) => connected,
            Err(error) => {
                let error = websocket_connect_error(error);
                if let Some(key) = key {
                    let mut state = self.state.lock().await;
                    if let Some(connection) = state.sessions.get(key) {
                        if connection.alive.load(Ordering::Acquire) {
                            return Ok(connection.clone());
                        }
                    }
                    state.sessions.remove(key);
                    state.disabled.insert(key.to_owned());
                }
                return Err(error);
            }
        };

        let (sender, receiver) = mpsc::channel(4);
        let alive = Arc::new(AtomicBool::new(true));
        let connection = Connection {
            sender,
            alive: Arc::clone(&alive),
        };

        if let Some(key) = key {
            enum Registration {
                Installed,
                Existing(Connection),
                Disabled,
            }

            let registration = {
                let mut state = self.state.lock().await;
                if state.disabled.contains(key) {
                    Registration::Disabled
                } else if let Some(existing) = state
                    .sessions
                    .get(key)
                    .filter(|existing| existing.alive.load(Ordering::Acquire))
                {
                    Registration::Existing(existing.clone())
                } else {
                    state.sessions.remove(key);
                    state.sessions.insert(key.to_owned(), connection.clone());
                    Registration::Installed
                }
            };

            match registration {
                Registration::Installed => {
                    tokio::spawn(run_connection(
                        socket,
                        receiver,
                        alive,
                        Some(key.to_owned()),
                        Arc::downgrade(&self.state),
                        CONNECTION_IDLE_TIMEOUT,
                        production_dialer(),
                    ));
                    Ok(connection)
                }
                Registration::Existing(existing) => {
                    // Both handshakes raced for the same key. The map's current
                    // live connection wins; close the unobserved socket rather
                    // than leaving a detached actor behind.
                    connection.alive.store(false, Ordering::Release);
                    drop(receiver);
                    retire_socket(socket);
                    Ok(existing)
                }
                Registration::Disabled => {
                    connection.alive.store(false, Ordering::Release);
                    drop(receiver);
                    retire_socket(socket);
                    Err(transport_error(
                        TransportPhase::Connect,
                        "Responses WebSocket disabled after an earlier failure",
                    ))
                }
            }
        } else {
            tokio::spawn(run_connection(
                socket,
                receiver,
                alive,
                None,
                Arc::downgrade(&self.state),
                CONNECTION_IDLE_TIMEOUT,
                production_dialer(),
            ));
            Ok(connection)
        }
    }

    async fn remove(&self, key: &str, connection: &Connection) {
        let mut state = self.state.lock().await;
        if state
            .sessions
            .get(key)
            .is_some_and(|current| current.sender.same_channel(&connection.sender))
        {
            state.sessions.remove(key);
        }
    }

    /// Sends one full request to a cached or one-shot connection and returns
    /// the raw JSON event stream. The caller owns protocol decoding and stream
    /// deadlines.
    pub(crate) async fn request(
        &self,
        key: Option<&str>,
        url: Url,
        headers: http::HeaderMap,
        body: Value,
        liveness: ResponsesWsLiveness,
        startup_timeout: Duration,
        resumer: Option<ResponseResumer>,
    ) -> Result<mpsc::Receiver<Result<Value, AiError>>, AiError> {
        let deadline = tokio::time::Instant::now() + startup_timeout;
        // Only this future owns connection establishment; no generation command
        // exists yet. A timeout here is proven safe for HTTP fallback.
        let connection = tokio::time::timeout(
            startup_timeout.min(crate::client::DEFAULT_CONNECT_TIMEOUT),
            self.connect(key, url.clone(), headers.clone()),
        )
        .await
        .map_err(|_| {
            AiError::NetworkUnavailable(TransportError {
                phase: TransportPhase::Connect,
                timeout: true,
                message: "Responses WebSocket handshake timed out before request send".to_owned(),
            })
        })??;
        // Keep queueing and sending inside the original endpoint startup budget,
        // without letting that outer timer misclassify a handshake timeout.
        tokio::time::timeout_at(
            deadline,
            self.send_request(key, connection, url, headers, body, liveness, resumer),
        )
        .await
        .map_err(|_| {
            AiError::Transport(TransportError {
                phase: TransportPhase::ResponseHeaders,
                timeout: true,
                message: "Responses WebSocket request start timed out; acceptance unknown"
                    .to_owned(),
            })
        })?
    }

    async fn send_request(
        &self,
        key: Option<&str>,
        connection: Connection,
        url: Url,
        headers: http::HeaderMap,
        body: Value,
        liveness: ResponsesWsLiveness,
        resumer: Option<ResponseResumer>,
    ) -> Result<mpsc::Receiver<Result<Value, AiError>>, AiError> {
        let (reply, events) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (started, started_result) = oneshot::channel();
        let command = RequestCommand {
            body,
            url,
            headers,
            reply,
            started: Some(started),
            liveness,
            resumer,
        };
        if connection.sender.send(command).await.is_err() {
            connection.alive.store(false, Ordering::Release);
            if let Some(key) = key {
                self.remove(key, &connection).await;
            }
            return Err(transport_error(
                TransportPhase::Connect,
                "Responses WebSocket connection closed before request send",
            ));
        }
        match started_result.await {
            Ok(Ok(())) => Ok(events),
            Ok(Err(message)) => {
                connection.alive.store(false, Ordering::Release);
                if let Some(key) = key {
                    self.remove(key, &connection).await;
                }
                Err(transport_error(TransportPhase::ResponseHeaders, message))
            }
            Err(_) => {
                connection.alive.store(false, Ordering::Release);
                if let Some(key) = key {
                    self.remove(key, &connection).await;
                }
                // The actor owns `started` and only acknowledges it immediately
                // after `socket.send` succeeds. If the sender disappears first,
                // the queued command was dropped without reaching the socket, so
                // replaying through HTTP is safe.
                Err(transport_error(
                    TransportPhase::Connect,
                    "Responses WebSocket actor stopped before request start was acknowledged",
                ))
            }
        }
    }

    /// Performs a best-effort `generate=false` request used to establish a
    /// provider-side continuation while a caller is still preparing a turn.
    pub(crate) async fn prewarm(
        &self,
        key: &str,
        url: Url,
        headers: http::HeaderMap,
        mut body: Value,
        liveness: ResponsesWsLiveness,
        startup_timeout: Duration,
    ) -> Result<(), AiError> {
        let Some(object) = body.as_object_mut() else {
            return Err(crate::error::DecodeError::Json(
                "Responses request body is not an object".to_owned(),
            )
            .into());
        };
        object.insert("generate".to_owned(), Value::Bool(false));
        let deadline = tokio::time::Instant::now() + startup_timeout;
        let mut events = self
            .request(Some(key), url, headers, body, liveness, startup_timeout, None)
            .await?;
        tokio::time::timeout_at(deadline, async {
            while let Some(event) = events.recv().await {
                let event = event?;
                if terminal_kind(&event).is_some() {
                    return Ok(());
                }
            }
            Err(transport_error(
                TransportPhase::Body,
                "Responses WebSocket prewarm ended before completion",
            ))
        })
        .await
        .map_err(|_| {
            AiError::Transport(TransportError {
                phase: TransportPhase::Body,
                timeout: true,
                message: "Responses WebSocket prewarm response timed out".to_owned(),
            })
        })?
    }

    /// Header value required by the current Codex Responses WebSocket route.
    pub(crate) fn beta_header_value() -> &'static str {
        RESPONSES_WEBSOCKETS_BETA
    }
}

// Failed terminals and unknown incomplete outcomes poison continuation even
// when their code is unfamiliar. Retire before forwarding to the decoder so a
// host-authorized replacement cannot race onto the same socket.
fn failed_terminal(value: &Value) -> bool {
    match value.get("type").and_then(Value::as_str) {
        Some("response.failed" | "response.cancelled") => true,
        Some("response.incomplete") => !matches!(
            value
                .pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str),
            Some("max_output_tokens" | "content_filter")
        ),
        _ => false,
    }
}

/// Provider-supplied error code from an event frame, wherever the route puts
/// it: a nested `response.error`, a nested `error`, or the event itself.
fn issue_code(value: &Value) -> Option<String> {
    let error = value
        .get("response")
        .and_then(|response| response.get("error"))
        .or_else(|| value.get("error"));
    error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .or_else(|| value.get("code").and_then(Value::as_str))
        .map(str::to_ascii_lowercase)
}

fn connection_refresh_error(value: &Value) -> bool {
    let Some(code) = issue_code(value) else {
        return false;
    };
    code == "websocket_connection_limit_reached"
        || (code.contains("websocket") && code.contains("connection") && code.contains("limit"))
}

/// The provider no longer knows the `previous_response_id` cursor we sent, e.g.
/// because the connection that owned it went away. Upstream retries once with
/// the full local body (`openai-codex-responses.ts:337-339`).
fn stale_continuation_error(value: &Value) -> bool {
    issue_code(value).is_some_and(|code| code == "previous_response_not_found")
}

async fn disable_key(state: &Weak<Mutex<PoolState>>, key: Option<&str>) {
    let (Some(state), Some(key)) = (state.upgrade(), key) else {
        return;
    };
    state.lock().await.disabled.insert(key.to_owned());
}

async fn remove_connection(
    state: &Weak<Mutex<PoolState>>,
    key: Option<&str>,
    alive: &Arc<AtomicBool>,
) {
    let (Some(state), Some(key)) = (state.upgrade(), key) else {
        return;
    };
    let mut state = state.lock().await;
    if state
        .sessions
        .get(key)
        .is_some_and(|connection| Arc::ptr_eq(&connection.alive, alive))
    {
        state.sessions.remove(key);
    }
}

async fn close_socket<S>(socket: &mut S)
where
    S: futures_util::Sink<Message, Error = tungstenite::Error> + Unpin,
{
    let _ = tokio::time::timeout(CONNECTION_CLOSE_TIMEOUT, socket.close()).await;
}

fn retire_socket<S>(mut socket: S)
where
    S: futures_util::Sink<Message, Error = tungstenite::Error> + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        close_socket(&mut socket).await;
    });
}

/// How one generation command ended. Reconnection is decided inside
/// [`run_generation`]; this is what the actor loop must do next.
enum GenerationEnd {
    /// A provider terminal event completed the generation; keep the socket.
    Completed,
    /// The consumer dropped the request; retire and fence the pool key.
    Abandoned,
    /// A provider failure was already forwarded; retire and fence the key so a
    /// later request cannot race this socket.
    Forwarded,
    /// Retire before publishing the terminal error.
    Fatal { error: AiError },
}

/// One generation attempt's outcome over one socket.
enum AttemptOutcome {
    /// A provider terminal event was forwarded.
    Completed,
    /// A provider failure event was already forwarded to the consumer.
    Forwarded,
    /// The socket or heartbeat died, or the provider rejected the connection.
    /// `visible` records whether output was already forwarded, and `progress`
    /// carries the consumer's response cursor. Together they decide between a
    /// bounded reconnect (before output), a bounded cursor resume (after output,
    /// when the provider retains the generation), and a typed non-resumable
    /// failure.
    Interrupted {
        detail: String,
        visible: bool,
        progress: GenerationProgress,
    },
    /// A protocol/decode failure: the stream is poisoned.
    Fatal { error: AiError },
    /// The consumer dropped the request.
    Abandoned,
}

fn interrupted(
    detail: impl Into<String>,
    visible: bool,
    progress: &GenerationProgress,
) -> AttemptOutcome {
    let mut detail = detail.into();
    // Bounded, credential-free diagnostic: the provider text is already
    // truncated at the request boundary, and this keeps one failure from
    // carrying an unbounded payload into the terminal error.
    if detail.len() > MAX_INTERRUPTION_DETAIL_BYTES {
        let mut end = MAX_INTERRUPTION_DETAIL_BYTES;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
    }
    AttemptOutcome::Interrupted {
        detail,
        visible,
        progress: progress.clone(),
    }
}

/// Publishes one provider event.
///
/// Pre-output lifecycle events are buffered until the first output-bearing
/// event arrives, so an abandoned attempt's prefix is never duplicated by a
/// retry. Returns `false` when the consumer is gone.
#[allow(clippy::too_many_arguments)]
async fn publish_event(
    reply: &mpsc::Sender<Result<Value, AiError>>,
    pre_output: &mut Vec<Value>,
    pre_output_bytes: &mut usize,
    visible: &mut bool,
    progress: &mut GenerationProgress,
    value: Value,
) -> bool {
    if !*visible {
        if is_pre_output_event(&value)
            && pre_output.len() < PRE_OUTPUT_BUFFER_EVENTS
            && *pre_output_bytes < PRE_OUTPUT_BUFFER_BYTES
        {
            *pre_output_bytes = pre_output_bytes.saturating_add(
                serde_json::to_string(&value)
                    .map(|text| text.len())
                    .unwrap_or_default(),
            );
            pre_output.push(value);
            return true;
        }
        *visible = true;
        for buffered in pre_output.drain(..) {
            progress.observe(&buffered);
            if reply.send(Ok(buffered)).await.is_err() {
                return false;
            }
        }
        *pre_output_bytes = 0;
    }
    progress.observe(&value);
    reply.send(Ok(value)).await.is_ok()
}

/// Classifies and publishes one decoded provider event.
///
/// Returns `Some(outcome)` when the attempt must end.
#[allow(clippy::too_many_arguments)]
async fn handle_provider_event(
    value: Value,
    command: &RequestCommand,
    continuation: &mut Option<Continuation>,
    pre_output: &mut Vec<Value>,
    pre_output_bytes: &mut usize,
    visible: &mut bool,
    progress: &mut GenerationProgress,
) -> Option<AttemptOutcome> {
    let is_terminal = terminal_kind(&value).is_some();
    let connection_refresh = connection_refresh_error(&value) || failed_terminal(&value);
    // A connection-lifetime rejection or a stale provider-side continuation
    // before any output is retryable: nothing visible was published, so a fresh
    // socket with the full local replay cannot duplicate output or work.
    // Upstream does the same (`openai-codex-responses.ts:337-344`). Every other
    // provider failure keeps the pre-existing behavior: publish it and retire.
    let retryable_provider = connection_refresh_error(&value) || stale_continuation_error(&value);
    if retryable_provider && !*visible {
        return Some(interrupted(
            format!(
                "Responses WebSocket provider rejection before output: {}",
                issue_code(&value).unwrap_or_else(|| "unspecified code".to_owned())
            ),
            false,
            progress,
        ));
    }
    if is_terminal {
        update_continuation(&command.body, &value, continuation);
    }
    if !publish_event(
        &command.reply,
        pre_output,
        pre_output_bytes,
        visible,
        progress,
        value,
    )
    .await
    {
        return Some(AttemptOutcome::Abandoned);
    }
    if connection_refresh {
        return Some(AttemptOutcome::Forwarded);
    }
    if is_terminal {
        return Some(AttemptOutcome::Completed);
    }
    None
}

/// Pumps one attempt's provider events into the consumer channel, including the
/// unchanged ping/pong heartbeat and acknowledgement deadline.
async fn pump_generation<S>(
    socket: &mut S,
    command: &RequestCommand,
    continuation: &mut Option<Continuation>,
) -> AttemptOutcome
where
    S: futures_core::Stream<Item = Result<Message, tungstenite::Error>>
        + futures_util::Sink<Message, Error = tungstenite::Error>
        + Unpin,
{
    let mut pre_output: Vec<Value> = Vec::new();
    let mut pre_output_bytes = 0_usize;
    let mut visible = false;
    let mut progress = GenerationProgress::default();
    // Heartbeats prove only that the transport path still carries control
    // frames. They are intentionally kept out of `reply`, so the client
    // continues to apply its independent model-response idle deadline.
    let heartbeat = tokio::time::sleep(command.liveness.interval);
    tokio::pin!(heartbeat);
    let heartbeat_ack = tokio::time::sleep(HEARTBEAT_ACK_DEADLINE);
    tokio::pin!(heartbeat_ack);
    let mut expected_pong: Option<tungstenite::Bytes> = None;
    let mut heartbeat_sequence = 0_u64;
    loop {
        let message = tokio::select! {
            biased;
            _ = command.reply.closed() => {
                // The consumer dropped or timed out. Stop the provider-side
                // stream instead of leaving this actor and socket blocked
                // forever waiting for another frame.
                return AttemptOutcome::Abandoned;
            }
            _ = &mut heartbeat_ack, if expected_pong.is_some() => {
                return interrupted(
                    "Responses WebSocket heartbeat acknowledgement timed out; network path may have been lost",
                    visible,
                    &progress,
                );
            }
            _ = &mut heartbeat, if expected_pong.is_none() => {
                heartbeat_sequence = heartbeat_sequence.wrapping_add(1);
                let payload: tungstenite::Bytes = heartbeat_sequence.to_be_bytes().to_vec().into();
                let ping_result = tokio::select! {
                    biased;
                    _ = command.reply.closed() => return AttemptOutcome::Abandoned,
                    result = tokio::time::timeout(
                        command.liveness.acknowledgement_timeout,
                        socket.send(Message::Ping(payload.clone())),
                    ) => result,
                };
                if !matches!(ping_result, Ok(Ok(()))) {
                    return interrupted(
                        "Responses WebSocket heartbeat probe failed; network path may have been lost",
                        visible,
                        &progress,
                    );
                }
                expected_pong = Some(payload);
                heartbeat_ack.as_mut().reset(
                    tokio::time::Instant::now() + command.liveness.acknowledgement_timeout,
                );
                continue;
            }
            message = socket.next() => message,
        };
        let Some(message) = message else {
            return interrupted(
                "Responses WebSocket ended before completion",
                visible,
                &progress,
            );
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                return interrupted(
                    format!("Responses WebSocket read: {error}"),
                    visible,
                    &progress,
                );
            }
        };
        match message {
            Message::Text(text) => {
                let value = match serde_json::from_str::<Value>(text.as_ref()) {
                    Ok(value) => value,
                    Err(error) => {
                        return AttemptOutcome::Fatal {
                            error: AiError::Decode(crate::error::DecodeError::Json(format!(
                                "invalid Responses WebSocket event: {error}"
                            ))),
                        };
                    }
                };
                if let Some(outcome) = handle_provider_event(
                    value,
                    command,
                    continuation,
                    &mut pre_output,
                    &mut pre_output_bytes,
                    &mut visible,
                    &mut progress,
                )
                .await
                {
                    return outcome;
                }
            }
            Message::Binary(bytes) => {
                let value = match serde_json::from_slice::<Value>(&bytes) {
                    Ok(value) => value,
                    Err(error) => {
                        return AttemptOutcome::Fatal {
                            error: AiError::Decode(crate::error::DecodeError::Json(format!(
                                "invalid Responses WebSocket event: {error}"
                            ))),
                        };
                    }
                };
                if let Some(outcome) = handle_provider_event(
                    value,
                    command,
                    continuation,
                    &mut pre_output,
                    &mut pre_output_bytes,
                    &mut visible,
                    &mut progress,
                )
                .await
                {
                    return outcome;
                }
            }
            Message::Ping(payload) => {
                let pong_result = tokio::select! {
                    biased;
                    _ = command.reply.closed() => return AttemptOutcome::Abandoned,
                    result = tokio::time::timeout(
                        command.liveness.acknowledgement_timeout,
                        socket.send(Message::Pong(payload)),
                    ) => result,
                };
                if !matches!(pong_result, Ok(Ok(()))) {
                    return interrupted(
                        "Responses WebSocket control response failed; network path may have been lost",
                        visible,
                        &progress,
                    );
                }
            }
            Message::Close(_) => {
                return interrupted(
                    "Responses WebSocket closed before completion",
                    visible,
                    &progress,
                );
            }
            Message::Pong(payload) => {
                if expected_pong
                    .as_ref()
                    .is_some_and(|expected| expected == &payload)
                {
                    expected_pong = None;
                    heartbeat
                        .as_mut()
                        .reset(tokio::time::Instant::now() + command.liveness.interval);
                }
            }
            Message::Frame(_) => {}
        }
    }
}

/// Runs one generation to completion, recovering a dropped socket.
///
/// A drop before any consumer-visible output replays the full local body on a
/// fresh socket: the cached `previous_response_id` cursor belonged to the dead
/// connection, and nothing visible was published, so the replay cannot
/// duplicate output or provider work.
///
/// A drop after output is resumed instead of failing the turn when the provider
/// retains the generation: the consumer's sequence cursor lets the retrieve
/// endpoint hand back exactly the events nobody has seen, so every delta is
/// delivered once. Both recoveries share one bounded attempt budget and one
/// bounded total wait; when neither is possible the turn fails closed with the
/// typed [`crate::error::StreamProtocolError::ResponseNotResumable`] error
/// rather than replaying output or fabricating a terminal.
async fn run_generation<S>(
    socket: &mut S,
    command: &RequestCommand,
    continuation: &mut Option<Continuation>,
    replay_frame: Option<&str>,
    dialer: &SocketDialer<S>,
) -> GenerationEnd
where
    S: futures_core::Stream<Item = Result<Message, tungstenite::Error>>
        + futures_util::Sink<Message, Error = tungstenite::Error>
        + Unpin,
{
    let resumer = command.resumer.as_ref();
    let mut attempts = 0_u32;
    let reconnect_started = tokio::time::Instant::now();
    let mut cursor = GenerationProgress::default();
    // Once output is visible a fresh socket cannot be replayed without
    // duplicating it, so every later attempt must be a cursor resume.
    let mut resuming = false;
    loop {
        let outcome = if resuming {
            let (Some(resumer), Some(response_id), Some(last_sequence)) =
                (resumer, cursor.response_id.clone(), cursor.last_sequence)
            else {
                return GenerationEnd::Fatal {
                    error: not_resumable(attempts, true, "no resumable cursor"),
                };
            };
            match resumer(response_id, last_sequence).await {
                Ok(mut resumed) => pump_resumed(&mut resumed, command, last_sequence).await,
                // The attempt budget, not the resume error, bounds this loop:
                // an unresumable route still terminates with the typed
                // non-resumable error after the bounded attempts.
                Err(error) => interrupted(
                    format!("Responses resume attempt failed: {error}"),
                    true,
                    &cursor,
                ),
            }
        } else {
            pump_generation(socket, command, continuation).await
        };
        match outcome {
            AttemptOutcome::Completed => return GenerationEnd::Completed,
            AttemptOutcome::Forwarded => return GenerationEnd::Forwarded,
            AttemptOutcome::Abandoned => return GenerationEnd::Abandoned,
            AttemptOutcome::Fatal { error } => return GenerationEnd::Fatal { error },
            AttemptOutcome::Interrupted {
                detail,
                visible,
                progress,
            } => {
                cursor.absorb(&progress);
                if visible {
                    resuming = true;
                }
                let elapsed = reconnect_started.elapsed();
                if attempts >= MAX_SOCKET_RECONNECT_ATTEMPTS || elapsed >= RECONNECT_TOTAL_BUDGET {
                    return GenerationEnd::Fatal {
                        error: not_resumable(attempts, visible, &detail),
                    };
                }
                if visible {
                    // A resume needs a retained generation and a cursor; without
                    // either there is nothing to resume.
                    if resumer.is_none()
                        || cursor.response_id.is_none()
                        || cursor.last_sequence.is_none()
                    {
                        return GenerationEnd::Fatal {
                            error: not_resumable(attempts, true, &detail),
                        };
                    }
                } else if replay_frame.is_none() {
                    // Without a replayable body a reconnect cannot restart the
                    // generation at all, so fail closed instead of guessing.
                    return GenerationEnd::Fatal {
                        error: not_resumable(attempts, false, &detail),
                    };
                }
                attempts += 1;
                let delay = reconnect_delay(attempts).min(RECONNECT_TOTAL_BUDGET - elapsed);
                tokio::select! {
                    biased;
                    _ = command.reply.closed() => return GenerationEnd::Abandoned,
                    _ = tokio::time::sleep(delay) => {}
                }
                if visible {
                    // The next loop iteration dials the resume cursor; no socket
                    // handshake is involved.
                    continue;
                }
                // A fresh socket cannot use the dead connection's cursor.
                *continuation = None;
                match dialer(command.url.clone(), command.headers.clone()).await {
                    Ok(reconnected) => *socket = reconnected,
                    // The attempt budget, not the dial error, bounds this loop:
                    // an unreachable route still terminates with the typed error.
                    Err(_dial_error) => continue,
                }
                let resend = tokio::select! {
                    biased;
                    _ = command.reply.closed() => return GenerationEnd::Abandoned,
                    result = socket.send(Message::Text(replay_frame.unwrap_or_default().to_owned().into())) => result,
                };
                if resend.is_err() {
                    continue;
                }
            }
        }
    }
}

/// Pumps the remaining events of a resumed generation into the consumer.
///
/// Anything at or before the cursor is dropped even though the provider resumes
/// after it: a re-sent event must never duplicate consumer-visible output, so
/// the resume is exactly-once even if the provider replays the boundary.
async fn pump_resumed(
    resumed: &mut mpsc::Receiver<Result<Value, AiError>>,
    command: &RequestCommand,
    starting_after: u64,
) -> AttemptOutcome {
    let mut visible = true;
    let mut progress = GenerationProgress::default();
    let mut continuation = None;
    let mut pre_output: Vec<Value> = Vec::new();
    let mut pre_output_bytes = 0_usize;
    loop {
        let next = tokio::select! {
            biased;
            _ = command.reply.closed() => return AttemptOutcome::Abandoned,
            next = resumed.recv() => next,
        };
        let Some(next) = next else {
            return interrupted(
                "Responses WebSocket resume ended before completion",
                visible,
                &progress,
            );
        };
        let value = match next {
            Ok(value) => value,
            Err(error) => return AttemptOutcome::Fatal { error },
        };
        if value
            .get("sequence_number")
            .and_then(Value::as_u64)
            .is_some_and(|sequence| sequence <= starting_after)
        {
            continue;
        }
        if let Some(outcome) = handle_provider_event(
            value,
            command,
            &mut continuation,
            &mut pre_output,
            &mut pre_output_bytes,
            &mut visible,
            &mut progress,
        )
        .await
        {
            return outcome;
        }
    }
}

async fn run_connection<S>(
    mut socket: S,
    mut commands: mpsc::Receiver<RequestCommand>,
    alive: Arc<AtomicBool>,
    key: Option<String>,
    state: Weak<Mutex<PoolState>>,
    idle_timeout: Duration,
    dialer: SocketDialer<S>,
) where
    S: futures_core::Stream<Item = Result<Message, tungstenite::Error>>
        + futures_util::Sink<Message, Error = tungstenite::Error>
        + Unpin,
{
    let mut continuation = None;
    // Fatal transport failures must mark the actor dead and disable its key
    // before publishing an error (including the request-start acknowledgement).
    // The consumer can immediately request a replacement on another task;
    // cleanup after a send/await is too late to prevent poisoned socket reuse.
    'actor: loop {
        // Poll the socket even without an active request so peer closes and
        // control frames are handled promptly. Control traffic does not extend
        // the request-idle lifetime.
        let idle = tokio::time::sleep(idle_timeout);
        tokio::pin!(idle);
        let mut command = loop {
            if idle.is_elapsed() {
                break 'actor;
            }
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else {
                        break 'actor;
                    };
                    break command;
                }
                message = socket.next() => {
                    match message {
                        Some(Ok(Message::Ping(payload))) => {
                            if !matches!(
                                tokio::time::timeout(
                                    CONNECTION_CLOSE_TIMEOUT,
                                    socket.send(Message::Pong(payload)),
                                )
                                .await,
                                Ok(Ok(()))
                            ) {
                                break 'actor;
                            }
                        }
                        Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                            break 'actor;
                        }
                        Some(Ok(Message::Text(_) | Message::Binary(_))) => {
                            // A data event outside a request cannot safely be
                            // associated with a later generation.
                            alive.store(false, Ordering::Release);
                            disable_key(&state, key.as_deref()).await;
                            break 'actor;
                        }
                    }
                }
                _ = &mut idle => break 'actor,
            }
        };

        // A request future can be cancelled while this command waits behind an
        // active turn. Do not send an orphaned generation after its receiver is
        // already gone.
        if command.reply.is_closed() {
            continue;
        }
        let started = command.started.take();
        let (wire_body, _) = incremental_body(&command.body, continuation.as_ref());
        let Value::Object(mut payload) = wire_body else {
            let _ = started
                .map(|started| started.send(Err("Responses WebSocket body is not an object".to_owned())));
            let _ = command
                .reply
                .send(Err(transport_error(
                    TransportPhase::ResponseHeaders,
                    "Responses WebSocket body is not an object",
                )))
                .await;
            break 'actor;
        };
        payload.insert(
            "type".to_owned(),
            Value::String("response.create".to_owned()),
        );
        let text = match serde_json::to_string(&Value::Object(payload)) {
            Ok(text) => text,
            Err(error) => {
                let message = format!("Responses WebSocket request encoding: {error}");
                let _ = started.map(|started| started.send(Err(message.clone())));
                let _ = command
                    .reply
                    .send(Err(transport_error(
                        TransportPhase::ResponseHeaders,
                        message,
                    )))
                    .await;
                break 'actor;
            }
        };
        // A reconnect replays the complete local window instead of the cached
        // cursor: `previous_response_id` state belongs to the dead connection.
        let replay_frame = full_replay_frame(&command.body);
        if command.reply.is_closed() {
            continue;
        }
        let send_result = tokio::select! {
            biased;
            _ = command.reply.closed() => {
                // Cancelling a send can leave the WebSocket sink in an
                // indeterminate state. Discard it rather than reusing a socket
                // that may have transmitted part or all of the frame.
                alive.store(false, Ordering::Release);
                disable_key(&state, key.as_deref()).await;
                break 'actor;
            }
            result = socket.send(Message::Text(text.into())) => result,
        };
        if let Err(error) = send_result {
            let message = format!("Responses WebSocket request send: {error}");
            alive.store(false, Ordering::Release);
            disable_key(&state, key.as_deref()).await;
            let _ = started.map(|started| started.send(Err(message)));
            break 'actor;
        }
        let _ = started.map(|started| started.send(Ok(())));

        match run_generation(
            &mut socket,
            &command,
            &mut continuation,
            replay_frame.as_deref(),
            &dialer,
        )
        .await
        {
            GenerationEnd::Completed => {}
            GenerationEnd::Abandoned | GenerationEnd::Forwarded => {
                alive.store(false, Ordering::Release);
                disable_key(&state, key.as_deref()).await;
                break 'actor;
            }
            GenerationEnd::Fatal { error } => {
                // Retire before publishing: a later explicit request must not
                // race this actor or reuse its socket.
                alive.store(false, Ordering::Release);
                disable_key(&state, key.as_deref()).await;
                let _ = command.reply.send(Err(error)).await;
                break 'actor;
            }
        }
    }

    alive.store(false, Ordering::Release);
    remove_connection(&state, key.as_deref(), &alive).await;
    close_socket(&mut socket).await;
}

/// The reconnect frame for one command: the complete local body, with no
/// inherited `previous_response_id` cursor.
fn full_replay_frame(body: &Value) -> Option<String> {
    let (wire_body, _) = incremental_body(body, None);
    let Value::Object(mut payload) = wire_body else {
        return None;
    };
    payload.insert(
        "type".to_owned(),
        Value::String("response.create".to_owned()),
    );
    serde_json::to_string(&Value::Object(payload)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;
    use serde_json::json;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;
    use tokio_tungstenite::{accept_async, MaybeTlsStream, WebSocketStream};

    type ClientSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
    type ServerSocket = WebSocketStream<TcpStream>;

    fn item(id: &str) -> Value {
        serde_json::json!({"type": "message", "id": id})
    }

    async fn websocket_pair() -> (ClientSocket, ServerSocket) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            accept_async(stream).await.unwrap()
        });
        let (client, _) = connect_async(format!("ws://{address}/")).await.unwrap();
        (client, server.await.unwrap())
    }

    /// A dialer that fails immediately: tests that never reconnect use it so an
    /// accidental reconnect attempt is visible as a typed failure rather than a
    /// hang.
    fn unreachable_dialer<S>() -> SocketDialer<S>
    where
        S: Send + 'static,
    {
        Arc::new(|_url, _headers| {
            Box::pin(async {
                Err(transport_error(
                    TransportPhase::Connect,
                    "test dialer has no endpoint",
                ))
            })
        })
    }

    /// A dialer that connects to one test listener and counts attempts.
    fn counting_dialer(
        address: std::net::SocketAddr,
    ) -> (SocketDialer<ClientSocket>, Arc<std::sync::atomic::AtomicUsize>) {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let dialer: SocketDialer<ClientSocket> = Arc::new(move |_url, headers| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let request = format!("ws://{address}/")
                    .into_client_request()
                    .map_err(|error| {
                        transport_error(TransportPhase::Connect, format!("test dial: {error}"))
                    })?;
                let mut request = request;
                for (name, value) in headers {
                    if let Some(name) = name {
                        request.headers_mut().insert(name, value);
                    }
                }
                match connect_async(request).await {
                    Ok((socket, _)) => Ok(socket),
                    Err(error) => Err(websocket_connect_error(error)),
                }
            })
        });
        (dialer, attempts)
    }

    async fn spawn_test_actor<S>(
        socket: S,
        state: &Arc<Mutex<PoolState>>,
        key: &str,
        idle_timeout: Duration,
        dialer: SocketDialer<S>,
    ) -> (Connection, JoinHandle<()>)
    where
        S: futures_core::Stream<Item = Result<Message, tungstenite::Error>>
            + futures_util::Sink<Message, Error = tungstenite::Error>
            + Send
            + Unpin
            + 'static,
    {
        let (sender, receiver) = mpsc::channel(4);
        let alive = Arc::new(AtomicBool::new(true));
        let connection = Connection {
            sender,
            alive: Arc::clone(&alive),
        };
        state
            .lock()
            .await
            .sessions
            .insert(key.to_owned(), connection.clone());
        let actor = tokio::spawn(run_connection(
            socket,
            receiver,
            alive,
            Some(key.to_owned()),
            Arc::downgrade(state),
            idle_timeout,
            dialer,
        ));
        (connection, actor)
    }

    #[test]
    fn websocket_connect_failures_require_positive_io_classification() {
        for kind in [
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::TimedOut,
        ] {
            let error = websocket_connect_error(tungstenite::Error::Io(std::io::Error::from(kind)));
            assert!(matches!(error, AiError::NetworkUnavailable(_)));
        }
        // TLS/certificate errors surfaced by rustls use InvalidData, not a
        // transient network kind. Configuration/protocol errors also stay generic.
        for error in [
            tungstenite::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid peer certificate",
            )),
            tungstenite::Error::Url(tungstenite::error::UrlError::UnsupportedUrlScheme),
        ] {
            assert!(matches!(
                websocket_connect_error(error),
                AiError::Transport(_)
            ));
        }
    }

    #[test]
    fn detects_provider_connection_lifetime_errors() {
        assert!(connection_refresh_error(&serde_json::json!({
            "type": "response.failed",
            "response": {
                "error": {
                    "code": "websocket_connection_limit_reached",
                    "message": "Create a new websocket connection to continue."
                }
            }
        })));
        assert!(connection_refresh_error(&serde_json::json!({
            "type": "error",
            "code": "websocket_connection_limit_reached",
            "message": "connection limit reached"
        })));
        assert!(connection_refresh_error(&serde_json::json!({
            "type": "error",
            "error": {
                "code": "gateway_websocket_connection_limit",
                "message": "connection limit reached"
            }
        })));
        assert!(!connection_refresh_error(&serde_json::json!({
            "type": "response.failed",
            "response": {
                "error": {"code": "invalid_request", "message": "bad request"}
            }
        })));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_same_key_handshakes_share_one_connection_and_close_the_loser() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (closed, closed_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            // Do not complete either handshake until both clients arrive. This
            // would deadlock if the first handshake held the global pool lock.
            let (first, _) = listener.accept().await.unwrap();
            let (second, _) = listener.accept().await.unwrap();
            let (first, second) = tokio::join!(accept_async(first), accept_async(second));
            let mut first = first.unwrap();
            let mut second = second.unwrap();
            let closed_message = tokio::select! {
                message = first.next() => message,
                message = second.next() => message,
            };
            let _ = closed.send(matches!(closed_message, Some(Ok(Message::Close(_))) | None));
            let _ = release_rx.await;
        });

        let pool = ResponsesWsPool::default();
        let url = Url::parse(&format!("ws://{address}/")).unwrap();
        let first = tokio::spawn({
            let pool = pool.clone();
            let url = url.clone();
            async move {
                pool.connect(Some("shared"), url, http::HeaderMap::new())
                    .await
            }
        });
        let second = tokio::spawn({
            let pool = pool.clone();
            async move {
                pool.connect(Some("shared"), url, http::HeaderMap::new())
                    .await
            }
        });
        let (first, second) = tokio::time::timeout(Duration::from_secs(3), async move {
            tokio::join!(first, second)
        })
        .await
        .expect("same-key handshakes must not serialize on the global pool lock");
        let first = first.unwrap().unwrap();
        let second = second.unwrap().unwrap();

        assert!(first.sender.same_channel(&second.sender));
        assert_eq!(pool.state.lock().await.sessions.len(), 1);
        assert!(tokio::time::timeout(Duration::from_secs(1), closed_rx)
            .await
            .expect("racing socket was not retired")
            .unwrap());

        let _ = release.send(());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn actor_exit_before_start_is_a_replay_safe_open_failure() {
        let pool = ResponsesWsPool::default();
        let (sender, mut commands) = mpsc::channel(1);
        let connection = Connection {
            sender,
            alive: Arc::new(AtomicBool::new(true)),
        };
        pool.state
            .lock()
            .await
            .sessions
            .insert("stale".to_owned(), connection.clone());

        let request = tokio::spawn({
            let pool = pool.clone();
            async move {
                pool.request(
                    Some("stale"),
                    Url::parse("ws://127.0.0.1:1/").unwrap(),
                    http::HeaderMap::new(),
                    serde_json::json!({"model": "gpt", "input": []}),
                    ResponsesWsLiveness::for_response_idle(Duration::from_secs(60)),
                    Duration::from_secs(2),
                    None,
                )
                .await
            }
        });

        // Simulate an idle actor selecting shutdown after the command entered
        // its queue but before it attempted the WebSocket send.
        drop(
            commands
                .recv()
                .await
                .expect("request command was not queued"),
        );
        let error = request
            .await
            .unwrap()
            .expect_err("request unexpectedly started");
        assert!(matches!(
            error,
            AiError::Transport(TransportError {
                phase: TransportPhase::Connect,
                ..
            })
        ));
        assert!(!connection.alive.load(Ordering::Acquire));
        assert!(!pool.state.lock().await.sessions.contains_key("stale"));
    }

    #[tokio::test]
    async fn queued_request_deadline_closes_receiver_without_replaying() {
        let pool = ResponsesWsPool::default();
        let (sender, mut commands) = mpsc::channel(1);
        pool.state.lock().await.sessions.insert(
            "queued".to_owned(),
            Connection {
                sender,
                alive: Arc::new(AtomicBool::new(true)),
            },
        );
        let error = pool
            .request(
                Some("queued"),
                Url::parse("ws://127.0.0.1:1/").unwrap(),
                http::HeaderMap::new(),
                serde_json::json!({"model": "gpt", "input": []}),
                ResponsesWsLiveness::for_response_idle(Duration::from_secs(60)),
                Duration::from_millis(20),
                None,
            )
            .await
            .expect_err("unacknowledged request must reach its startup deadline");
        assert!(matches!(
            error,
            AiError::Transport(TransportError {
                phase: TransportPhase::ResponseHeaders,
                timeout: true,
                ..
            })
        ));
        let command = commands.recv().await.unwrap();
        assert!(
            command.reply.is_closed(),
            "actor must not send an orphaned queued generation"
        );
        assert!(command
            .started
            .as_ref()
            .is_some_and(oneshot::Sender::is_closed));
        assert!(
            commands.try_recv().is_err(),
            "deadline must not enqueue a replacement"
        );
    }

    #[tokio::test]
    async fn idle_actor_detects_peer_close_and_evicts_itself() {
        let (client, mut server) = websocket_pair().await;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) =
            spawn_test_actor(
            client,
            &state,
            "closed",
            Duration::from_secs(60),
            unreachable_dialer(),
        )
        .await;

        server.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .expect("idle actor did not observe peer close")
            .unwrap();

        assert!(!connection.alive.load(Ordering::Acquire));
        let state = state.lock().await;
        assert!(!state.sessions.contains_key("closed"));
        assert!(!state.disabled.contains("closed"));
    }

    #[tokio::test]
    async fn idle_actor_times_out_and_evicts_itself() {
        let (client, mut server) = websocket_pair().await;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) =
            spawn_test_actor(
            client,
            &state,
            "idle",
            Duration::from_millis(20),
            unreachable_dialer(),
        )
        .await;

        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .expect("idle actor was retained past its timeout")
            .unwrap();

        assert!(!connection.alive.load(Ordering::Acquire));
        assert!(!state.lock().await.sessions.contains_key("idle"));
        let close = tokio::time::timeout(Duration::from_secs(1), server.next())
            .await
            .expect("idle socket was not closed");
        assert!(matches!(close, Some(Ok(Message::Close(_))) | None));
    }

    #[tokio::test]
    async fn idle_actor_does_not_retain_pool_state() {
        let (client, _server) = websocket_pair().await;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) =
            spawn_test_actor(
            client,
            &state,
            "cycle",
            Duration::from_secs(60),
            unreachable_dialer(),
        )
        .await;
        let weak_state = Arc::downgrade(&state);

        drop(connection);
        drop(state);
        assert!(weak_state.upgrade().is_none());
        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .expect("actor did not stop after its pool was dropped")
            .unwrap();
    }

    #[tokio::test]
    async fn dropping_an_active_response_retires_the_socket_and_provider_work() {
        let (client, mut server) = websocket_pair().await;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) =
            spawn_test_actor(
            client,
            &state,
            "cancel",
            Duration::from_secs(60),
            unreachable_dialer(),
        )
        .await;
        let (reply, events) = mpsc::channel(1);
        let (started, started_rx) = oneshot::channel();
        connection
            .sender
            .send(RequestCommand {
                body: serde_json::json!({"model": "gpt", "input": []}),
                url: Url::parse("ws://127.0.0.1:1/").unwrap(),
                headers: http::HeaderMap::new(),
                reply,
                started: Some(started),
                liveness: ResponsesWsLiveness::for_response_idle(Duration::from_secs(60)),
                resumer: None,
            })
            .await
            .unwrap();

        assert!(matches!(server.next().await, Some(Ok(Message::Text(_)))));
        started_rx.await.unwrap().unwrap();
        drop(events);

        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .expect("cancelled request left its WebSocket actor running")
            .unwrap();
        let close = tokio::time::timeout(Duration::from_secs(1), server.next())
            .await
            .expect("cancelled request did not close the provider socket");
        assert!(matches!(close, Some(Ok(Message::Close(_))) | None));
        assert!(!connection.alive.load(Ordering::Acquire));
        let state = state.lock().await;
        assert!(!state.sessions.contains_key("cancel"));
        assert!(state.disabled.contains("cancel"));
    }

    #[tokio::test]
    async fn send_failure_retires_before_start_error_with_a_contended_pool() {
        let (client, _server) = websocket_pair().await;
        // Fail the generation send without relying on OS TCP buffer timing.
        let client = client.with(|message| {
            // `With` may repoll its failed conversion during socket cleanup.
            futures_util::future::poll_fn(move |_| {
                std::task::Poll::Ready(if matches!(message, Message::Text(_)) {
                    Err(tungstenite::Error::ConnectionClosed)
                } else {
                    Ok(message.clone())
                })
            })
        });
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) =
            spawn_test_actor(
            client,
            &state,
            "send-failure",
            Duration::from_secs(60),
            unreachable_dialer(),
        )
        .await;
        let (reply, _events) = mpsc::channel(1);
        let (started, mut started_rx) = oneshot::channel();
        let guard = state.lock().await;
        connection
            .sender
            .send(RequestCommand {
                body: serde_json::json!({"model": "gpt", "input": []}),
                url: Url::parse("ws://127.0.0.1:1/").unwrap(),
                headers: http::HeaderMap::new(),
                reply,
                started: Some(started),
                liveness: ResponsesWsLiveness::for_response_idle(Duration::from_secs(60)),
                resumer: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while connection.alive.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor did not observe send failure");
        assert!(
            matches!(
                started_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "start error escaped before the pool key was disabled"
        );
        drop(guard);
        let error = tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("request send"));
        assert!(state.lock().await.disabled.contains("send-failure"));
        tokio::time::timeout(Duration::from_secs(2), actor)
            .await
            .unwrap()
            .unwrap();
        assert!(!state.lock().await.sessions.contains_key("send-failure"));
    }

    #[tokio::test]
    async fn fatal_events_retire_before_publishing_with_a_contended_pool() {
        for failure in [
            None, // TCP EOF without a Close frame is a WebSocket read error.
            Some(Message::Close(None)),
            Some(Message::Text("{".into())),
            Some(Message::Binary(vec![0xff].into())),
            Some(Message::Text("test EOF".into())),
        ] {
            let (client, mut server) = websocket_pair().await;
            // Tungstenite normally turns unclean TCP EOF into a read error.
            // Also exercise the actor's distinct Stream::None failure branch.
            let client = client.take_while(|message| {
                std::future::ready(
                    !matches!(message, Ok(Message::Text(text)) if text.as_str() == "test EOF"),
                )
            });
            let state = Arc::new(Mutex::new(PoolState::default()));
            let (connection, actor) =
                spawn_test_actor(
                client,
                &state,
                "poisoned",
                Duration::from_secs(60),
                unreachable_dialer(),
            )
            .await;
            let (reply, mut events) = mpsc::channel(1);
            let (started, started_rx) = oneshot::channel();
            connection
                .sender
                .send(RequestCommand {
                    body: serde_json::json!({"model": "gpt", "input": []}),
                    url: Url::parse("ws://127.0.0.1:1/").unwrap(),
                    headers: http::HeaderMap::new(),
                    reply,
                    started: Some(started),
                    liveness: ResponsesWsLiveness::for_response_idle(Duration::from_secs(60)),
                    resumer: None,
                })
                .await
                .unwrap();
            assert!(matches!(server.next().await, Some(Ok(Message::Text(_)))));
            started_rx.await.unwrap().unwrap();
            let output = serde_json::json!({
                "type": "response.output_text.delta", "delta": "provisional"
            });
            server
                .send(Message::Text(output.to_string().into()))
                .await
                .unwrap();
            assert_eq!(events.recv().await.unwrap().unwrap(), output);

            // Holding this lock forces retirement to suspend. The old ordering
            // published the error before waiting for this lock, allowing a
            // consumer to race its next request against an enabled pool key.
            let guard = state.lock().await;
            if let Some(failure) = failure {
                server.send(failure).await.unwrap();
            }
            drop(server);
            tokio::time::timeout(Duration::from_secs(1), async {
                while connection.alive.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("actor did not detect the injected failure");
            assert!(
                matches!(events.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "failure escaped before the pool key was disabled"
            );
            assert!(!guard.disabled.contains("poisoned"));
            drop(guard);

            let error = tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .expect("retirement did not publish the failure")
                .unwrap()
                .unwrap_err();
            assert!(matches!(error, AiError::Transport(_) | AiError::Decode(_)));
            assert!(state.lock().await.disabled.contains("poisoned"));
            assert!(events.recv().await.is_none());
            tokio::time::timeout(Duration::from_secs(2), actor)
                .await
                .expect("failed actor did not exit")
                .unwrap();
            assert!(!state.lock().await.sessions.contains_key("poisoned"));
        }
    }

    #[tokio::test]
    async fn failed_terminals_retire_before_publication_for_text_and_binary() {
        for value in [
            serde_json::json!({"type":"response.failed", "response":{"error":{"code":"unknown_failure","message":"boom"}}}),
            serde_json::json!({"type":"response.failed", "response":{"error":{"code":"invalid_prompt","message":"denied"}}}),
            serde_json::json!({"type":"response.incomplete", "response":{"incomplete_details":{"reason":"upstream_disconnect"}}}),
        ] {
            for binary in [false, true] {
                let failure = if binary {
                    Message::Binary(value.to_string().into_bytes().into())
                } else {
                    Message::Text(value.to_string().into())
                };
                let (client, mut server) = websocket_pair().await;
                let state = Arc::new(Mutex::new(PoolState::default()));
                let (connection, actor) =
                    spawn_test_actor(
                client,
                &state,
                "poisoned",
                Duration::from_secs(60),
                unreachable_dialer(),
            )
            .await;
                let (reply, mut events) = mpsc::channel(1);
                let (started, started_rx) = oneshot::channel();
                connection
                    .sender
                    .send(RequestCommand {
                        body: serde_json::json!({"model": "gpt", "input": []}),
                        url: Url::parse("ws://127.0.0.1:1/").unwrap(),
                        headers: http::HeaderMap::new(),
                        reply,
                        started: Some(started),
                        liveness: ResponsesWsLiveness::for_response_idle(Duration::from_secs(60)),
                        resumer: None,
                    })
                    .await
                    .unwrap();
                assert!(matches!(server.next().await, Some(Ok(Message::Text(_)))));
                started_rx.await.unwrap().unwrap();
                let output = serde_json::json!({
                    "type": "response.output_text.delta", "delta": "provisional"
                });
                server
                    .send(Message::Text(output.to_string().into()))
                    .await
                    .unwrap();
                assert_eq!(events.recv().await.unwrap().unwrap(), output);

                // Holding this lock forces retirement to suspend. The old ordering
                // published the error before waiting for this lock, allowing a
                // consumer to race its next request against an enabled pool key.
                let guard = state.lock().await;
                server.send(failure).await.unwrap();
                tokio::time::timeout(Duration::from_secs(1), async {
                    while connection.alive.load(Ordering::Acquire) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("actor did not detect the injected failure");
                assert!(
                    matches!(events.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                    "failure escaped before the pool key was disabled"
                );
                assert!(!guard.disabled.contains("poisoned"));
                drop(guard);

                let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
                    .await
                    .expect("retirement did not publish the failure")
                    .unwrap()
                    .unwrap();
                assert_eq!(event, value);
                drop(server);
                assert!(state.lock().await.disabled.contains("poisoned"));
                assert!(events.recv().await.is_none());
                tokio::time::timeout(Duration::from_secs(2), actor)
                    .await
                    .expect("failed actor did not exit")
                    .unwrap();
                assert!(!state.lock().await.sessions.contains_key("poisoned"));
            }
        }
    }

    #[test]
    fn continuation_replaces_only_the_new_input_suffix() {
        let first = serde_json::json!({
            "model": "gpt",
            "input": [item("user"), item("tool")],
            "tools": []
        });
        let output = item("assistant");
        let continuation = Continuation {
            fixed_body: without_continuation_fields(&first).unwrap(),
            request_input: first["input"].as_array().unwrap().clone(),
            response_output: vec![output.clone()],
            response_id: "resp_1".to_owned(),
        };
        let next = serde_json::json!({
            "model": "gpt",
            "input": [item("user"), item("tool"), output, item("next")],
            "tools": []
        });
        let (wire, incremental) = incremental_body(&next, Some(&continuation));
        assert!(incremental);
        assert_eq!(wire["previous_response_id"], "resp_1");
        assert_eq!(wire["input"], serde_json::json!([item("next")]));
    }

    #[test]
    fn continuation_keeps_encrypted_reasoning_and_invalidates_changed_controls() {
        let first = serde_json::json!({
            "model": "gpt", "input": [item("user")],
            "tools": [], "reasoning": {"effort": "high"}, "store": false
        });
        let reasoning = serde_json::json!({
            "type": "reasoning", "id": "rs_1", "encrypted_content": "opaque",
            "summary": [], "future": {"keep": true}
        });
        let terminal = serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp_1", "output": [reasoning.clone(), item("assistant")]}
        });
        let mut continuation = None;
        update_continuation(&first, &terminal, &mut continuation);
        let mut next = first.clone();
        next["input"] =
            serde_json::json!([item("user"), reasoning, item("assistant"), item("next")]);
        let original = next.clone();
        let (wire, incremental) = incremental_body(&next, continuation.as_ref());
        assert!(incremental);
        assert_eq!(wire["previous_response_id"], "resp_1");
        assert_eq!(wire["input"], serde_json::json!([item("next")]));
        assert_eq!(
            next, original,
            "HTTP fallback must retain the complete opaque window"
        );
        for (key, value) in [
            (
                "tools",
                serde_json::json!([{"type": "function", "name": "new_tool"}]),
            ),
            ("reasoning", serde_json::json!({"effort": "low"})),
        ] {
            let mut changed = next.clone();
            changed[key] = value;
            let (wire, incremental) = incremental_body(&changed, continuation.as_ref());
            assert!(!incremental);
            assert_eq!(wire, changed);
            assert_eq!(wire["input"][1]["encrypted_content"], "opaque");
        }
        let (wire, incremental) = incremental_body(&next, None);
        assert!(!incremental, "a fresh socket cannot use the old cursor");
        assert_eq!(wire, original);
    }

    #[test]
    fn continuation_falls_back_on_branch_or_shape_change() {
        let first = serde_json::json!({"model": "gpt", "input": [item("user")]});
        let continuation = Continuation {
            fixed_body: without_continuation_fields(&first).unwrap(),
            request_input: first["input"].as_array().unwrap().clone(),
            response_output: vec![item("assistant")],
            response_id: "resp_1".to_owned(),
        };
        let branch = serde_json::json!({"model": "gpt", "input": [item("other")]});
        let (wire, incremental) = incremental_body(&branch, Some(&continuation));
        assert!(!incremental);
        assert_eq!(wire, branch);
    }

    // --- Dropped-socket reconnect and resumption ---

    type ServerScript = Arc<dyn Fn(ServerSocket) -> BoxFuture<'static, ()> + Send + Sync>;

    /// Serves one scripted connection per accept, in order.
    async fn scripted_server(scripts: Vec<ServerScript>) -> (std::net::SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            for script in scripts {
                let (stream, _) = listener.accept().await.unwrap();
                let socket = accept_async(stream).await.unwrap();
                script(socket).await;
            }
        });
        (address, handle)
    }

    fn text_frame(value: Value) -> Message {
        Message::Text(value.to_string().into())
    }

    /// Reads one generation frame and returns its decoded body, asserting the
    /// frame is a `response.create`.
    async fn next_generation_frame(socket: &mut ServerSocket) -> Value {
        let frame = socket.next().await.unwrap().unwrap();
        let Message::Text(text) = frame else {
            panic!("expected a text generation frame");
        };
        let body: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(body["type"], "response.create");
        body
    }

    fn test_generation_request() -> Value {
        serde_json::json!({
            "model": "gpt",
            "input": [{"type": "message", "role": "user", "id": "user-1"}],
            "store": false
        })
    }

    /// Drives one command through the actor and collects forwarded events until
    /// either a terminal arrives or an error ends the stream.
    async fn drive_command(
        connection: &Connection,
        key_url: Url,
        liveness: ResponsesWsLiveness,
    ) -> (Vec<Value>, Option<AiError>) {
        drive_command_with_resumer(connection, key_url, liveness, None).await
    }

    /// As [`drive_command`], with an injected resume source for the post-output
    /// drop path.
    async fn drive_command_with_resumer(
        connection: &Connection,
        key_url: Url,
        liveness: ResponsesWsLiveness,
        resumer: Option<ResponseResumer>,
    ) -> (Vec<Value>, Option<AiError>) {
        let (reply, mut events) = mpsc::channel(16);
        let (started, started_rx) = oneshot::channel();
        connection
            .sender
            .send(RequestCommand {
                body: test_generation_request(),
                url: key_url,
                headers: http::HeaderMap::new(),
                reply,
                started: Some(started),
                liveness,
                resumer,
            })
            .await
            .unwrap();
        started_rx.await.unwrap().unwrap();
        let mut forwarded = Vec::new();
        let mut error = None;
        loop {
            match tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
                Ok(Some(Ok(event))) => {
                    let terminal = terminal_kind(&event).is_some();
                    forwarded.push(event);
                    if terminal {
                        break;
                    }
                }
                Ok(Some(Err(failure))) => {
                    error = Some(failure);
                    break;
                }
                Ok(None) => break,
                Err(_) => panic!("actor stopped forwarding events: {forwarded:?}"),
            }
        }
        (forwarded, error)
    }

    fn deltas(events: &[Value]) -> Vec<Value> {
        events
            .iter()
            .filter(|event| event["type"] == "response.output_text.delta")
            .map(|event| event["delta"].clone())
            .collect()
    }

    fn count_of(events: &[Value], kind: &str) -> usize {
        events
            .iter()
            .filter(|event| event["type"] == kind)
            .count()
    }

    fn liveness() -> ResponsesWsLiveness {
        ResponsesWsLiveness::for_response_idle(Duration::from_secs(60))
    }

    #[test]
    fn reconnect_backoff_is_monotonic_capped_and_within_budget() {
        assert_eq!(reconnect_delay(1), Duration::from_millis(250));
        assert_eq!(reconnect_delay(2), Duration::from_millis(500));
        assert_eq!(reconnect_delay(3), Duration::from_secs(1));
        assert_eq!(reconnect_delay(4), Duration::from_secs(2));
        for attempt in 5..64 {
            assert_eq!(reconnect_delay(attempt), RECONNECT_MAX_DELAY);
        }
        let spent: Duration = (1..=MAX_SOCKET_RECONNECT_ATTEMPTS).map(reconnect_delay).sum();
        assert!(
            spent <= RECONNECT_TOTAL_BUDGET,
            "the whole attempt budget must fit the reconnect deadline: {spent:?}"
        );
    }

    #[tokio::test]
    async fn a_drop_before_output_reconnects_and_resumes_with_each_delta_once() {
        let first = Arc::new(|mut socket: ServerSocket| -> BoxFuture<'static, ()> {
            Box::pin(async move {
                let body = next_generation_frame(&mut socket).await;
                assert!(body.get("previous_response_id").is_none());
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.created", "response": {"id": "resp_1"}
                    })))
                    .await
                    .unwrap();
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.in_progress", "response": {"id": "resp_1"}
                    })))
                    .await
                    .unwrap();
                // Unclean drop while the provider is still thinking: the common
                // long-running Codex failure.
                drop(socket);
            })
        });
        let second = Arc::new(|mut socket: ServerSocket| -> BoxFuture<'static, ()> {
            Box::pin(async move {
                let body = next_generation_frame(&mut socket).await;
                assert!(
                    body.get("previous_response_id").is_none(),
                    "a reconnect must replay the full local body"
                );
                assert_eq!(body["input"], test_generation_request()["input"]);
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.created", "response": {"id": "resp_2"}
                    })))
                    .await
                    .unwrap();
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.output_text.delta", "delta": "Hello "
                    })))
                    .await
                    .unwrap();
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.output_text.delta", "delta": "world"
                    })))
                    .await
                    .unwrap();
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.completed",
                        "response": {"id": "resp_2", "output": []}
                    })))
                    .await
                    .unwrap();
                // Keep the socket alive until the terminal was delivered.
                let _ = tokio::time::timeout(Duration::from_secs(1), socket.next()).await;
            })
        });
        let (address, server) = scripted_server(vec![first, second]).await;
        let (dialer, attempts) = counting_dialer(address);
        let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) = spawn_test_actor(
            initial,
            &state,
            "resume",
            Duration::from_secs(60),
            dialer,
        )
        .await;

        let (events, error) = drive_command(
            &connection,
            Url::parse(&format!("ws://{address}/")).unwrap(),
            liveness(),
        )
        .await;

        assert!(error.is_none(), "a resumable drop must not surface: {error:?}");
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "exactly one reconnect");
        assert_eq!(
            count_of(&events, "response.created"),
            1,
            "the abandoned attempt's prelude must not be duplicated: {events:?}"
        );
        assert_eq!(count_of(&events, "response.in_progress"), 1);
        assert_eq!(deltas(&events), vec![json!("Hello "), json!("world")]);
        let message: String = deltas(&events)
            .iter()
            .map(|delta| delta.as_str().unwrap())
            .collect();
        assert_eq!(message, "Hello world", "each delta exactly once, no gap");
        assert!(connection.alive.load(Ordering::Acquire));
        assert!(!state.lock().await.disabled.contains("resume"));

        drop(connection);
        let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_drop_after_visible_output_fails_closed_without_resuming() {
        let first = Arc::new(|mut socket: ServerSocket| -> BoxFuture<'static, ()> {
            Box::pin(async move {
                let _ = next_generation_frame(&mut socket).await;
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.created", "response": {"id": "resp_1"}
                    })))
                    .await
                    .unwrap();
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.output_text.delta", "delta": "partial"
                    })))
                    .await
                    .unwrap();
                drop(socket);
            })
        });
        let (address, server) = scripted_server(vec![first]).await;
        let (dialer, attempts) = counting_dialer(address);
        let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) = spawn_test_actor(
            initial,
            &state,
            "visible",
            Duration::from_secs(60),
            dialer,
        )
        .await;

        let (events, error) = drive_command(
            &connection,
            Url::parse(&format!("ws://{address}/")).unwrap(),
            liveness(),
        )
        .await;

        assert_eq!(deltas(&events), vec![json!("partial")]);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            0,
            "a generation that already emitted output must never be replayed"
        );
        let error = error.expect("the interrupted turn must fail loudly");
        match error {
            AiError::StreamProtocol(crate::error::StreamProtocolError::ResponseNotResumable {
                attempts,
                visible_output,
                detail,
            }) => {
                assert_eq!(attempts, 0);
                assert!(visible_output);
                assert!(!detail.is_empty());
            }
            other => panic!("expected a typed non-resumable error, got {other:?}"),
        }
        assert!(!connection.alive.load(Ordering::Acquire));
        assert!(state.lock().await.disabled.contains("visible"));

        drop(connection);
        let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
        server.await.unwrap();
    }

    /// A resumer that records every `(response_id, starting_after)` and serves
    /// one scripted result per call. A missing entry fails like an unreachable
    /// retrieve endpoint.
    fn scripted_resumer(
        script: Vec<Option<Vec<Value>>>,
    ) -> (ResponseResumer, Arc<Mutex<Vec<(String, u64)>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let script = Arc::new(script);
        let index = Arc::new(Mutex::new(0_usize));
        let resumer: ResponseResumer = Arc::new(move |response_id, starting_after| {
            let recorded = Arc::clone(&recorded);
            let script = Arc::clone(&script);
            let index = Arc::clone(&index);
            Box::pin(async move {
                recorded.lock().await.push((response_id, starting_after));
                let position = {
                    let mut index = index.lock().await;
                    let position = *index;
                    *index += 1;
                    position
                };
                match script.get(position) {
                    Some(Some(events)) => {
                        let (sender, receiver) = mpsc::channel(32);
                        for event in events.clone() {
                            let _ = sender.send(Ok(event)).await;
                        }
                        Ok(receiver)
                    }
                    _ => Err(transport_error(
                        TransportPhase::Connect,
                        "scripted resume failure",
                    )),
                }
            }) as ResumeFuture
        });
        (resumer, calls)
    }

    fn scripted_stream_that_drops_after(delta: &str) -> ServerScript {
        let delta = delta.to_owned();
        Arc::new(move |mut socket: ServerSocket| -> BoxFuture<'static, ()> {
            let delta = delta.clone();
            Box::pin(async move {
                let _ = next_generation_frame(&mut socket).await;
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.created", "sequence_number": 0,
                        "response": {"id": "resp_1"}
                    })))
                    .await
                    .unwrap();
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.output_text.delta", "sequence_number": 1,
                        "delta": delta
                    })))
                    .await
                    .unwrap();
                // Unclean mid-stream drop: the socket that owns the in-flight
                // response is gone while the provider keeps generating.
                drop(socket);
            })
        })
    }

    #[test]
    fn only_a_stored_request_is_a_resume_candidate() {
        assert!(body_requests_storage(&json!({"store": true})));
        assert!(!body_requests_storage(&json!({"store": false})));
        assert!(!body_requests_storage(&json!({"model": "gpt"})));
    }

    #[tokio::test]
    async fn a_mid_stream_drop_resumes_from_the_cursor_with_each_delta_once() {
        let (address, server) =
            scripted_server(vec![scripted_stream_that_drops_after("Hello ")]).await;
        let (dialer, reconnect_attempts) = counting_dialer(address);
        let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) = spawn_test_actor(
            initial,
            &state,
            "resume-mid",
            Duration::from_secs(60),
            dialer,
        )
        .await;

        // The provider retained the generation, so the resumed read replays the
        // boundary event the consumer already saw plus the unseen remainder.
        let (resumer, calls) = scripted_resumer(vec![Some(vec![
            json!({"type": "response.output_text.delta", "sequence_number": 1, "delta": "Hello "}),
            json!({"type": "response.output_text.delta", "sequence_number": 2, "delta": "world"}),
            json!({"type": "response.completed", "sequence_number": 3,
                   "response": {"id": "resp_1", "output": []}}),
        ])]);

        let (events, error) = drive_command_with_resumer(
            &connection,
            Url::parse(&format!("ws://{address}/")).unwrap(),
            liveness(),
            Some(resumer),
        )
        .await;

        assert!(
            error.is_none(),
            "a resumable mid-stream drop must not surface: {error:?}"
        );
        assert_eq!(
            deltas(&events),
            vec![json!("Hello "), json!("world")],
            "no duplicated or gapped delta: {events:?}"
        );
        assert_eq!(count_of(&events, "response.created"), 1);
        assert_eq!(count_of(&events, "response.completed"), 1);
        assert_eq!(
            reconnect_attempts.load(Ordering::SeqCst),
            0,
            "a cursor resume is not a socket redial"
        );
        assert_eq!(*calls.lock().await, vec![("resp_1".to_owned(), 1)]);
        assert!(connection.alive.load(Ordering::Acquire));
        assert!(!state.lock().await.disabled.contains("resume-mid"));

        drop(connection);
        let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_resume_that_drops_again_continues_from_the_advanced_cursor() {
        let (address, server) =
            scripted_server(vec![scripted_stream_that_drops_after("Hello ")]).await;
        let (dialer, _reconnect_attempts) = counting_dialer(address);
        let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) = spawn_test_actor(
            initial,
            &state,
            "resume-twice",
            Duration::from_secs(60),
            dialer,
        )
        .await;

        let (resumer, calls) = scripted_resumer(vec![
            Some(vec![
                json!({"type": "response.output_text.delta", "sequence_number": 1, "delta": "Hello "}),
                json!({"type": "response.output_text.delta", "sequence_number": 2, "delta": "world"}),
            ]),
            Some(vec![
                json!({"type": "response.output_text.delta", "sequence_number": 2, "delta": "world"}),
                json!({"type": "response.output_text.delta", "sequence_number": 3, "delta": "!"}),
                json!({"type": "response.completed", "sequence_number": 4,
                       "response": {"id": "resp_1", "output": []}}),
            ]),
        ]);

        let (events, error) = drive_command_with_resumer(
            &connection,
            Url::parse(&format!("ws://{address}/")).unwrap(),
            liveness(),
            Some(resumer),
        )
        .await;

        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            deltas(&events),
            vec![json!("Hello "), json!("world"), json!("!")],
            "each delta exactly once across two resumed reads: {events:?}"
        );
        assert_eq!(
            *calls.lock().await,
            vec![("resp_1".to_owned(), 1), ("resp_1".to_owned(), 2)],
            "the second resume continues from the advanced cursor"
        );

        drop(connection);
        let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_unrecoverable_mid_stream_drop_yields_the_typed_error_with_a_bounded_retry() {
        let (address, server) =
            scripted_server(vec![scripted_stream_that_drops_after("partial")]).await;
        let (dialer, _reconnect_attempts) = counting_dialer(address);
        let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) = spawn_test_actor(
            initial,
            &state,
            "resume-unrecoverable",
            Duration::from_secs(60),
            dialer,
        )
        .await;

        // Every resume attempt fails, so the retry budget bounds the loop and
        // the turn fails closed instead of silently disappearing.
        let (resumer, calls) = scripted_resumer(Vec::new());

        let (events, error) = drive_command_with_resumer(
            &connection,
            Url::parse(&format!("ws://{address}/")).unwrap(),
            liveness(),
            Some(resumer),
        )
        .await;

        assert_eq!(deltas(&events), vec![json!("partial")]);
        assert_eq!(
            calls.lock().await.len(),
            MAX_SOCKET_RECONNECT_ATTEMPTS as usize,
            "the resume retry count is bounded"
        );
        match error.expect("an unrecoverable drop must fail closed") {
            AiError::StreamProtocol(crate::error::StreamProtocolError::ResponseNotResumable {
                attempts,
                visible_output,
                detail,
            }) => {
                assert_eq!(attempts, MAX_SOCKET_RECONNECT_ATTEMPTS);
                assert!(visible_output);
                assert!(detail.contains("resume"), "{detail}");
            }
            other => panic!("expected a typed non-resumable error, got {other:?}"),
        }
        assert!(!connection.alive.load(Ordering::Acquire));
        assert!(state.lock().await.disabled.contains("resume-unrecoverable"));

        drop(connection);
        let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_attempts_and_total_wait_are_bounded() {
        // Every connection drops the generation frame immediately.
        let script = || {
            let script: ServerScript = Arc::new(|mut socket: ServerSocket| -> BoxFuture<'static, ()> {
                Box::pin(async move {
                    let _ = socket.next().await;
                    drop(socket);
                })
            });
            script
        };
        let scripts: Vec<ServerScript> = (0..MAX_SOCKET_RECONNECT_ATTEMPTS as usize + 2)
            .map(|_| script())
            .collect();
        let (address, server) = scripted_server(scripts).await;
        let (dialer, attempts) = counting_dialer(address);
        let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) = spawn_test_actor(
            initial,
            &state,
            "bounded",
            Duration::from_secs(60),
            dialer,
        )
        .await;

        let started_at = std::time::Instant::now();
        let (_events, error) = drive_command(
            &connection,
            Url::parse(&format!("ws://{address}/")).unwrap(),
            liveness(),
        )
        .await;
        let elapsed = started_at.elapsed();

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            MAX_SOCKET_RECONNECT_ATTEMPTS as usize,
            "the reconnect budget must be exactly MAX_SOCKET_RECONNECT_ATTEMPTS"
        );
        match error.expect("an unreachable route must fail closed") {
            AiError::StreamProtocol(crate::error::StreamProtocolError::ResponseNotResumable {
                attempts,
                visible_output,
                ..
            }) => {
                assert_eq!(attempts, MAX_SOCKET_RECONNECT_ATTEMPTS);
                assert!(!visible_output, "nothing was ever published");
            }
            other => panic!("expected a typed non-resumable error, got {other:?}"),
        }
        assert!(
            elapsed < RECONNECT_TOTAL_BUDGET + Duration::from_secs(2),
            "reconnect waiting must stay inside the bounded budget: {elapsed:?}"
        );
        assert!(
            elapsed >= RECONNECT_INITIAL_DELAY,
            "attempts must be spaced by the bounded backoff: {elapsed:?}"
        );
        assert!(!connection.alive.load(Ordering::Acquire));

        drop(connection);
        let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_stale_continuation_is_retried_with_the_full_local_body() {
        // One connection serves two commands: the first completes and plants a
        // continuation cursor, the second rejects it as unknown.
        let first = Arc::new(|mut socket: ServerSocket| -> BoxFuture<'static, ()> {
            Box::pin(async move {
                let body = next_generation_frame(&mut socket).await;
                assert!(body.get("previous_response_id").is_none());
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.created", "response": {"id": "resp_1"}
                    })))
                    .await
                    .unwrap();
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.completed",
                        "response": {"id": "resp_1", "output": []}
                    })))
                    .await
                    .unwrap();
                let second = next_generation_frame(&mut socket).await;
                assert_eq!(
                    second.get("previous_response_id"),
                    Some(&json!("resp_1")),
                    "a live socket reuses the cached cursor"
                );
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "error",
                        "code": "previous_response_not_found",
                        "message": "no such response"
                    })))
                    .await
                    .unwrap();
                // Keep serving until the actor reconnects.
                let _ = tokio::time::timeout(Duration::from_secs(1), socket.next()).await;
            })
        });
        let second = Arc::new(|mut socket: ServerSocket| -> BoxFuture<'static, ()> {
            Box::pin(async move {
                let body = next_generation_frame(&mut socket).await;
                assert!(
                    body.get("previous_response_id").is_none(),
                    "a stale cursor must be dropped for the full local body"
                );
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.created", "response": {"id": "resp_2"}
                    })))
                    .await
                    .unwrap();
                socket
                    .send(text_frame(serde_json::json!({
                        "type": "response.completed",
                        "response": {"id": "resp_2", "output": []}
                    })))
                    .await
                    .unwrap();
                let _ = tokio::time::timeout(Duration::from_secs(1), socket.next()).await;
            })
        });
        let (address, server) = scripted_server(vec![first, second]).await;
        let (dialer, attempts) = counting_dialer(address);
        let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) = spawn_test_actor(
            initial,
            &state,
            "stale",
            Duration::from_secs(60),
            dialer,
        )
        .await;
        let url = Url::parse(&format!("ws://{address}/")).unwrap();

        let (first_events, first_error) = drive_command(&connection, url.clone(), liveness()).await;
        assert!(first_error.is_none(), "{first_error:?}");
        assert_eq!(count_of(&first_events, "response.completed"), 1);

        let (second_events, second_error) = drive_command(&connection, url, liveness()).await;
        assert!(
            second_error.is_none(),
            "the stale-cursor rejection must be recovered, not surfaced: {second_error:?}"
        );
        assert_eq!(count_of(&second_events, "response.created"), 1);
        assert_eq!(
            second_events.last().unwrap()["type"],
            "response.completed"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "the stale cursor is retried once on a fresh socket"
        );
        assert!(!state.lock().await.disabled.contains("stale"));

        drop(connection);
        let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
        server.await.unwrap();
    }
}
