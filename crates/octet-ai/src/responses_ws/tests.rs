//! Unit tests for `crate::responses_ws`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::responses_ws`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::{accept_async, MaybeTlsStream, WebSocketStream};

type ClientSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type ServerSocket = WebSocketStream<TcpStream>;

#[tokio::test]
async fn expired_connection_is_replaced_without_replaying_a_generation() {
    let pool = ResponsesWsPool::default();
    let (sender, mut old_commands) = mpsc::channel(1);
    let old = Connection {
        sender,
        alive: Arc::new(AtomicBool::new(true)),
        opened_at: tokio::time::Instant::now() - MAX_CONNECTION_LIFETIME,
    };
    pool.state
        .lock()
        .await
        .sessions
        .insert("aged".into(), old.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("ws://{}/", listener.local_addr().unwrap())).unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        accept_async(stream).await.unwrap()
    });
    let fresh = pool
        .connect(Some("aged"), url, http::HeaderMap::new())
        .await
        .unwrap();
    let mut socket = peer.await.unwrap();
    assert!(fresh.reusable());
    assert!(!fresh.sender.same_channel(&old.sender));
    assert!(
        old_commands.try_recv().is_err(),
        "no old generation command is replayed"
    );
    assert!(!pool.state.lock().await.disabled.contains("aged"));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), socket.next())
            .await
            .is_err(),
        "renewing a socket does not create a provider generation"
    );
    drop(fresh);
    drop(pool);
}

#[tokio::test]
async fn expired_actor_drops_queued_command_before_send_and_does_not_disable_key() {
    let (socket, mut peer) = websocket_pair().await;
    let (sender, commands) = mpsc::channel(1);
    let (reply, _events) = event_channel(64);
    let (started, start_result) = oneshot::channel();
    sender
        .send(RequestCommand {
            body: json!({"model": "fixture", "input": []}),
            reply,
            started: Some(started),
            liveness: ResponsesWsLiveness::for_response_idle(Duration::from_secs(60)),
            resumer: None,
            steering: None,
        })
        .await
        .unwrap();
    let state = Arc::new(Mutex::new(PoolState::default()));
    let alive = Arc::new(AtomicBool::new(true));
    let actor = tokio::spawn(run_connection(
        socket,
        commands,
        alive.clone(),
        Some("expired".into()),
        Arc::downgrade(&state),
        ConnectionTiming {
            idle_timeout: CONNECTION_IDLE_TIMEOUT,
            opened_at: tokio::time::Instant::now() - MAX_CONNECTION_LIFETIME,
        },
        unreachable_dialer(),
    ));
    let message = tokio::time::timeout(Duration::from_secs(1), peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(message, Message::Close(_)),
        "generation sent on expired socket: {message:?}"
    );
    // Reading Close already handles the peer acknowledgement; flushing after
    // the actor has closed can correctly return SendAfterClosing.
    actor.await.unwrap();
    assert!(start_result.await.is_err());
    assert!(!alive.load(Ordering::Acquire));
    assert!(state.lock().await.disabled.is_empty());
}

#[test]
fn websocket_byte_admission_precedes_json_decoding() {
    assert_eq!(
        websocket_config().max_message_size,
        Some(MAX_WS_MESSAGE_BYTES)
    );
    assert_eq!(websocket_config().max_frame_size, Some(MAX_WS_FRAME_BYTES));
    // Invalid JSON must report the byte limit, not a parse error.
    let bytes = vec![b'x'; MAX_WS_MESSAGE_BYTES + 1];
    assert!(matches!(
        decode_websocket_event(&bytes),
        Err(AiError::Decode(crate::error::DecodeError::ResponseTooLarge))
    ));
    let value = json!({"text": "\n\"é"});
    let size = serde_json::to_vec(&value).unwrap().len();
    assert_eq!(bounded_event_size(&value, size), Some(size));
    assert_eq!(bounded_event_size(&value, size - 1), None);
}

