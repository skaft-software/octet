use super::*;

#[tokio::test]
async fn idle_barrier_preserves_order_capacity_and_epoch() {
    let (service, mut receiver) = ExtensionSessionLifecycleService::channel(2).unwrap();
    service.activate();
    let mutation = service
        .try_submit(ExtensionSessionLifecycleOperation::Create)
        .unwrap();
    let idle = service
        .try_submit(ExtensionSessionLifecycleOperation::WaitForIdle)
        .unwrap();
    assert!(receiver.try_next_idle_wait().is_none());
    assert!(matches!(
        service.try_submit(ExtensionSessionLifecycleOperation::Reload),
        Err(SessionLifecycleSubmitError::Full)
    ));
    receiver.try_next().unwrap().respond(Ok("created".into()));
    assert_eq!(mutation.await.unwrap(), Ok("created".into()));
    let request = receiver.try_next_idle_wait().unwrap();
    assert_eq!(
        request.operation(),
        &ExtensionSessionLifecycleOperation::WaitForIdle
    );
    request.respond(Ok("foreground".into()));
    assert_eq!(idle.await.unwrap(), Ok("foreground".into()));
    let stale = service
        .try_submit(ExtensionSessionLifecycleOperation::WaitForIdle)
        .unwrap();
    service.deactivate();
    service.activate();
    assert!(receiver.try_next_idle_wait().is_none());
    assert_eq!(
        stale.await.unwrap(),
        Err(ExtensionSessionLifecycleError::Unavailable)
    );
}

#[tokio::test]
async fn owner_scoped_idle_wire_requires_negotiation_and_waits_for_consumer() {
    let (events, _) = broadcast::channel(8);
    let (mut state, mut frames) =
        super::super::tests::protocol_read_state_for_test(ManifestContributions::default(), events);
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = EXTENSION_API_VERSION_0_4.into();
    }
    let owner = ExtensionResourceOwner {
        session_id: "foreground".into(),
        extension_instance_id: state.instance_id.clone(),
        process_generation: state.generation,
    };
    lock_std_mutex(&state.issued_resource_owners).insert(owner.clone());
    let (service, mut receiver) = ExtensionSessionLifecycleService::channel(2).unwrap();
    service.activate();
    state.session_lifecycle = Some(service);
    let call = |id| {
        serde_json::json!({"jsonrpc":"2.0","id":id,"method":"session/wait_for_idle","params":{"parent_request_id":1,"resource_owner":owner}}).to_string()
    };
    handle_protocol_line(call(1).as_bytes(), &state).unwrap();
    let refused: serde_json::Value =
        serde_json::from_slice(&frames.recv().await.unwrap().line).unwrap();
    assert!(refused.get("error").is_some());
    assert!(receiver.try_next().is_none());
    write_std_lock(&state.protocol)
        .features
        .insert(EXTENSION_FEATURE_SESSION_CONTROL_V1.into());
    handle_protocol_line(call(2).as_bytes(), &state).unwrap();
    assert!(
        frames.try_recv().is_err(),
        "no idle ACK before the actual consumer"
    );
    let request = receiver.try_next_idle_wait().unwrap();
    request.respond(Ok("foreground".into()));
    let mut frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
        .await
        .unwrap()
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&frame.line).unwrap();
    assert_eq!(response["id"], 2);
    assert_eq!(
        response["result"],
        serde_json::json!({"session_id":"foreground"})
    );
    if let Some(completion) = frame.completion.take() {
        let _ = completion.send(Ok(()));
    }
}
