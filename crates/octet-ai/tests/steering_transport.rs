#![allow(missing_docs)]
use futures_util::{SinkExt, StreamExt};
use octet_ai::*;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_tungstenite::{accept_async, tungstenite::Message as Ws, WebSocketStream};

type Socket = WebSocketStream<TcpStream>;
fn model(url: String) -> Model {
    let features = ResponsesFeatures {
        steering: true,
        ..Default::default()
    };
    Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("gpt-6-test".into()),
            endpoint: EndpointId("test".into()),
            api_name: "gpt-6-test".into(),
            display_name: None,
            protocol: Protocol::OpenAiResponses,
            capabilities: Capabilities {
                responses_features: features,
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 10000,
                max_output_tokens: 2000,
            },
            pricing: None,
            cache: Default::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("test".into()),
            base_url: url.parse().unwrap(),
            auth: Auth::bearer("secret-test-key"),
            default_headers: Default::default(),
            transport: EndpointTransport::WebSocketPreferred,
            runtime: RequestRuntime {
                responses_features: features,
                ..Default::default()
            },
            timeout: Duration::from_secs(2),
        }),
    }
}
fn request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("original".into())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(50),
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: Some("steering-session".into()),
    }
}
fn client() -> AiClient {
    AiClient::with_http_client(reqwest::Client::builder().no_proxy().build().unwrap())
        .with_stream_timeouts(Duration::from_secs(2), Duration::from_secs(5))
}
async fn listener() -> (TcpListener, Model) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = model(format!("http://{}/", listener.local_addr().unwrap()));
    (listener, model)
}
async fn send(socket: &mut Socket, event: Value) {
    socket
        .send(Ws::Text(event.to_string().into()))
        .await
        .unwrap();
}
async fn recv(socket: &mut Socket) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Ws::Text(text) => return serde_json::from_str(&text).unwrap(),
                Ws::Ping(payload) => socket.send(Ws::Pong(payload)).await.unwrap(),
                other => panic!("unexpected client frame {other:?}"),
            }
        }
    })
    .await
    .expect("client command timed out")
}
async fn created(socket: &mut Socket, id: &str) {
    send(
        socket,
        json!({"type":"response.created","response":{"id":id}}),
    )
    .await;
}
async fn text(socket: &mut Socket, text: &str) {
    send(socket,json!({"type":"response.content_part.added","output_index":0,"content_index":0,"part":{"type":"output_text"}})).await;
    send(socket,json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":text})).await;
    send(
        socket,
        json!({"type":"response.output_text.done","output_index":0,"content_index":0}),
    )
    .await;
}
async fn done(socket: &mut Socket, id: &str, steered: bool, output: Value) {
    send(
        socket,
        json!({"type":if steered {"response.incomplete"} else {"response.completed"},
        "response":{"id":id,"status":if steered {"incomplete"}else{"completed"},"output":output,
        "incomplete_details":if steered {json!({"reason":"steered"})}else{Value::Null},
        "usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}),
    )
    .await;
}
async fn accepted(socket: &mut Socket, id: &str, parent: &str) {
    send(
        socket,
        json!({"type":"response.steer.accepted","steer":{"id":id,"previous_response_id":parent}}),
    )
    .await;
}
async fn next(session: &mut SteeringSession) -> Option<Result<SteeringEvent, AiError>> {
    tokio::time::timeout(Duration::from_secs(4), session.next_event())
        .await
        .expect("session event timeout")
}
async fn until_started(session: &mut SteeringSession) {
    loop {
        if matches!(
            next(session).await.unwrap().unwrap(),
            SteeringEvent::Response {
                event: StreamEvent::Started { .. },
                ..
            }
        ) {
            return;
        }
    }
}

#[tokio::test]
async fn steering_mid_generation_late_acceptance_multiple_inputs_preserve_segments() {
    let (listener, model) = listener().await;
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        let initial = recv(&mut socket).await;
        assert_eq!(initial["type"], "response.create");
        assert!(initial.get("stream").is_none());
        assert_eq!(initial["max_output_tokens"], 50);
        created(&mut socket, "r1").await;
        text(&mut socket, "original-prefix").await;
        // The provider deliberately does not terminate until it receives two
        // updates. A command port polled only while idle deadlocks this test.
        for expected in ["first update", "second update"] {
            let steer = recv(&mut socket).await;
            assert_eq!(
                steer,
                json!({"type":"response.steer","previous_response_id":"r1","input":expected})
            );
        }
        done(&mut socket, "r1", false, json!([])).await;
        // Original completed normally before the acceptance acknowledgements.
        accepted(&mut socket, "s1", "r1").await;
        accepted(&mut socket, "s2", "r1").await;
        created(&mut socket, "r2").await;
        text(&mut socket, "successor").await;
        done(&mut socket, "r2", false, json!([])).await;
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    let control = session.control();
    assert_eq!(control.steer("first update".into()).await.unwrap(), 0);
    assert_eq!(control.steer("second update".into()).await.unwrap(), 1);
    let mut finished = Vec::new();
    let mut usages = 0;
    let mut starts = 1;
    while let Some(event) = next(&mut session).await {
        match event.unwrap() {
            SteeringEvent::Response {
                event: StreamEvent::Finished(response),
                ..
            } => finished.push(response),
            SteeringEvent::Response {
                event: StreamEvent::Usage(_),
                ..
            } => usages += 1,
            SteeringEvent::Response {
                event: StreamEvent::Started { .. },
                ..
            } => starts += 1,
            _ => {}
        }
    }
    assert_eq!(starts, 2);
    assert_eq!(usages, 2);
    assert_eq!(finished.len(), 2);
    assert!(
        matches!(&finished[0].message.content[0],AssistantPart::Text(t) if t=="original-prefix")
    );
    assert!(matches!(&finished[1].message.content[0],AssistantPart::Text(t) if t=="successor"));
    for response in &finished {
        assert_eq!(response.usage.output_tokens, 2);
    }
    let states = session.steering_updates();
    assert_eq!(
        states.iter().map(|u| u.local_id).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(states
        .iter()
        .all(|u| matches!(&u.state,SteeringState::Applied {response_id} if response_id=="r2")));
    server.await.unwrap();
}

#[tokio::test]
async fn steering_queued_before_created_waits_and_steered_is_its_own_terminal() {
    let (listener, model) = listener().await;
    let (proceed, ready) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        ready.await.unwrap();
        // No response.steer may precede the response.created frame.
        assert!(
            tokio::time::timeout(Duration::from_millis(30), socket.next())
                .await
                .is_err()
        );
        created(&mut socket, "r1").await;
        assert_eq!(recv(&mut socket).await["previous_response_id"], "r1");
        accepted(&mut socket, "s1", "r1").await;
        text(&mut socket, "not rewritten").await;
        done(&mut socket, "r1", true, json!([])).await;
        created(&mut socket, "r2").await;
        done(&mut socket, "r2", false, json!([])).await;
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    session
        .control()
        .steer("queued before created".into())
        .await
        .unwrap();
    proceed.send(()).unwrap();
    let mut reasons = Vec::new();
    while let Some(event) = next(&mut session).await {
        if let SteeringEvent::Response {
            event: StreamEvent::Finished(r),
            ..
        } = event.unwrap()
        {
            reasons.push(r.stop_reason);
        }
    }
    assert_eq!(reasons, vec![StopReason::Steered, StopReason::EndTurn]);
    server.await.unwrap();
}

fn tool() -> ToolDef {
    ToolDef {
        async_execution: false,
        name: "status".into(),
        description: "status".into(),
        parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
        constrained_sampling: None,
    }
}
#[tokio::test]
async fn steering_sync_tool_pending_continues_on_same_socket_without_replay() {
    let (listener, model) = listener().await;
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        assert_eq!(recv(&mut socket).await["type"], "response.steer");
        accepted(&mut socket, "s1", "r1").await;
        let call = json!({"id":"fc1","type":"function_call","call_id":"call1","name":"status","arguments":"{}"});
        send(
            &mut socket,
            json!({"type":"response.output_item.added","output_index":0,"item":call}),
        )
        .await;
        send(&mut socket,json!({"type":"response.function_call_arguments.done","output_index":0,"arguments":"{}"})).await;
        send(
            &mut socket,
            json!({"type":"response.output_item.done","output_index":0,"item":call}),
        )
        .await;
        done(&mut socket, "r1", false, json!([call])).await;
        send(&mut socket,json!({"type":"response.steer.pending","steer":{"id":"s1","previous_response_id":"r1"},"reason":"waiting_for_required_input","required_input":[{"type":"function_call_output","call_id":"call1","name":"status"}]})).await;
        let continuation = recv(&mut socket).await;
        assert_eq!(continuation["type"], "response.create");
        assert_eq!(continuation["previous_response_id"], "r1");
        assert_eq!(continuation["max_output_tokens"], 17);
        assert_eq!(continuation["input"].as_array().unwrap().len(), 1);
        assert_eq!(continuation["input"][0]["type"], "function_call_output");
        assert_eq!(continuation["input"][0]["call_id"], "call1");
        assert!(!continuation.to_string().contains("steer-input-once"));
        created(&mut socket, "r2").await;
        text(&mut socket, "after tool").await;
        done(&mut socket, "r2", false, json!([])).await;
    });
    let mut req = request();
    req.tools = vec![tool()];
    let mut session = client().steerable_responses(&model, req).await.unwrap();
    until_started(&mut session).await;
    let control = session.control();
    control.steer("steer-input-once".into()).await.unwrap();
    let mut finished = 0;
    let mut pending = false;
    while let Some(event) = next(&mut session).await {
        match event.unwrap() {
            SteeringEvent::Response {
                event: StreamEvent::Finished(response),
                ..
            } => {
                finished += 1;
                if finished == 1 {
                    assert!(matches!(
                        response.message.content[0],
                        AssistantPart::ToolCall(_)
                    ));
                }
            }
            SteeringEvent::Steer(SteeringUpdate {
                state: SteeringState::Pending { .. },
                ..
            }) => {
                pending = true;
                let mut req = request();
                req.tools = vec![tool()];
                req.max_output_tokens = Some(17);
                req.messages = vec![Message::User(UserMessage {
                    content: vec![UserPart::ToolResult(ToolResult {
                        added_tool_names: None,
                        tool_call_id: ToolCallId("call1".into()),
                        content: vec![ToolResultPart::Text("done".into())],
                        is_error: false,
                    })],
                })];
                control.continue_with(req).await.unwrap();
            }
            _ => {}
        }
    }
    assert!(pending);
    assert_eq!(finished, 2);
    server.await.unwrap();
}

#[tokio::test]
async fn steering_rejected_input_does_not_erase_original_or_retry() {
    let (listener, model) = listener().await;
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        let steer = recv(&mut socket).await;
        send(&mut socket,json!({"type":"response.steer.failed","steer":{"id":"s1","previous_response_id":"r1","input":steer["input"]},"error":{"code":"steering_not_supported","message":"secret-test-key"}})).await;
        text(&mut socket, "original survives").await;
        done(&mut socket, "r1", false, json!([])).await;
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    session.control().steer("update".into()).await.unwrap();
    let mut completed = false;
    while let Some(event) = next(&mut session).await {
        if let SteeringEvent::Response {
            event: StreamEvent::Finished(_),
            ..
        } = event.unwrap()
        {
            completed = true;
        }
    }
    assert!(completed);
    assert_eq!(
        session.steering_updates()[0].state,
        SteeringState::Failed {
            code: Some("steering_not_supported".into())
        }
    );
    server.await.unwrap();
}

#[tokio::test]
async fn steering_disconnect_after_acceptance_is_ambiguous_not_replayed() {
    let (listener, model) = listener().await;
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        recv(&mut socket).await;
        accepted(&mut socket, "s1", "r1").await;
        socket.close(None).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    session.control().steer("keep me".into()).await.unwrap();
    let mut failed = false;
    while let Some(event) = next(&mut session).await {
        if event.is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed);
    assert_eq!(
        session.steering_updates()[0].state,
        SteeringState::Ambiguous
    );
    server.await.unwrap();
}

#[tokio::test]
async fn steering_deadline_with_live_pongs_preserves_uncertainty() {
    let (listener, model) = listener().await;
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        recv(&mut socket).await;
        accepted(&mut socket, "s1", "r1").await;
        while let Some(Ok(message)) = socket.next().await {
            match message {
                Ws::Ping(p) => {
                    if socket.send(Ws::Pong(p)).await.is_err() {
                        break;
                    }
                }
                Ws::Close(_) => break,
                _ => {}
            }
        }
    });
    let mut session = client()
        .with_stream_timeouts(Duration::from_millis(160), Duration::from_millis(100))
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    session.control().steer("update".into()).await.unwrap();
    let error = loop {
        if let Some(Err(e)) = next(&mut session).await {
            break e;
        }
    };
    assert!(matches!(
        error,
        AiError::Transport(TransportError { timeout: true, .. })
    ));
    assert_eq!(
        session.steering_updates()[0].state,
        SteeringState::Ambiguous
    );
    server.await.unwrap();
}

#[tokio::test]
async fn steering_cancel_marks_pending_and_closes_local_socket() {
    let (listener, model) = listener().await;
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        recv(&mut socket).await;
        accepted(&mut socket, "s1", "r1").await;
        accepted_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Ws::Close(_)) {
                    break;
                }
            }
        })
        .await
        .unwrap();
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    session.control().steer("pending".into()).await.unwrap();
    accepted_rx.await.unwrap();
    assert_eq!(session.cancel()[0].state, SteeringState::Ambiguous);
    assert!(next(&mut session).await.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn steering_requires_both_authorities_before_dispatch() {
    let (_, mut model) = listener().await;
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_features
        .steering = false;
    let client = client().track_request_dispatch();
    assert!(client.steerable_responses(&model, request()).await.is_err());
    assert!(!client.request_may_have_been_sent());
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_features
        .steering = true;
    Arc::make_mut(&mut model.spec)
        .capabilities
        .responses_features
        .steering = false;
    assert!(client.steerable_responses(&model, request()).await.is_err());
    assert!(!client.request_may_have_been_sent());
}

#[tokio::test]
async fn steering_new_input_targets_successor_and_late_ack_does_not_end_session() {
    let (listener, model) = listener().await;
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        assert_eq!(recv(&mut socket).await["previous_response_id"], "r1");
        done(&mut socket, "r1", true, json!([])).await;
        created(&mut socket, "r2").await;
        // An unusually delayed acknowledgement is still correlated with r1.
        accepted(&mut socket, "s1", "r1").await;
        assert_eq!(recv(&mut socket).await["previous_response_id"], "r2");
        accepted(&mut socket, "s2", "r2").await;
        done(&mut socket, "r2", true, json!([])).await;
        created(&mut socket, "r3").await;
        done(&mut socket, "r3", false, json!([])).await;
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    let control = session.control();
    control.steer("first".into()).await.unwrap();
    let mut finished = 0;
    while let Some(event) = next(&mut session).await {
        match event.unwrap() {
            SteeringEvent::Response {
                response_id,
                event: StreamEvent::Started { .. },
            } if response_id == "r2" => {
                control.steer("second".into()).await.unwrap();
            }
            SteeringEvent::Response {
                event: StreamEvent::Finished(_),
                ..
            } => finished += 1,
            _ => {}
        }
    }
    assert_eq!(finished, 3);
    let states = session.steering_updates();
    assert_eq!(
        states[0].state,
        SteeringState::Applied {
            response_id: "r2".into()
        }
    );
    assert_eq!(
        states[1].state,
        SteeringState::Applied {
            response_id: "r3".into()
        }
    );
    server.await.unwrap();
}

#[tokio::test]
async fn steering_accepted_input_can_fail_without_being_replayed() {
    let (listener, model) = listener().await;
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        recv(&mut socket).await;
        accepted(&mut socket, "s1", "r1").await;
        done(&mut socket, "r1", false, json!([])).await;
        send(&mut socket, json!({"type":"response.steer.failed","steer":{"id":"s1","previous_response_id":"r1","input":"keep"},"error":{"code":"invalid_input"}})).await;
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    session.control().steer("keep".into()).await.unwrap();
    while let Some(event) = next(&mut session).await {
        event.unwrap();
    }
    assert_eq!(
        session.steering_updates()[0].state,
        SteeringState::Failed {
            code: Some("invalid_input".into())
        }
    );
    server.await.unwrap();
}

#[tokio::test]
async fn steering_silent_dead_socket_times_out_without_reconnect() {
    let (listener, model) = listener().await;
    let (release, hold) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        recv(&mut socket).await;
        accepted(&mut socket, "s1", "r1").await;
        // Keep TCP open but never poll or answer Ping. This is a half-open path,
        // not an EOF; the heartbeat must beat the overall deadline.
        hold.await.unwrap();
        drop(socket);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    });
    let mut session = client()
        .with_stream_timeouts(Duration::from_millis(200), Duration::from_secs(3))
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    session.control().steer("keep".into()).await.unwrap();
    let error = loop {
        if let Some(Err(error)) = next(&mut session).await {
            break error;
        }
    };
    assert!(matches!(
        error,
        AiError::Transport(TransportError { timeout: true, .. })
    ));
    assert!(error.to_string().contains("heartbeat"));
    assert_eq!(
        session.steering_updates()[0].state,
        SteeringState::Ambiguous
    );
    release.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn steering_admission_bounds_are_local_and_do_not_send_before_created() {
    let (listener, model) = listener().await;
    let (release, hold) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        hold.await.unwrap();
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    let control = session.control();
    assert!(control.steer(String::new()).await.is_err());
    assert!(control.steer("x".repeat(65537)).await.is_err());
    for id in 0..64 {
        assert_eq!(control.steer(format!("input {id}")).await.unwrap(), id);
    }
    assert!(control.steer("overflow".into()).await.is_err());
    assert_eq!(session.cancel().len(), 64);
    release.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn steering_prepare_receipt_is_a_real_pre_dispatch_persistence_boundary() {
    let (listener, model) = listener().await;
    let (reserved, reservation_ready) = oneshot::channel();
    let (checked, no_dispatch) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        reservation_ready.await.unwrap();
        // Even terminal processing must not dispatch a prepared reservation.
        text(&mut socket, "original").await;
        done(&mut socket, "r1", false, json!([])).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), socket.next())
                .await
                .is_err()
        );
        checked.send(()).unwrap();
        assert_eq!(
            recv(&mut socket).await["input"],
            "persist me before dispatch"
        );
        accepted(&mut socket, "s1", "r1").await;
        created(&mut socket, "r2").await;
        done(&mut socket, "r2", false, json!([])).await;
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    let control = session.control();
    let receipt = control
        .prepare_steer("persist me before dispatch".into())
        .unwrap();
    assert_eq!(receipt.local_id(), 0);
    reserved.send(()).unwrap();
    no_dispatch.await.unwrap();
    // A durable host write belongs here, before commit.
    assert_eq!(control.commit_steer(receipt).await.unwrap(), 0);
    while let Some(event) = next(&mut session).await {
        event.unwrap();
    }
    server.await.unwrap();
}

