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

fn compact_fixture() -> (
    ProtocolReadState,
    mpsc::Receiver<WriterFrame>,
    ExtensionSessionLifecycleReceiver,
    ExtensionResourceOwner,
) {
    let (events, _) = broadcast::channel(8);
    let (mut state, frames) =
        super::super::tests::protocol_read_state_for_test(ManifestContributions::default(), events);
    let owner = super::super::tests::test_resource_owner("foreground");
    lock_std_mutex(&state.issued_resource_owners).insert(owner.clone());
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = EXTENSION_API_VERSION_0_4.into();
        protocol.features.extend([
            EXTENSION_FEATURE_SESSION_CONTROL_V1.into(),
            EXTENSION_FEATURE_SESSION_COMPACTION_V1.into(),
        ]);
    }
    let (service, receiver) = ExtensionSessionLifecycleService::channel(1).unwrap();
    let service = service.with_compaction();
    service.activate();
    state.session_lifecycle = Some(service);
    (state, frames, receiver, owner)
}

fn compact_call(state: &ProtocolReadState, id: u64, params: serde_json::Value) {
    handle_protocol_line(
        &super::super::tests::wave1_line(id, "session/compact", params),
        state,
    )
    .unwrap();
}

fn compact_params(owner: &ExtensionResourceOwner) -> serde_json::Value {
    serde_json::json!({"parent_request_id":1,"resource_owner":owner,"custom_instructions":"keep the goal\n\tand constraints"})
}

async fn compact_frame(frames: &mut mpsc::Receiver<WriterFrame>) -> serde_json::Value {
    let frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
        .await
        .unwrap()
        .unwrap();
    serde_json::from_slice(&frame.line).unwrap()
}

#[tokio::test]
async fn compaction_requires_consumer_features_bounds_and_an_authentic_retained_owner() {
    for case in 0..12 {
        let (mut state, mut frames, mut receiver, owner) = compact_fixture();
        let mut params = compact_params(&owner);
        match case {
            0 => {
                write_std_lock(&state.protocol)
                    .features
                    .remove(EXTENSION_FEATURE_SESSION_COMPACTION_V1);
            }
            1 => {
                write_std_lock(&state.protocol)
                    .features
                    .remove(EXTENSION_FEATURE_SESSION_CONTROL_V1);
            }
            2 => {
                write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_2.into();
            }
            3 => {
                state.session_lifecycle =
                    Some(ExtensionSessionLifecycleService::channel(1).unwrap().0);
            }
            4 => {
                params["custom_instructions"] = "x".repeat(16 * 1024 + 1).into();
            }
            5 => {
                params["custom_instructions"] = "invalid\rcontrol".into();
            }
            6 => {
                params["unknown"] = true.into();
            }
            7 => {
                params.as_object_mut().unwrap().remove("resource_owner");
            }
            8 => {
                params["resource_owner"]["session_id"] = "foreign".into();
            }
            9 => {
                params["resource_owner"]["process_generation"] = 2.into();
            }
            10 => {
                state.session_lifecycle.as_ref().unwrap().deactivate();
            }
            11 => {
                lock_std_mutex(&state.tombstones).insert(1, Duration::from_secs(30));
            }
            _ => unreachable!(),
        }
        compact_call(&state, 10, params);
        assert!(
            compact_frame(&mut frames).await.get("error").is_some(),
            "case {case}"
        );
        assert!(
            receiver.try_next().is_none(),
            "refused before dispatch: {case}"
        );
    }
}

#[tokio::test]
async fn compaction_live_parent_is_refused_then_retained_call_waits_for_real_outcome() {
    let (state, mut frames, mut receiver, owner) = compact_fixture();
    super::super::tests::insert_test_parent(&state, 1, Some(owner.clone()));
    compact_call(&state, 10, compact_params(&owner));
    let refused = compact_frame(&mut frames).await;
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("settled parent"));
    assert!(
        receiver.try_next().is_none(),
        "recursive awaited hook cannot queue"
    );
    handle_protocol_line(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#, &state).unwrap();
    compact_call(&state, 11, compact_params(&owner));
    assert!(frames.try_recv().is_err(), "queue admission is not success");
    assert!(
        receiver.try_next_idle_wait().is_none(),
        "command idle-barrier pump must not run compaction"
    );
    let request = receiver.try_next().unwrap();
    assert_eq!(request.resource_owner(), Some(&owner));
    assert!(
        matches!(request.operation(), ExtensionSessionLifecycleOperation::Compact { instructions: Some(text) } if text.contains("goal"))
    );
    compact_call(&state, 12, compact_params(&owner));
    assert!(
        compact_frame(&mut frames).await.get("error").is_some(),
        "in-flight request retains lifecycle capacity"
    );
    request.respond_compaction(Ok(ExtensionSessionCompactionResult {
        entry_id: "actual-compaction".into(),
        summary: "actual stored summary".into(),
        first_kept: "kept-user".into(),
    }));
    let response = compact_frame(&mut frames).await;
    assert_eq!(response["id"], 11);
    assert_eq!(
        response["result"],
        serde_json::json!({"entry_id":"actual-compaction","summary":"actual stored summary","first_kept":"kept-user"})
    );
    assert!(lock_std_mutex(&state.child_requests).is_empty());
}