#[tokio::test]
async fn event_channel_backpressures_bytes_and_releases_on_receive_or_drop() {
    let (mut sender, mut receiver) = event_channel(64);
    sender.bytes = Arc::new(Semaphore::new(8));
    let value = json!("aa"); // Four serialized bytes.
    sender.send(Ok(value.clone())).await.unwrap();
    sender.send(Ok(value.clone())).await.unwrap();
    assert_eq!(sender.bytes.available_permits(), 0);
    let pending = sender.send(Ok(value.clone()));
    tokio::pin!(pending);
    assert!(matches!(
        futures_util::poll!(&mut pending),
        std::task::Poll::Pending
    ));
    assert_eq!(receiver.recv().await.unwrap().unwrap(), value);
    pending.await.unwrap();
    assert_eq!(sender.bytes.available_permits(), 0);
    let pending = sender.send(Ok(value));
    tokio::pin!(pending);
    assert!(matches!(
        futures_util::poll!(&mut pending),
        std::task::Poll::Pending
    ));
    receiver.close();
    assert!(sender.is_closed());
    assert!(pending.await.is_err());
    drop(receiver);
    assert_eq!(sender.bytes.available_permits(), 8);
}

#[tokio::test]
async fn prospective_prelude_limit_flushes_without_losing_events() {
    let (reply, mut receiver) = event_channel(64);
    let mut prelude = Vec::new();
    let mut bytes = 0;
    let mut visible = false;
    let mut progress = GenerationProgress::default();
    let first = json!({"type": "response.created", "padding": "x".repeat(35_000)});
    let second = json!({"type": "response.in_progress", "padding": "y".repeat(35_000)});
    assert!(
        publish_event(
            &reply,
            &mut prelude,
            &mut bytes,
            &mut visible,
            &mut progress,
            first.clone()
        )
        .await
    );
    assert_eq!(prelude.len(), 1);
    assert!(bytes <= PRE_OUTPUT_BUFFER_BYTES);
    assert!(
        publish_event(
            &reply,
            &mut prelude,
            &mut bytes,
            &mut visible,
            &mut progress,
            second.clone()
        )
        .await
    );
    assert!(prelude.is_empty());
    assert_eq!(bytes, 0);
    assert!(visible);
    assert_eq!(receiver.recv().await.unwrap().unwrap(), first);
    assert_eq!(receiver.recv().await.unwrap().unwrap(), second);
}

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
) -> (
    SocketDialer<ClientSocket>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
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
        opened_at: tokio::time::Instant::now(),
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
        ConnectionTiming {
            idle_timeout,
            opened_at: connection.opened_at,
        },
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
        opened_at: tokio::time::Instant::now(),
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
                Some(crate::client::DEFAULT_CONNECT_TIMEOUT),
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
            opened_at: tokio::time::Instant::now(),
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
            Some(crate::client::DEFAULT_CONNECT_TIMEOUT),
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
    let (connection, actor) = spawn_test_actor(
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
    let (connection, actor) = spawn_test_actor(
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
    let (connection, actor) = spawn_test_actor(
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
    let (connection, actor) = spawn_test_actor(
        client,
        &state,
        "cancel",
        Duration::from_secs(60),
        unreachable_dialer(),
    )
    .await;
    let (reply, events) = event_channel(1);
    let (started, started_rx) = oneshot::channel();
    connection
        .sender
        .send(RequestCommand {
            steering: None,
            body: serde_json::json!({"model": "gpt", "input": []}),
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
    let (connection, actor) = spawn_test_actor(
        client,
        &state,
        "send-failure",
        Duration::from_secs(60),
        unreachable_dialer(),
    )
    .await;
    let (reply, _events) = event_channel(1);
    let (started, mut started_rx) = oneshot::channel();
    let guard = state.lock().await;
    connection
        .sender
        .send(RequestCommand {
            steering: None,
            body: serde_json::json!({"model": "gpt", "input": []}),
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
    // `malformed` marks a socket event the codec cannot decode at all; the
    // other cases are transport failures detected while output was already
    // visible, which the row turns into the typed non-resumable error
    // instead of the replayable heartbeat timeout that pre-dated it.
    for (failure, malformed) in [
        // TCP EOF without a Close frame is a WebSocket read error.
        (None, false),
        (Some(Message::Close(None)), false),
        (Some(Message::Text("{".into())), true),
        (Some(Message::Binary(vec![0xff].into())), true),
        (Some(Message::Text("test EOF".into())), false),
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
        let (connection, actor) = spawn_test_actor(
            client,
            &state,
            "poisoned",
            Duration::from_secs(60),
            unreachable_dialer(),
        )
        .await;
        let (reply, mut events) = event_channel(1);
        let (started, started_rx) = oneshot::channel();
        connection
            .sender
            .send(RequestCommand {
                steering: None,
                body: serde_json::json!({"model": "gpt", "input": []}),
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
        // A malformed frame is a decode failure. Every other injected
        // failure arrives after visible output was published (`provisional`
        // above) and this request carries no resumer, so the terminal is the
        // typed non-resumable error rather than the old replayable
        // `TransportPhase::Body` heartbeat timeout.
        match (malformed, &error) {
            (true, AiError::Decode(_)) => {}
            (
                false,
                AiError::StreamProtocol(crate::error::StreamProtocolError::ResponseNotResumable {
                    visible_output: true,
                    ..
                }),
            ) => {}
            _ => panic!("unexpected terminal error: {error:?}"),
        }
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
            let (connection, actor) = spawn_test_actor(
                client,
                &state,
                "poisoned",
                Duration::from_secs(60),
                unreachable_dialer(),
            )
            .await;
            let (reply, mut events) = event_channel(1);
            let (started, started_rx) = oneshot::channel();
            connection
                .sender
                .send(RequestCommand {
                    steering: None,
                    body: serde_json::json!({"model": "gpt", "input": []}),
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
    next["input"] = serde_json::json!([item("user"), reasoning, item("assistant"), item("next")]);
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

/// Joins a scripted server under a bounded wait.
///
/// The accept loop serves exactly one connection per script, so an
/// over-provisioned list would block forever on an `accept` that never
/// arrives. Bounding the join turns that harness mismatch into a loud
/// failure instead of a hang.
async fn finish_scripted_server(server: JoinHandle<()>) {
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("scripted server did not finish: script count must match the connection count")
        .expect("a scripted server task panicked");
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
    _key_url: Url,
    liveness: ResponsesWsLiveness,
    resumer: Option<ResponseResumer>,
) -> (Vec<Value>, Option<AiError>) {
    let (reply, mut events) = event_channel(16);
    let (started, started_rx) = oneshot::channel();
    connection
        .sender
        .send(RequestCommand {
            steering: None,
            body: test_generation_request(),
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
    events.iter().filter(|event| event["type"] == kind).count()
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
    let spent: Duration = (1..=MAX_SOCKET_RECONNECT_ATTEMPTS)
        .map(reconnect_delay)
        .sum();
    assert!(
        spent <= RECONNECT_TOTAL_BUDGET,
        "the whole attempt budget must fit the reconnect deadline: {spent:?}"
    );
}

#[tokio::test]
async fn a_drop_before_output_preserves_progress_without_replaying_inference() {
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
    let (address, server) = scripted_server(vec![first]).await;
    let (dialer, attempts) = counting_dialer(address);
    let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
    let state = Arc::new(Mutex::new(PoolState::default()));
    let (connection, actor) =
        spawn_test_actor(initial, &state, "resume", Duration::from_secs(60), dialer).await;

    let (events, error) = drive_command(
        &connection,
        Url::parse(&format!("ws://{address}/")).unwrap(),
        liveness(),
    )
    .await;

    assert!(matches!(
        error,
        Some(AiError::StreamProtocol(
            crate::error::StreamProtocolError::ResponseNotResumable {
                attempts: 0,
                visible_output: false,
                ..
            }
        ))
    ));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "no hidden inference replay"
    );
    assert_eq!(count_of(&events, "response.created"), 1);
    assert_eq!(count_of(&events, "response.in_progress"), 1);
    assert!(deltas(&events).is_empty());
    assert!(!connection.alive.load(Ordering::Acquire));
    assert!(state.lock().await.disabled.contains("resume"));

    drop(connection);
    let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
    finish_scripted_server(server).await;
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
    let (connection, actor) =
        spawn_test_actor(initial, &state, "visible", Duration::from_secs(60), dialer).await;

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
    finish_scripted_server(server).await;
}

type ResumeCalls = Arc<Mutex<Vec<(String, u64)>>>;

/// A resumer that records every `(response_id, starting_after)` and serves
/// one scripted result per call. A missing entry fails like an unreachable
/// retrieve endpoint.
fn scripted_resumer(script: Vec<Option<Vec<Value>>>) -> (ResponseResumer, ResumeCalls) {
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
async fn a_pending_resume_opener_obeys_receiver_cancellation_and_absolute_budget() {
    struct DropSignal(Arc<AtomicBool>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    for cancel in [true, false] {
        let (address, server) =
            scripted_server(vec![scripted_stream_that_drops_after("partial")]).await;
        let (dialer, _) = counting_dialer(address);
        let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
        let state = Arc::new(Mutex::new(PoolState::default()));
        let (connection, actor) = spawn_test_actor(
            initial,
            &state,
            "pending-resume",
            Duration::from_secs(60),
            dialer,
        )
        .await;
        let opened = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let resumer: ResponseResumer = {
            let opened = Arc::clone(&opened);
            let dropped = Arc::clone(&dropped);
            Arc::new(move |_, _| {
                let opened = Arc::clone(&opened);
                let dropped = Arc::clone(&dropped);
                Box::pin(async move {
                    let _guard = DropSignal(dropped);
                    opened.notify_one();
                    std::future::pending().await
                })
            })
        };
        let (reply, mut events) = event_channel(16);
        connection
            .sender
            .send(RequestCommand {
                body: test_generation_request(),
                reply,
                started: None,
                liveness: liveness(),
                resumer: Some(resumer),
                steering: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), opened.notified())
            .await
            .unwrap();
        if cancel {
            drop(events);
        } else {
            let error =
                tokio::time::timeout(RECONNECT_TOTAL_BUDGET + Duration::from_secs(1), async {
                    loop {
                        match events.recv().await {
                            Some(Err(error)) => break error,
                            Some(Ok(_)) => {}
                            None => panic!("resume must publish a terminal error"),
                        }
                    }
                })
                .await
                .expect("absolute resume deadline");
            assert!(matches!(
                error,
                AiError::StreamProtocol(
                    crate::error::StreamProtocolError::ResponseNotResumable { .. }
                )
            ));
        }
        tokio::time::timeout(Duration::from_secs(2), actor)
            .await
            .expect("actor must settle")
            .unwrap();
        assert!(
            dropped.load(Ordering::SeqCst),
            "pending HTTP opener was dropped"
        );
        assert!(!connection.alive.load(Ordering::Acquire));
        finish_scripted_server(server).await;
    }
}

#[tokio::test]
async fn a_mid_stream_drop_resumes_from_the_cursor_with_each_delta_once() {
    let (address, server) = scripted_server(vec![
        scripted_stream_that_drops_after("Hello "),
        // The next turn after a resumed drop must reach a live session: the
        // pool dials a fresh socket for the same key.
        Arc::new(|mut socket: ServerSocket| -> BoxFuture<'static, ()> {
            Box::pin(async move {
                let body = next_generation_frame(&mut socket).await;
                assert!(
                    body.get("previous_response_id").is_none(),
                    "a fresh session must receive the full local body"
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
        }),
    ])
    .await;
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
    // The resumed read supersedes the socket that dropped mid-generation.
    // The dead socket must not stay advertised as usable (`alive` is what
    // `ResponsesWsPool::connect` reuses), but the pool key must stay enabled
    // so the next turn dials a fresh session instead of failing.
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let retired = !connection.alive.load(Ordering::Acquire)
                && !state.lock().await.sessions.contains_key("resume-mid");
            if retired {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a socket dropped mid-generation must not stay advertised as usable");
    assert!(!state.lock().await.disabled.contains("resume-mid"));

    // A later turn on the same key must still complete: the pool dials a
    // fresh session and sends the full local body to it.
    let pool = ResponsesWsPool {
        state: Arc::clone(&state),
    };
    let mut next = pool
        .request(
            Some("resume-mid"),
            Url::parse(&format!("ws://{address}/")).unwrap(),
            http::HeaderMap::new(),
            test_generation_request(),
            liveness(),
            Duration::from_secs(5),
            Some(crate::client::DEFAULT_CONNECT_TIMEOUT),
            None,
        )
        .await
        .expect("the key left behind by a resumed drop must be reusable");
    let mut completed = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), next.recv())
            .await
            .expect("the replacement session stopped forwarding events")
        {
            Some(Ok(value)) if terminal_kind(&value).is_some() => {
                assert_eq!(value["type"], "response.completed");
                completed = true;
                break;
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => panic!("the replacement session failed: {error:?}"),
            None => break,
        }
    }
    assert!(completed, "the replacement session must finish the turn");
    drop(next);

    drop(connection);
    let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
    finish_scripted_server(server).await;
}

#[tokio::test]
async fn a_resume_that_drops_again_continues_from_the_advanced_cursor() {
    let (address, server) = scripted_server(vec![scripted_stream_that_drops_after("Hello ")]).await;
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
    finish_scripted_server(server).await;
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
    finish_scripted_server(server).await;
}

#[tokio::test]
async fn an_accepted_request_without_events_is_not_replayed() {
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
    let (address, server) = scripted_server(vec![script()]).await;
    let (dialer, attempts) = counting_dialer(address);
    let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
    let state = Arc::new(Mutex::new(PoolState::default()));
    let (connection, actor) =
        spawn_test_actor(initial, &state, "bounded", Duration::from_secs(60), dialer).await;

    let (_events, error) = drive_command(
        &connection,
        Url::parse(&format!("ws://{address}/")).unwrap(),
        liveness(),
    )
    .await;

    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "an ambiguous physical attempt must never be hidden by replay"
    );
    match error.expect("an unreachable route must fail closed") {
        AiError::StreamProtocol(crate::error::StreamProtocolError::ResponseNotResumable {
            attempts,
            visible_output,
            ..
        }) => {
            assert_eq!(attempts, 0);
            assert!(!visible_output, "nothing was ever published");
        }
        other => panic!("expected a typed non-resumable error, got {other:?}"),
    }
    assert!(!connection.alive.load(Ordering::Acquire));

    drop(connection);
    let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
    finish_scripted_server(server).await;
}

#[tokio::test]
async fn a_stale_continuation_is_fenced_and_forwarded_without_replay() {
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
    let (address, server) = scripted_server(vec![first]).await;
    let (dialer, attempts) = counting_dialer(address);
    let initial = connect_async(format!("ws://{address}/")).await.unwrap().0;
    let state = Arc::new(Mutex::new(PoolState::default()));
    let (connection, actor) =
        spawn_test_actor(initial, &state, "stale", Duration::from_secs(60), dialer).await;
    let url = Url::parse(&format!("ws://{address}/")).unwrap();

    let (first_events, first_error) = drive_command(&connection, url.clone(), liveness()).await;
    assert!(first_error.is_none(), "{first_error:?}");
    assert_eq!(count_of(&first_events, "response.completed"), 1);

    let (second_events, second_error) = drive_command(&connection, url, liveness()).await;
    assert!(
        second_error.is_none(),
        "raw provider rejection, not a local error"
    );
    assert_eq!(second_events.len(), 1);
    assert_eq!(second_events[0]["code"], "previous_response_not_found");
    assert_eq!(attempts.load(Ordering::SeqCst), 0, "no hidden retry");
    assert!(!connection.alive.load(Ordering::Acquire));
    assert!(state.lock().await.disabled.contains("stale"));

    drop(connection);
    let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
    finish_scripted_server(server).await;
}
