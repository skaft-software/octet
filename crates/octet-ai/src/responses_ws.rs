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
use tokio::sync::{mpsc, oneshot, Mutex, OwnedSemaphorePermit, Semaphore};
#[cfg(test)]
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{connect_async_with_config, tungstenite::client::IntoClientRequest};
use url::Url;

use crate::error::{AiError, ConfigError, TransportError, TransportPhase};

#[path = "responses_steering.rs"]
mod steering_transport;
pub(crate) use steering_transport::SteeringOperation;

const RESPONSES_WEBSOCKETS_BETA: &str = "responses_websockets=2026-02-06";
const EVENT_CHANNEL_CAPACITY: usize = 64;
// Preserve tungstenite's message/frame ceilings, and independently bound queued
// serialized event bytes. The latter is not a claim about exact Value heap size.
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_WS_FRAME_BYTES: usize = 16 * 1024 * 1024;
const EVENT_CHANNEL_BYTES: usize = 64 * 1024 * 1024;

fn websocket_config() -> tungstenite::protocol::WebSocketConfig {
    tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_WS_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_WS_FRAME_BYTES))
}

// Count without allocating a serialized copy, stopping as soon as the bound is
// exceeded. Used both for channel admission and prospective prelude admission.
fn bounded_event_size(value: &Value, limit: usize) -> Option<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .filter(|size| *size <= self.limit)
                .ok_or_else(|| std::io::Error::other("event exceeds byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.bytes)
}

struct QueuedEvent {
    event: Result<Value, AiError>,
    _permit: OwnedSemaphorePermit,
}

pub(crate) struct EventSender {
    sender: mpsc::Sender<QueuedEvent>,
    bytes: Arc<Semaphore>,
}

#[derive(Debug)]
pub(crate) struct EventReceiver {
    receiver: mpsc::Receiver<QueuedEvent>,
}

impl EventReceiver {
    pub(crate) fn close(&mut self) {
        self.receiver.close();
    }

    pub(crate) async fn recv(&mut self) -> Option<Result<Value, AiError>> {
        self.receiver.recv().await.map(|queued| queued.event)
    }

    #[cfg(test)]
    fn try_recv(&mut self) -> Result<Result<Value, AiError>, mpsc::error::TryRecvError> {
        self.receiver.try_recv().map(|queued| queued.event)
    }
}

impl EventSender {
    fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    async fn closed(&self) {
        self.sender.closed().await
    }

    pub(crate) async fn send(&self, event: Result<Value, AiError>) -> Result<(), ()> {
        let size = match &event {
            Ok(value) => bounded_event_size(value, EVENT_CHANNEL_BYTES),
            Err(_) => Some(0),
        };
        let size = size.ok_or(())?;
        let permit = tokio::select! {
            biased;
            _ = self.sender.closed() => return Err(()),
            permit = Arc::clone(&self.bytes).acquire_many_owned(size as u32) =>
                permit.map_err(|_| ())?,
        };
        self.sender
            .send(QueuedEvent {
                event,
                _permit: permit,
            })
            .await
            .map_err(|_| ())
    }
}

pub(crate) fn event_channel(capacity: usize) -> (EventSender, EventReceiver) {
    let (sender, receiver) = mpsc::channel(capacity);
    (
        EventSender {
            sender,
            bytes: Arc::new(Semaphore::new(EVENT_CHANNEL_BYTES)),
        },
        EventReceiver { receiver },
    )
}

fn decode_websocket_event(bytes: &[u8]) -> Result<Value, AiError> {
    if bytes.len() > MAX_WS_MESSAGE_BYTES {
        return Err(crate::error::DecodeError::ResponseTooLarge.into());
    }
    serde_json::from_slice(bytes).map_err(|error| {
        crate::error::DecodeError::Json(format!("invalid Responses WebSocket event: {error}"))
            .into()
    })
}
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONNECTION_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// Normal liveness probes catch a lost route well before the five-minute
/// response-progress deadline without burdening an otherwise idle socket.
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_HEARTBEAT_ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounded cursor-retrieval attempts for one stored generation. This budget
/// never authorizes replaying inference; only the host can replace a generation.
const MAX_SOCKET_RECONNECT_ATTEMPTS: u32 = 3;
/// First stored-response retrieval delay; doubled per attempt and capped by
/// [`RECONNECT_MAX_DELAY`].
const RECONNECT_INITIAL_DELAY: Duration = Duration::from_millis(250);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(2);
/// Hard ceiling on stored-response retrieval waiting for one generation. The
/// attempt budget bounds cursor GET requests, never fresh inference sends.
const RECONNECT_TOTAL_BUDGET: Duration = Duration::from_secs(6);
/// Bounds for lifecycle-only buffering. The prefix is flushed on every terminal
/// path so the host sees progress even when inference produced no output.
const PRE_OUTPUT_BUFFER_EVENTS: usize = 16;
const PRE_OUTPUT_BUFFER_BYTES: usize = 64 * 1024;
/// Upper bound on one interruption detail carried into a terminal error.
const MAX_INTERRUPTION_DETAIL_BYTES: usize = 512;
/// Inactive heartbeat-ack deadline. The timer is only armed once a probe is
/// outstanding (its reset uses the configured acknowledgement timeout), so the
/// initial value must be effectively never.
const HEARTBEAT_ACK_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);