#[tokio::test]
async fn compaction_cancellation_reaches_token_and_retains_accounting_worker_until_settlement() {
    let (state, mut frames, mut receiver, owner) = compact_fixture();
    compact_call(&state, 10, compact_params(&owner));
    let request = receiver.try_next().unwrap();
    let token = request.cancellation_token();
    handle_protocol_line(
        br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":10}}"#,
        &state,
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), token.cancelled())
        .await
        .unwrap();
    assert!(request.is_cancelled());
    assert_eq!(
        state.child_work_slots.available_permits(),
        MAX_CHILD_WORKERS - 1
    );
    request.respond_compaction(Err("cancelled after accounting settlement".into()));
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        state.child_work_slots.available_permits(),
        MAX_CHILD_WORKERS
    );
    assert!(
        frames.try_recv().is_err(),
        "cancelled request cannot receive late success"
    );
}

#[tokio::test]
async fn compaction_owner_epoch_and_process_retirement_cancel_even_executing_work() {
    for fence in 0..4 {
        let (state, mut frames, mut receiver, owner) = compact_fixture();
        compact_call(&state, 10, compact_params(&owner));
        let request = receiver.try_next().unwrap();
        let token = request.cancellation_token();
        match fence {
            0 => {
                lock_std_mutex(&state.issued_resource_owners).clear();
                state.pending_changed.notify_waiters();
            }
            1 => {
                let service = state.session_lifecycle.as_ref().unwrap();
                service.deactivate();
                service.activate();
            }
            2 => state.closed.store(true, Ordering::Release),
            3 => state.draining.store(true, Ordering::Release),
            _ => unreachable!(),
        }
        assert!(request.is_cancelled(), "dispatch fence is synchronous");
        tokio::time::timeout(Duration::from_secs(1), token.cancelled())
            .await
            .unwrap();
        assert_eq!(
            state.child_work_slots.available_permits(),
            MAX_CHILD_WORKERS - 1
        );
        request.respond_compaction(Err("operation settled after revocation".into()));
        assert!(compact_frame(&mut frames).await.get("error").is_some());
    }
}

#[tokio::test]
async fn compaction_post_commit_failure_is_not_a_success_or_automatic_retry() {
    let (state, mut frames, mut receiver, owner) = compact_fixture();
    compact_call(&state, 10, compact_params(&owner));
    receiver.try_next().unwrap().respond_compaction(Err(
        "compaction committed entry checkpoint-123; after-hook failed; do not retry".into(),
    ));
    let response = compact_frame(&mut frames).await;
    assert!(response.get("result").is_none());
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("checkpoint-123"));
    assert!(receiver.try_next().is_none());
}

#[test]
fn compaction_negotiation_requires_opt_in_consumer_and_session_control() {
    for (version, offered, control, accepted) in [
        ("0.4", true, true, true),
        ("0.4", false, true, false),
        ("0.4", true, false, false),
        ("0.2", true, true, false),
    ] {
        let manifest = ExtensionManifest::parse(&format!("name = \"compact-test\"\nversion = \"0.1.0\"\napi_version = \"{version}\"\n[entrypoint]\ncommand = \"test\"\n")).unwrap();
        let mut features = API_0_2_REQUIRED_FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect::<Vec<_>>();
        features.push(EXTENSION_FEATURE_SESSION_COMPACTION_V1.into());
        if control {
            features.push(EXTENSION_FEATURE_SESSION_CONTROL_V1.into());
        }
        let response = serde_json::from_value(serde_json::json!({
            "api_version":version,"tools":[],"commands":[],
            "protocol":{"version":version,"features":features,"limits":{"max_concurrent_requests":4}}
        })).unwrap();
        let result = negotiate_contributions_with_host_services(
            &manifest,
            response,
            4,
            OfferedHostServices {
                session_lifecycle: true,
                session_compaction: offered,
                ..OfferedHostServices::default()
            },
        );
        assert_eq!(
            result.is_ok(),
            accepted,
            "{version}: consumer={offered}, control={control}"
        );
    }
}

#[tokio::test]
async fn compaction_queued_revocation_never_reaches_idle_consumer() {
    let (state, mut frames, mut receiver, owner) = compact_fixture();
    compact_call(&state, 10, compact_params(&owner));
    lock_std_mutex(&state.issued_resource_owners).clear();
    state.pending_changed.notify_waiters();
    assert!(receiver.try_next().is_none());
    assert!(compact_frame(&mut frames).await.get("error").is_some());
    assert!(lock_std_mutex(&state.child_requests).is_empty());
}
