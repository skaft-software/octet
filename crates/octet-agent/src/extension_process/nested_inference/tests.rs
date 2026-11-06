use super::*;
use crate::extension_process::tests::{
    insert_test_parent, protocol_read_state_for_test, test_resource_owner,
};
use crate::tool_inference::ToolInferenceService;
use std::sync::atomic::AtomicUsize;

fn params() -> serde_json::Value {
    serde_json::json!({"parent_request_id":7,"inference_id":"request-one","model":{"provider":"fixture","id":"configured"},
        "request":{"messages":[{"User":{"content":[{"Text":"proposal"}]}}],"reasoning":"low","max_output_tokens":32}})
}

fn state() -> (ProtocolReadState, mpsc::Receiver<WriterFrame>) {
    let (events, _) = broadcast::channel(4);
    let (state, frames) = protocol_read_state_for_test(ManifestContributions::default(), events);
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = EXTENSION_API_VERSION_0_4.into();
        protocol.features.insert(FEATURE.into());
    }
    (state, frames)
}

#[test]
fn inference_wire_cannot_supply_transport_credentials_or_tool_authority() {
    let parsed: InferenceParams = serde_json::from_value(params()).unwrap();
    let request = inference_request(parsed).unwrap();
    assert_eq!(request.provider, "fixture");
    assert_eq!(request.model, "configured");
    assert!(request.request.tools.is_empty());
    assert!(matches!(
        request.request.tool_choice,
        octet_ai::ToolChoice::None
    ));
    assert!(request.request.responses.is_none());
    assert!(request.request.session_id.is_none());
    for key in [
        "url",
        "base_url",
        "api_key",
        "headers",
        "fetch",
        "tools",
        "session_id",
        "responses",
    ] {
        let mut supplied = params();
        supplied["request"][key] = serde_json::json!("forbidden");
        assert!(
            serde_json::from_value::<InferenceParams>(supplied).is_err(),
            "{key}"
        );
    }
    for key in ["endpoint", "baseUrl", "auth", "headers"] {
        let mut supplied = params();
        supplied["model"][key] = serde_json::json!("forbidden");
        assert!(
            serde_json::from_value::<InferenceParams>(supplied).is_err(),
            "{key}"
        );
    }
}

#[test]
fn inference_requires_bounded_identifiers_and_supported_portable_options() {
    for path in ["inference_id", "model.provider", "model.id"] {
        for value in ["", "https://untrusted.invalid?key=hidden", &"x".repeat(257)] {
            let mut supplied = params();
            match path {
                "inference_id" => supplied["inference_id"] = serde_json::json!(value),
                "model.provider" => supplied["model"]["provider"] = serde_json::json!(value),
                _ => supplied["model"]["id"] = serde_json::json!(value),
            }
            assert!(inference_request(serde_json::from_value(supplied).unwrap()).is_err());
        }
    }
    for effort in ["ultra", "budget=1024", "on", "invalid"] {
        let mut supplied = params();
        supplied["request"]["reasoning"] = serde_json::json!(effort);
        assert!(inference_request(serde_json::from_value(supplied).unwrap()).is_err());
    }
}

#[test]
fn inference_event_transport_preserves_fifo_and_refuses_overflow() {
    let (writer, mut frames) = mpsc::channel(2);
    let mut sequence = 0;
    let mut bytes = 0;
    for delta in ["first", "second"] {
        queue_event(
            &writer,
            DEFAULT_EXTENSION_MESSAGE_BYTES,
            "request-one",
            &StreamEvent::TextDelta {
                index: 0,
                delta: delta.into(),
            },
            &mut sequence,
            &mut bytes,
        )
        .unwrap();
    }
    assert!(queue_event(
        &writer,
        DEFAULT_EXTENSION_MESSAGE_BYTES,
        "request-one",
        &StreamEvent::TextEnd { index: 0 },
        &mut sequence,
        &mut bytes
    )
    .is_err());
    let first: serde_json::Value =
        serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap();
    let second: serde_json::Value =
        serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap();
    assert_eq!(first["params"]["sequence"], 0);
    assert_eq!(first["params"]["payload"]["delta"], "first");
    assert_eq!(second["params"]["sequence"], 1);
    assert_eq!(second["params"]["payload"]["delta"], "second");
    sequence = 4096;
    assert!(queue_event(
        &writer,
        DEFAULT_EXTENSION_MESSAGE_BYTES,
        "request-one",
        &StreamEvent::Started { response_id: None },
        &mut sequence,
        &mut bytes
    )
    .is_err());
    sequence = 0;
    bytes = MAX_OUTPUT_BYTES;
    assert!(queue_event(
        &writer,
        DEFAULT_EXTENSION_MESSAGE_BYTES,
        "request-one",
        &StreamEvent::Started { response_id: None },
        &mut sequence,
        &mut bytes
    )
    .is_err());
}