/// Bounded exponential backoff for one cursor retrieval attempt (1-based).
fn reconnect_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(8);
    RECONNECT_INITIAL_DELAY
        .saturating_mul(1_u32 << shift)
        .min(RECONNECT_MAX_DELAY)
}

/// Whether an event is a pre-output lifecycle event. Anything else is
/// consumer-visible content (or a terminal). Neither permits inference replay.
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
type DialFuture<S> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<S, AiError>> + Send>>;
pub(crate) type SocketDialer<S> = Arc<dyn Fn(Url, http::HeaderMap) -> DialFuture<S> + Send + Sync>;

/// Production socket type for a Responses WebSocket endpoint.
pub(crate) type ResponsesSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
            match connect_async_with_config(request, Some(websocket_config()), false).await {
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

/// Post-send liveness failure does not authorize transport-local replay. The
/// host owns any replacement, including attempt limits and unknown usage.
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
    let fixed = object
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "input" | "previous_response_id" | "generate"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
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
        || without_continuation_fields(body).as_ref() != Some(&continuation.fixed_body)
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

    let mut object = body
        .as_object()
        .expect("input belongs to an object")
        .iter()
        .filter(|(key, _)| key.as_str() != "input")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<String, Value>>();
    object.insert(
        "previous_response_id".to_owned(),
        Value::String(continuation.response_id.clone()),
    );
    object.insert(
        "input".to_owned(),
        Value::Array(input[baseline_len..].to_vec()),
    );
    (Value::Object(object), true)
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
    steering: Option<SteeringOperation>,
    body: Value,
    reply: EventSender,
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
        let (socket, _) =
            match connect_async_with_config(request, Some(websocket_config()), false).await {
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
    #[expect(
        clippy::too_many_arguments,
        reason = "Transport entry point keeps endpoint, request, and independent startup/resume policies explicit"
    )]
    pub(crate) async fn request(
        &self,
        key: Option<&str>,
        url: Url,
        headers: http::HeaderMap,
        body: Value,
        liveness: ResponsesWsLiveness,
        startup_timeout: Duration,
        connect_timeout: Option<Duration>,
        resumer: Option<ResponseResumer>,
    ) -> Result<EventReceiver, AiError> {
        self.request_operation(
            key,
            url,
            headers,
            body,
            liveness,
            startup_timeout,
            connect_timeout,
            resumer,
            None,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Operation entry keeps endpoint, request, startup/resume, and steering policies explicit"
    )]
    pub(crate) async fn request_operation(
        &self,
        key: Option<&str>,
        url: Url,
        headers: http::HeaderMap,
        body: Value,
        liveness: ResponsesWsLiveness,
        startup_timeout: Duration,
        connect_timeout: Option<Duration>,
        resumer: Option<ResponseResumer>,
        steering: Option<SteeringOperation>,
    ) -> Result<EventReceiver, AiError> {
        let deadline = tokio::time::Instant::now() + startup_timeout;
        // Only this future owns connection establishment; no generation command
        // exists yet. A timeout here is proven safe for HTTP fallback.
        let connection = tokio::time::timeout(
            connect_timeout.map_or(startup_timeout, |timeout| startup_timeout.min(timeout)),
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
            self.send_request_operation(key, connection, body, liveness, resumer, steering),
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

    async fn send_request_operation(
        &self,
        key: Option<&str>,
        connection: Connection,
        body: Value,
        liveness: ResponsesWsLiveness,
        resumer: Option<ResponseResumer>,
        steering: Option<SteeringOperation>,
    ) -> Result<EventReceiver, AiError> {
        let (reply, events) = event_channel(EVENT_CHANNEL_CAPACITY);
        let (started, started_result) = oneshot::channel();
        let command = RequestCommand {
            steering,
            body,
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
            .request(
                Some(key),
                url,
                headers,
                body,
                liveness,
                startup_timeout,
                Some(crate::client::DEFAULT_CONNECT_TIMEOUT),
                None,
            )
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
/// because the connection that owned it went away. Upstream pi's
/// `openai-codex-responses.ts` provider retries once with the full local body.
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
    /// The provider failed this generation or rejected the socket. The event is
    /// handed back unpublished so the actor retires and fences the key
    /// *before* the consumer can observe it; a later request must not race this
    /// socket, and an immediate retry must take the safe HTTP fallback.
    Forwarded { value: Value },
    /// Retire before publishing the terminal error.
    Fatal { error: AiError },
}

/// One generation attempt's outcome over one socket.
enum AttemptOutcome {
    /// A provider terminal event was forwarded.
    Completed,
    /// The provider failed the generation or rejected the socket. The event is
    /// carried out of the attempt unpublished so the actor can retire first.
    Forwarded { value: Value },
    /// The socket or heartbeat died. `visible` and the response cursor permit
    /// bounded stored-response retrieval after output, otherwise a typed
    /// non-resumable failure. Provider rejections are forwarded unchanged.
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

/// Flushes this attempt's buffered pre-output lifecycle events to the consumer.
///
/// Marks the attempt consumer-visible: once its own prelude reached the
/// consumer, a later retry on this attempt would duplicate it. Returns `false`
/// when the consumer is gone.
async fn flush_pre_output(
    reply: &EventSender,
    pre_output: &mut Vec<Value>,
    pre_output_bytes: &mut usize,
    visible: &mut bool,
    progress: &mut GenerationProgress,
) -> bool {
    *visible = true;
    for buffered in pre_output.drain(..) {
        progress.observe(&buffered);
        if reply.send(Ok(buffered)).await.is_err() {
            return false;
        }
    }
    *pre_output_bytes = 0;
    true
}

/// Publishes one provider event.
///
/// Pre-output lifecycle events are buffered until output or a terminal outcome
/// arrives. Failure paths flush that same prefix; inference is never replayed.
/// Returns `false` when the consumer is gone.
#[allow(clippy::too_many_arguments)]
async fn publish_event(
    reply: &EventSender,
    pre_output: &mut Vec<Value>,
    pre_output_bytes: &mut usize,
    visible: &mut bool,
    progress: &mut GenerationProgress,
    value: Value,
) -> bool {
    if !*visible {
        if is_pre_output_event(&value) && pre_output.len() < PRE_OUTPUT_BUFFER_EVENTS {
            if let Some(size) = bounded_event_size(
                &value,
                PRE_OUTPUT_BUFFER_BYTES.saturating_sub(*pre_output_bytes),
            ) {
                *pre_output_bytes += size;
                pre_output.push(value);
                return true;
            }
        }
        if !flush_pre_output(reply, pre_output, pre_output_bytes, visible, progress).await {
            return false;
        }
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
    // Check before continuation cloning or channel admission, including events
    // supplied by cursor retrieval rather than the WebSocket decoder.
    if bounded_event_size(&value, EVENT_CHANNEL_BYTES).is_none() {
        return Some(AttemptOutcome::Fatal {
            error: crate::error::DecodeError::ResponseTooLarge.into(),
        });
    }
    let is_terminal = terminal_kind(&value).is_some();
    // Provider rejections belong to the host retry classifier and its shared
    // physical-attempt budget. No transport-local generation replay is allowed,
    // even before visible output: acceptance and billing can already have begun.
    if is_terminal {
        update_continuation(&command.body, &value, continuation);
    }
    if connection_refresh_error(&value)
        || stale_continuation_error(&value)
        || failed_terminal(&value)
    {
        // The provider failed this generation or rejected the socket. Hand the
        // event back unpublished: the actor retires and fences the pool key
        // before it reaches the consumer, so an immediate retry observes the
        // disabled key and takes the safe HTTP fallback instead of racing
        // another command onto this actor.
        //
        // This attempt's buffered lifecycle prelude is flushed first: the
        // provider did accept the response, and the consumer's `Started`/
        // `first_body_seen` diagnostics must still reflect that (the pre-row
        // contract). Flushing here is safe because the attempt ends with the
        // failure, so nothing is duplicated by a later attempt.
        if !flush_pre_output(
            &command.reply,
            pre_output,
            pre_output_bytes,
            visible,
            progress,
        )
        .await
        {
            return Some(AttemptOutcome::Abandoned);
        }
        return Some(AttemptOutcome::Forwarded { value });
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
    if is_terminal {
        return Some(AttemptOutcome::Completed);
    }
    None
}

/// Flushes the abandoned attempt's buffered prelude before a typed terminal.
///
/// The consumer must still learn that the provider accepted the response
/// before the transport failed (the pre-row `Started` / `first_body_seen`
/// contract), and this happens only once the attempt can never be retried, so
/// nothing is duplicated later. Returns `false` when the consumer is gone.
async fn flush_pending_prelude(reply: &EventSender, prelude: &mut Vec<Value>) -> bool {
    for buffered in prelude.drain(..) {
        if reply.send(Ok(buffered)).await.is_err() {
            return false;
        }
    }
    true
}

/// Pumps one attempt's provider events into the consumer channel, including the
/// unchanged ping/pong heartbeat and acknowledgement deadline.
async fn pump_generation<S>(
    socket: &mut S,
    command: &RequestCommand,
    continuation: &mut Option<Continuation>,
    pre_output: &mut Vec<Value>,
    pre_output_bytes: &mut usize,
) -> AttemptOutcome
where
    S: futures_core::Stream<Item = Result<Message, tungstenite::Error>>
        + futures_util::Sink<Message, Error = tungstenite::Error>
        + Unpin,
{
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
            Message::Text(_) | Message::Binary(_) => {
                let value = match decode_websocket_event(message.into_data().as_ref()) {
                    Ok(value) => value,
                    Err(error) => return AttemptOutcome::Fatal { error },
                };
                if let Some(outcome) = handle_provider_event(
                    value,
                    command,
                    continuation,
                    pre_output,
                    pre_output_bytes,
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

/// Runs one generation without replaying inference. A stored response may be
/// retrieved from an explicit cursor after interruption; that is a read of the
/// same generation, not a new response.create. Missing storage/cursor authority
/// fails closed so the host owns replacement budgets and usage uncertainty.
async fn run_generation<S>(
    socket: &mut S,
    command: &RequestCommand,
    continuation: &mut Option<Continuation>,
) -> GenerationEnd
where
    S: futures_core::Stream<Item = Result<Message, tungstenite::Error>>
        + futures_util::Sink<Message, Error = tungstenite::Error>
        + Unpin,
{
    let resumer = command.resumer.as_ref();
    let mut attempts = 0_u32;
    let mut reconnect_started: Option<tokio::time::Instant> = None;
    let mut cursor = GenerationProgress::default();
    // Preserve the generation's lifecycle prelude, including on failure.
    let mut prelude: Vec<Value> = Vec::new();
    let mut prelude_bytes = 0_usize;
    // Once output is visible a fresh socket cannot be replayed without
    // duplicating it, so every later attempt must be a cursor resume.
    let mut resuming = false;
    loop {
        let outcome = if resuming {
            let (Some(resumer), Some(response_id), Some(last_sequence)) =
                (resumer, cursor.response_id.clone(), cursor.last_sequence)
            else {
                if !flush_pending_prelude(&command.reply, &mut prelude).await {
                    return GenerationEnd::Abandoned;
                }
                return GenerationEnd::Fatal {
                    error: not_resumable(attempts, true, "no resumable cursor"),
                };
            };
            let deadline =
                reconnect_started.expect("resume follows interruption") + RECONNECT_TOTAL_BUDGET;
            let opened = tokio::select! {
                biased;
                _ = command.reply.closed() => return GenerationEnd::Abandoned,
                _ = tokio::time::sleep_until(deadline) => return GenerationEnd::Fatal {
                    error: not_resumable(attempts, true, "resume request deadline exceeded"),
                },
                opened = resumer(response_id, last_sequence) => opened,
            };
            match opened {
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
            pump_generation(
                socket,
                command,
                continuation,
                &mut prelude,
                &mut prelude_bytes,
            )
            .await
        };
        match outcome {
            AttemptOutcome::Completed => return GenerationEnd::Completed,
            AttemptOutcome::Forwarded { value } => return GenerationEnd::Forwarded { value },
            AttemptOutcome::Abandoned => return GenerationEnd::Abandoned,
            AttemptOutcome::Fatal { error } => {
                if !flush_pending_prelude(&command.reply, &mut prelude).await {
                    return GenerationEnd::Abandoned;
                }
                return GenerationEnd::Fatal { error };
            }
            AttemptOutcome::Interrupted {
                detail,
                visible,
                progress,
            } => {
                cursor.absorb(&progress);
                if !visible {
                    // Zero output is not proof of nonacceptance. Surface this
                    // physical attempt instead of silently resending it outside
                    // the host's retry and accounting envelope.
                    if !flush_pending_prelude(&command.reply, &mut prelude).await {
                        return GenerationEnd::Abandoned;
                    }
                    return GenerationEnd::Fatal {
                        error: not_resumable(attempts, false, &detail),
                    };
                }
                resuming = true;
                let elapsed = reconnect_started
                    .get_or_insert_with(tokio::time::Instant::now)
                    .elapsed();
                if attempts >= MAX_SOCKET_RECONNECT_ATTEMPTS || elapsed >= RECONNECT_TOTAL_BUDGET {
                    if !flush_pending_prelude(&command.reply, &mut prelude).await {
                        return GenerationEnd::Abandoned;
                    }
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
                        if !flush_pending_prelude(&command.reply, &mut prelude).await {
                            return GenerationEnd::Abandoned;
                        }
                        return GenerationEnd::Fatal {
                            error: not_resumable(attempts, true, &detail),
                        };
                    }
                }
                attempts += 1;
                let delay = reconnect_delay(attempts).min(RECONNECT_TOTAL_BUDGET - elapsed);
                tokio::select! {
                    biased;
                    _ = command.reply.closed() => return GenerationEnd::Abandoned,
                    _ = tokio::time::sleep(delay) => {}
                }
                // Only a cursor GET can be attempted next. Never send another
                // response.create on a fresh socket from inside this command.
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
    _dialer: SocketDialer<S>,
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
        let (wire_body, _) = incremental_body(
            &command.body,
            if command.steering.is_some() {
                None
            } else {
                continuation.as_ref()
            },
        );
        let Value::Object(mut payload) = wire_body else {
            let _ = started.map(|started| {
                started.send(Err("Responses WebSocket body is not an object".to_owned()))
            });
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

        let outcome = if let Some(steering) = command.steering.take() {
            // Steering changes replay lineage; never cache a guessed input prefix.
            continuation = None;
            steering_transport::run(&mut socket, &command, steering).await
        } else {
            run_generation(&mut socket, &command, &mut continuation).await
        };
        match outcome {
            GenerationEnd::Completed => {}
            GenerationEnd::Abandoned => {
                alive.store(false, Ordering::Release);
                disable_key(&state, key.as_deref()).await;
                break 'actor;
            }
            GenerationEnd::Forwarded { value } => {
                // Retire the socket and fence the key *before* publishing the
                // provider failure: the consumer (or an agent retry it drives)
                // must observe the disabled key and take the safe HTTP
                // fallback, never race another command onto this actor.
                alive.store(false, Ordering::Release);
                disable_key(&state, key.as_deref()).await;
                let _ = command.reply.send(Ok(value)).await;
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

#[cfg(test)]
mod tests;