#[tokio::test]
async fn steering_dropped_reservation_never_dispatches() {
    let (listener, model) = listener().await;
    let (finish, dropped) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        recv(&mut socket).await;
        created(&mut socket, "r1").await;
        dropped.await.unwrap();
        done(&mut socket, "r1", false, json!([])).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), socket.next())
                .await
                .is_err()
        );
    });
    let mut session = client()
        .steerable_responses(&model, request())
        .await
        .unwrap();
    until_started(&mut session).await;
    drop(
        session
            .control()
            .prepare_steer("never submit".into())
            .unwrap(),
    );
    finish.send(()).unwrap();
    while let Some(event) = next(&mut session).await {
        event.unwrap();
    }
    assert_eq!(
        session.steering_updates()[0].state,
        SteeringState::Failed {
            code: Some("not_submitted".into())
        }
    );
    server.await.unwrap();
}

#[tokio::test]
async fn raw_compact_update_requires_own_authority_before_any_http_dispatch() {
    let (listener, model) = listener().await;
    let client = client().track_request_dispatch();
    let request = ResponsesCompactRequest {
        model: model.spec.api_name.clone(),
        input: ResponsesInput::new(vec![ResponsesConfigurationUpdate {
            reasoning: ReasoningConfig::Off,
        }
        .to_item()]),
        instructions: None,
        tools: None,
        parallel_tool_calls: None,
        reasoning: None,
        text: None,
        prompt_cache_key: None,
        session_id: None,
    };
    assert!(matches!(
        client.open_compact_responses(&model, request).await,
        Err(AiError::Config(_))
    ));
    assert!(!client.request_may_have_been_sent());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}
