//! Replacement authority is native even when session_entries is not negotiated.
use super::super::tests::{
    insert_test_parent, protocol_read_state_for_test, test_resource_owner, wave1_line,
};
use super::*;
use serde_json::{json, Value};

fn fixture() -> (
    ProtocolReadState,
    mpsc::Receiver<WriterFrame>,
    ExtensionSessionLifecycleReceiver,
    ExtensionResourceOwner,
) {
    let (events, _) = broadcast::channel(8);
    let (mut state, frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = EXTENSION_API_VERSION_0_4.into();
        protocol
            .features
            .insert(EXTENSION_FEATURE_SESSION_CONTROL_V1.into());
    }
    let original = test_resource_owner("original-native-owner");
    insert_test_parent(&state, 1, Some(original.clone()));
    lock_std_mutex(&state.pending).get_mut(&1).unwrap().method = methods::COMMAND_EXECUTE.into();
    let (service, receiver) = ExtensionSessionLifecycleService::channel(2).unwrap();
    service.activate();
    state.session_lifecycle = Some(service);
    (state, frames, receiver, original)
}

fn call(
    state: &ProtocolReadState,
    id: u64,
    method: &str,
    parent: u64,
    owner: &ExtensionResourceOwner,
) {
    handle_protocol_line(
        &wave1_line(
            id,
            method,
            json!({"parent_request_id":parent,"resource_owner":owner}),
        ),
        state,
    )
    .unwrap();
}

fn publish_owner(state: &ProtocolReadState, owner: ExtensionResourceOwner) {
    // Same owner-only native publication as set_host_state_with_session when
    // session_entries is absent. This is not a wire-supplied grant.
    *lock_std_mutex(&state.session_leaf.mirror) = Some(SessionMirror { owner, value: None });
    lock_std_mutex(&state.issued_resource_owners).clear();
}

async fn reply(frames: &mut mpsc::Receiver<WriterFrame>) -> Value {
    let mut frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
        .await
        .unwrap()
        .unwrap();
    let value = serde_json::from_slice(&frame.line).unwrap();
    if let Some(completion) = frame.completion.take() {
        let _ = completion.send(Ok(()));
    }
    value
}

#[tokio::test]
async fn replacement_without_session_entries_rebinds_only_the_live_parent() {
    let (state, mut frames, mut receiver, original) = fixture();
    assert!(!read_std_lock(&state.protocol).supports(EXTENSION_FEATURE_SESSION_ENTRIES));
    call(&state, 10, "session/create", 1, &original);
    let request = receiver.try_next().unwrap();
    assert!(
        frames.try_recv().is_err(),
        "no receipt before native replacement"
    );
    let replacement = test_resource_owner("replacement-native-owner");
    publish_owner(&state, replacement.clone());
    request.respond_replacement(Ok("replacement-display-id".into()));
    assert_eq!(
        reply(&mut frames).await,
        json!({
            "jsonrpc":"2.0", "id":10, "result":{"session_id":"replacement-display-id"}
        })
    );
    assert_eq!(
        lock_std_mutex(&state.pending)[&1].resource_owner.as_ref(),
        Some(&replacement)
    );
    assert!(!lock_std_mutex(&state.issued_resource_owners).contains(&original));
    assert!(lock_std_mutex(&state.issued_resource_owners).contains(&replacement));

    // A retained old context has no live parent to rebind; it remains refused.
    call(&state, 11, "session/wait_for_idle", 99, &original);
    assert_eq!(reply(&mut frames).await["error"]["code"], -32002);
    assert!(receiver.try_next().is_none());
    // The same admitted live command can continue, deriving the new authority
    // from its parent rather than trusting its old explicit JSON owner.
    call(&state, 12, "session/wait_for_idle", 1, &original);
    receiver
        .try_next()
        .unwrap()
        .respond(Ok("replacement-display-id".into()));
    assert_eq!(
        reply(&mut frames).await["result"]["session_id"],
        "replacement-display-id"
    );
    assert!(lock_std_mutex(&state.child_requests).is_empty());
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn replacement_owner_failures_send_one_typed_error_without_renewing_authority() {
    for failure in [
        "missing",
        "instance",
        "generation",
        "parent_owner",
        "parent_settled",
    ] {
        let (state, mut frames, mut receiver, original) = fixture();
        call(&state, 10, "session/create", 1, &original);
        let request = receiver.try_next().unwrap();
        let replacement = test_resource_owner("replacement-native-owner");
        let mut published = replacement.clone();
        match failure {
            "instance" => published.extension_instance_id = "other-instance".into(),
            "generation" => published.process_generation += 1,
            _ => {}
        }
        publish_owner(&state, published);
        match failure {
            "missing" => *lock_std_mutex(&state.session_leaf.mirror) = None,
            "parent_owner" => {
                lock_std_mutex(&state.pending)
                    .get_mut(&1)
                    .unwrap()
                    .resource_owner = Some(test_resource_owner("unrelated-owner"))
            }
            "parent_settled" => {
                lock_std_mutex(&state.pending).remove(&1);
            }
            _ => {}
        }
        request.respond_replacement(Ok("replacement-display-id".into()));
        let response = reply(&mut frames).await;
        assert_eq!(response["id"], 10, "{failure}");
        assert_eq!(response["error"]["code"], -32002, "{failure}");
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("not_foreground_owner:"));
        assert!(response.get("result").is_none(), "{failure}");
        assert!(
            lock_std_mutex(&state.issued_resource_owners).is_empty(),
            "{failure}"
        );
        assert!(
            lock_std_mutex(&state.child_requests).is_empty(),
            "{failure}"
        );
        assert!(frames.try_recv().is_err(), "one terminal reply: {failure}");
    }
}

#[tokio::test]
async fn cancelled_replacement_child_does_not_rebind_or_reply_after_cancellation() {
    let (state, mut frames, mut receiver, original) = fixture();
    call(&state, 10, "session/create", 1, &original);
    let request = receiver.try_next().unwrap();
    handle_protocol_line(
        br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":10}}"#,
        &state,
    )
    .unwrap();
    // Ordinary $/cancelRequest settles this child without a response envelope.
    assert!(frames.try_recv().is_err());
    publish_owner(&state, test_resource_owner("replacement-native-owner"));
    request.respond_replacement(Ok("replacement-display-id".into()));
    tokio::task::yield_now().await;
    assert_eq!(
        lock_std_mutex(&state.pending)[&1].resource_owner.as_ref(),
        Some(&original)
    );
    assert!(lock_std_mutex(&state.issued_resource_owners).is_empty());
    assert!(lock_std_mutex(&state.child_requests).is_empty());
    assert!(frames.try_recv().is_err());
}