#[test]
fn inference_refuses_unnegotiated_or_non_tool_parents_without_fatal_protocol_failure() {
    for (feature, tool_policy, service, valid_owner) in [
        (false, true, true, true),
        (true, false, true, true),
        (true, true, false, true),
        (true, true, true, false),
    ] {
        let (state, mut frames) = state();
        if !feature {
            write_std_lock(&state.protocol).features.remove(FEATURE);
        }
        insert_test_parent(&state, 7, Some(test_resource_owner("fixture-session")));
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let mut pending = lock_std_mutex(&state.pending);
            let parent = pending.get_mut(&7).unwrap();
            if tool_policy {
                parent.tool_call_policy_digest = Some([1; 32]);
            }
            if service {
                parent.child_interaction_progress =
                    Some(ToolProgressSink::null().with_inference(Arc::new(Script {
                        calls: calls.clone(),
                        wait: false,
                    })));
            }
            if !valid_owner {
                parent.resource_owner.as_mut().unwrap().process_generation += 1;
            }
        }
        let object = serde_json::json!({"jsonrpc":"2.0","id":"pi:1","method":"provider/infer","params":params()});
        dispatch(&state, object.as_object().unwrap(), params()).unwrap();
        let reply: serde_json::Value =
            serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap();
        assert!(reply.get("error").is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

struct Script {
    calls: Arc<AtomicUsize>,
    wait: bool,
}

#[async_trait::async_trait]
impl ToolInferenceService for Script {
    async fn stream(
        &self,
        _: ToolInferenceRequest,
        cancellation: CancellationToken,
        updates: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<octet_ai::Response, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let sender = updates.unwrap();
        sender
            .send(StreamEvent::Started { response_id: None })
            .await
            .unwrap();
        if self.wait {
            cancellation.cancelled().await;
            return Err(ToolError::new("private-provider-error-not-for-wire"));
        }
        sender
            .send(StreamEvent::TextStart { index: 0 })
            .await
            .unwrap();
        sender
            .send(StreamEvent::TextDelta {
                index: 0,
                delta: "advice".into(),
            })
            .await
            .unwrap();
        sender
            .send(StreamEvent::TextEnd { index: 0 })
            .await
            .unwrap();
        Err(ToolError::new("private-provider-error-not-for-wire"))
    }
}

#[tokio::test]
async fn inference_callbacks_are_live_parent_bound_and_do_not_disclose_provider_errors() {
    let (state, mut frames) = state();
    insert_test_parent(&state, 7, Some(test_resource_owner("fixture-session")));
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let mut pending = lock_std_mutex(&state.pending);
        let parent = pending.get_mut(&7).unwrap();
        parent.tool_call_policy_digest = Some([1; 32]);
        parent.child_interaction_progress =
            Some(ToolProgressSink::null().with_inference(Arc::new(Script {
                calls: calls.clone(),
                wait: false,
            })));
    }
    let object = serde_json::json!({"jsonrpc":"2.0","id":"pi:1","method":"provider/infer","params":params()});
    dispatch(&state, object.as_object().unwrap(), params()).unwrap();
    let mut deltas = Vec::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), frames.recv())
            .await
            .unwrap()
            .unwrap();
        let reply: serde_json::Value = serde_json::from_slice(&frame.line).unwrap();
        assert!(!String::from_utf8_lossy(&frame.line).contains("private-provider-error"));
        if reply.get("error").is_some() {
            assert_eq!(reply["id"], "pi:1");
            break;
        }
        deltas.push(reply);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(deltas.len(), 4);
    assert_eq!(deltas[0]["params"]["sequence"], 0);
    assert_eq!(deltas[2]["params"]["payload"]["delta"], "advice");
}

#[test]
fn inference_cancel_is_tagged_parent_owned_and_does_not_settle_the_response() {
    let (state, _) = state();
    insert_test_parent(&state, 7, Some(test_resource_owner("fixture-session")));
    lock_std_mutex(&state.pending)
        .get_mut(&7)
        .unwrap()
        .tool_call_policy_digest = Some([1; 32]);
    let id = ExtensionRequestId::String("pi:1".into());
    let child = insert_child_request(&state, id.clone(), Some(7), None).unwrap();
    let token = CancellationToken::default();
    *lock_std_mutex(&child.response_state.composition_cancellation) = Some(token.clone());
    let notification = serde_json::json!({"jsonrpc":"2.0","method":"provider/infer/cancel"});
    cancel(
        &state,
        notification.as_object().unwrap(),
        serde_json::json!({"id":"pi:1"}),
    )
    .unwrap();
    assert!(!token.is_cancelled());
    *lock_std_mutex(&child.response_state.inference_id) = Some("request-one".into());
    lock_std_mutex(&state.pending)
        .get_mut(&7)
        .unwrap()
        .resource_owner
        .as_mut()
        .unwrap()
        .process_generation = 2;
    cancel(
        &state,
        notification.as_object().unwrap(),
        serde_json::json!({"id":"pi:1"}),
    )
    .unwrap();
    assert!(!token.is_cancelled());
    lock_std_mutex(&state.pending)
        .get_mut(&7)
        .unwrap()
        .resource_owner
        .as_mut()
        .unwrap()
        .process_generation = 1;
    cancel(
        &state,
        notification.as_object().unwrap(),
        serde_json::json!({"id":"pi:1"}),
    )
    .unwrap();
    assert!(token.is_cancelled());
    assert_eq!(
        child.response_state.state.load(Ordering::Acquire),
        CHILD_ACTIVE
    );
    assert!(lock_std_mutex(&state.child_requests).contains_key(&id));
}

struct Successful;

#[async_trait::async_trait]
impl ToolInferenceService for Successful {
    async fn stream(
        &self,
        _: ToolInferenceRequest,
        _: CancellationToken,
        updates: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<octet_ai::Response, ToolError> {
        let updates = updates.unwrap();
        for event in [
            StreamEvent::Started { response_id: None },
            StreamEvent::TextStart { index: 0 },
            StreamEvent::TextDelta {
                index: 0,
                delta: "advice".into(),
            },
            StreamEvent::TextEnd { index: 0 },
        ] {
            updates.send(event).await.unwrap();
        }
        Ok(octet_ai::Response {
            message: octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("advice".into())],
                model: octet_ai::ModelId("configured".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            },
            stop_reason: octet_ai::StopReason::EndTurn,
            usage: octet_ai::Usage {
                input_tokens: 2,
                output_tokens: 3,
                total_tokens: 5,
                ..Default::default()
            },
            cost: Some(octet_ai::Cost {
                total: 7,
                total_picodollars_remainder: 123,
                ..Default::default()
            }),
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: None,
            diagnostics: vec![],
        })
    }
}

#[tokio::test]
async fn inference_drains_native_deltas_before_authoritative_terminal_usage() {
    let (state, mut frames) = state();
    insert_test_parent(&state, 7, Some(test_resource_owner("fixture-session")));
    {
        let mut pending = lock_std_mutex(&state.pending);
        let parent = pending.get_mut(&7).unwrap();
        parent.tool_call_policy_digest = Some([1; 32]);
        parent.child_interaction_progress =
            Some(ToolProgressSink::null().with_inference(Arc::new(Successful)));
    }
    let object = serde_json::json!({"jsonrpc":"2.0","id":"pi:1","method":"provider/infer","params":params()});
    dispatch(&state, object.as_object().unwrap(), params()).unwrap();
    for sequence in 0..4 {
        let frame = tokio::time::timeout(Duration::from_secs(2), frames.recv())
            .await
            .unwrap()
            .unwrap();
        let event: serde_json::Value = serde_json::from_slice(&frame.line).unwrap();
        assert_eq!(event["method"], "provider/infer_event");
        assert_eq!(event["params"]["sequence"], sequence);
    }
    let frame = tokio::time::timeout(Duration::from_secs(2), frames.recv())
        .await
        .unwrap()
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&frame.line).unwrap();
    assert_eq!(result["result"]["message"]["content"][0]["Text"], "advice");
    assert_eq!(result["result"]["usage"]["total_tokens"], 5);
    assert_eq!(result["result"]["cost"]["total"], 7);
    assert_eq!(result["result"]["cost"]["total_picodollars_remainder"], 123);
    assert_eq!(result["result"]["stop_reason"], "end_turn");
}
