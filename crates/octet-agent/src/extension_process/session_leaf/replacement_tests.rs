//! Replacement authority is native even when session_entries is not negotiated.
use super::super::tests::{
    insert_test_parent, protocol_read_state_for_test, test_resource_owner, wave1_line,
};
use super::*;
use crate::session::{EntryValue, Session};
use serde_json::{json, Value};

fn stage_counted(
    state: &ProtocolReadState,
    session: &Session,
    owner: &ExtensionResourceOwner,
) -> (transport::Descriptor, u64) {
    let publication = state.session_leaf.begin_snapshot_publication(owner.clone());
    let profile = state
        .session_leaf
        .profile(state.max_message_bytes())
        .unwrap();
    let mut store = lock_std_mutex(&state.session_leaf.transport);
    let revision = store.next_revision().unwrap();
    state
        .session_leaf
        .set_snapshot_publication_revision(owner, publication, revision);
    let view = store
        .history(
            session,
            &state.extension_identity.name,
            owner.clone(),
            &profile,
            None,
            revision,
        )
        .unwrap();
    let descriptor = view.descriptor.clone();
    let facts = transport::host_facts(&view).unwrap();
    store.current.insert(owner.clone(), Some(view));
    drop(store);
    *lock_std_mutex(&state.session_leaf.mirror) = Some(SessionMirror {
        owner: owner.clone(),
        value: Some(facts),
    });
    lock_std_mutex(&state.issued_resource_owners).clear();
    (descriptor, publication)
}

fn publish_counted(
    state: &ProtocolReadState,
    session: &Session,
    owner: &ExtensionResourceOwner,
) -> transport::Descriptor {
    let (descriptor, publication) = stage_counted(state, session, owner);
    state.session_leaf.finish_snapshot_publication(
        owner,
        publication,
        descriptor.view_revision,
        true,
    );
    descriptor
}

fn pin_counted(state: &ProtocolReadState, parent: u64, descriptor: &transport::Descriptor) {
    let profile = state
        .session_leaf
        .profile(state.max_message_bytes())
        .unwrap();
    let mut store = lock_std_mutex(&state.session_leaf.transport);
    let view = Arc::clone(store.current[&descriptor.owner].as_ref().unwrap());
    store.transfer(parent, view, &profile).unwrap();
    store.parents.insert(
        parent,
        transport::ParentView {
            owner: descriptor.owner.clone(),
            preparation: None,
            revision: descriptor.view_revision,
            head: descriptor.head.clone(),
        },
    );
}

fn counted_original(
    state: &ProtocolReadState,
    original: &mut ExtensionResourceOwner,
    session: &Session,
) -> transport::Descriptor {
    let mut protocol = write_std_lock(&state.protocol);
    protocol.features.extend(
        [
            "session_entries",
            "session_owner_routes_v1",
            "session_snapshot_transport_v1",
            "request_cancellation",
        ]
        .map(str::to_owned),
    );
    drop(protocol);
    assert!(owner_routes_enabled(&read_std_lock(&state.protocol)));
    original.session_id = session.resource_owner_key();
    lock_std_mutex(&state.pending)
        .get_mut(&1)
        .unwrap()
        .resource_owner = Some(original.clone());
    let descriptor = publish_counted(state, session, original);
    pin_counted(state, 1, &descriptor);
    lock_std_mutex(&state.issued_resource_owners).insert(original.clone());
    descriptor
}

fn durable_session(path: impl AsRef<std::path::Path>, entries: usize) -> Session {
    let mut session = Session::create(path.as_ref()).unwrap();
    for _ in 0..entries {
        session
            .append(EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            })
            .unwrap();
    }
    session
}

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

#[tokio::test]
async fn counted_replacement_rebinds_only_admitted_parent_to_exact_prepared_native_view() {
    let temp = tempfile::tempdir().unwrap();
    let old = durable_session(temp.path().join("old.jsonl"), 1);
    let new = durable_session(temp.path().join("new.jsonl"), 2);
    let (state, mut frames, mut receiver, mut original) = fixture();
    let previous = counted_original(&state, &mut original, &old);
    insert_test_parent(&state, 2, Some(original.clone()));
    pin_counted(&state, 2, &previous);
    call(&state, 10, "session/create", 1, &original);
    let request = receiver.try_next().unwrap();
    assert!(frames.try_recv().is_err());
    let replacement = ExtensionResourceOwner {
        session_id: new.resource_owner_key(),
        ..original.clone()
    };
    let next = publish_counted(&state, &new, &replacement);
    let retained = lock_std_mutex(&state.session_leaf.transport).retained();
    request.respond_replacement(Ok("replacement-display-id".into()));
    let mut response = reply(&mut frames).await;
    // The rebound parent is leased exactly its pinned replacement view.
    let lease = response["result"]
        .as_object_mut()
        .unwrap()
        .remove("session_snapshot")
        .expect("replacement parent lease");
    assert_eq!(
        response,
        json!({"jsonrpc":"2.0", "id":10, "result":{"session_id":"replacement-display-id"}})
    );
    let transfer_id = lease["transfer_id"].as_str().unwrap().to_owned();
    assert_ne!(transfer_id, next.transfer_id);
    let mut expected = serde_json::to_value(&next).unwrap();
    expected["transfer_id"] = json!(transfer_id);
    assert_eq!(lease, expected);
    assert!(lease["preparation"].is_null());
    {
        let store = lock_std_mutex(&state.session_leaf.transport);
        let pin = &store.parents[&1];
        assert_eq!(pin.owner, replacement);
        assert_eq!(pin.revision, next.view_revision);
        assert_eq!(pin.head, next.head);
        assert_eq!(pin.head, new.head_ref().map(|id| id.0.clone()));
        assert!(pin.preparation.is_none());
        assert!(pin.revision > previous.view_revision);
        assert_ne!(pin.head, previous.head);
        assert_eq!(store.parents[&2].owner, original);
        assert_eq!(store.parents[&2].revision, previous.view_revision);
        assert_eq!(store.parents[&2].head, previous.head);
        // The original parent-bound transfer plus its counted replacement lease.
        assert_eq!(store.in_flight(1), 2);
        assert_eq!(store.in_flight(2), 1);
        assert_eq!(store.retained(), retained);
    }
    assert_eq!(
        lock_std_mutex(&state.pending)[&1].resource_owner.as_ref(),
        Some(&replacement)
    );
    assert_eq!(
        lock_std_mutex(&state.pending)[&2].resource_owner.as_ref(),
        Some(&original)
    );
    assert!(lock_std_mutex(&state.issued_resource_owners).contains(&replacement));
    assert!(!lock_std_mutex(&state.issued_resource_owners).contains(&original));
    call(&state, 11, "session/wait_for_idle", 2, &original);
    assert_eq!(reply(&mut frames).await["error"]["code"], -32002);
    assert!(receiver.try_next().is_none());
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
async fn counted_replacement_refuses_unavailable_stale_prepared_or_foreign_views_atomically() {
    for failure in [
        "missing_pin",
        "foreign_pin",
        "prepared_pin",
        "missing_current",
        "failed_current",
        "foreign_descriptor",
        "prepared_current",
        "missing_mirror_facts",
        "stale_mirror_revision",
        "stale_mirror_head",
        "failed_publication",
        "stale_publication",
        "foreign_publication",
        "parent_terminal",
        "closed",
        "draining",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let old = durable_session(temp.path().join("old.jsonl"), 1);
        let new = durable_session(temp.path().join("new.jsonl"), 2);
        let (state, mut frames, mut receiver, mut original) = fixture();
        let previous = counted_original(&state, &mut original, &old);
        call(&state, 10, "session/create", 1, &original);
        let request = receiver.try_next().unwrap();
        let replacement = ExtensionResourceOwner {
            session_id: new.resource_owner_key(),
            ..original.clone()
        };
        let next = publish_counted(&state, &new, &replacement);
        {
            let mut store = lock_std_mutex(&state.session_leaf.transport);
            match failure {
                "missing_pin" => {
                    store.parents.remove(&1);
                }
                "foreign_pin" => {
                    store.parents.get_mut(&1).unwrap().owner.session_id = "unrelated-owner".into()
                }
                "prepared_pin" => {
                    store.parents.get_mut(&1).unwrap().preparation = Some(transport::Preparation {
                        activation_epoch: 7,
                        operation_id: "revoked-operation".into(),
                        tool_generation: 3,
                        head: previous.head.clone(),
                    })
                }
                "missing_current" => {
                    store.current.remove(&replacement);
                }
                "failed_current" => {
                    store.current.insert(replacement.clone(), None);
                }
                "foreign_descriptor" => {
                    Arc::get_mut(
                        store
                            .current
                            .get_mut(&replacement)
                            .unwrap()
                            .as_mut()
                            .unwrap(),
                    )
                    .unwrap()
                    .descriptor
                    .owner = original.clone();
                }
                "prepared_current" => {
                    Arc::get_mut(
                        store
                            .current
                            .get_mut(&replacement)
                            .unwrap()
                            .as_mut()
                            .unwrap(),
                    )
                    .unwrap()
                    .descriptor
                    .preparation = Some(transport::Preparation {
                        activation_epoch: 7,
                        operation_id: "revoked-operation".into(),
                        tool_generation: 3,
                        head: next.head.clone(),
                    });
                }
                _ => {}
            }
        }
        match failure {
            "missing_mirror_facts" => {
                lock_std_mutex(&state.session_leaf.mirror)
                    .as_mut()
                    .unwrap()
                    .value = None
            }
            "stale_mirror_revision" => {
                lock_std_mutex(&state.session_leaf.mirror)
                    .as_mut()
                    .unwrap()
                    .value
                    .as_mut()
                    .unwrap()["session_view_revision"] = json!(previous.view_revision)
            }
            "stale_mirror_head" => {
                lock_std_mutex(&state.session_leaf.mirror)
                    .as_mut()
                    .unwrap()
                    .value
                    .as_mut()
                    .unwrap()["session_leaf_id"] = json!(previous.head)
            }
            "failed_publication" => {
                let id = state
                    .session_leaf
                    .begin_snapshot_publication(replacement.clone());
                state
                    .session_leaf
                    .fail_snapshot_publication(&replacement, id);
            }
            "stale_publication" => {
                let id = state
                    .session_leaf
                    .begin_snapshot_publication(replacement.clone());
                state.session_leaf.set_snapshot_publication_revision(
                    &replacement,
                    id,
                    previous.view_revision,
                );
                state.session_leaf.finish_snapshot_publication(
                    &replacement,
                    id,
                    previous.view_revision,
                    true,
                );
            }
            "foreign_publication" => {
                let id = state
                    .session_leaf
                    .begin_snapshot_publication(original.clone());
                state.session_leaf.set_snapshot_publication_revision(
                    &original,
                    id,
                    previous.view_revision,
                );
                state.session_leaf.finish_snapshot_publication(
                    &original,
                    id,
                    previous.view_revision,
                    true,
                );
            }
            "parent_terminal" => lock_std_mutex(&state.pending)[&1]
                .terminal
                .store(REQUEST_CANCELLED, Ordering::Release),
            "closed" => state.closed.store(true, Ordering::Release),
            "draining" => state.draining.store(true, Ordering::Release),
            _ => {}
        }
        let before = {
            let store = lock_std_mutex(&state.session_leaf.transport);
            (
                store.parents.get(&1).map(|pin| {
                    (
                        pin.owner.clone(),
                        pin.preparation.clone(),
                        pin.revision,
                        pin.head.clone(),
                    )
                }),
                store.retained(),
            )
        };
        request.respond_replacement(Ok("replacement-display-id".into()));
        let response = reply(&mut frames).await;
        assert_eq!(response["id"], 10, "{failure}");
        assert_eq!(response["error"]["code"], -32002, "{failure}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("not_foreground_owner:"),
            "{failure}"
        );
        assert!(response.get("result").is_none(), "{failure}");
        assert_eq!(
            lock_std_mutex(&state.pending)[&1].resource_owner.as_ref(),
            Some(&original),
            "{failure}"
        );
        assert!(
            lock_std_mutex(&state.issued_resource_owners).is_empty(),
            "{failure}"
        );
        let store = lock_std_mutex(&state.session_leaf.transport);
        assert_eq!(
            (
                store.parents.get(&1).map(|pin| (
                    pin.owner.clone(),
                    pin.preparation.clone(),
                    pin.revision,
                    pin.head.clone()
                )),
                store.retained()
            ),
            before,
            "{failure}"
        );
        assert_ne!(
            store.parents.get(&1).map(|pin| pin.revision),
            Some(next.view_revision),
            "{failure}"
        );
        drop(store);
        assert!(
            lock_std_mutex(&state.child_requests).is_empty(),
            "{failure}"
        );
        assert!(frames.try_recv().is_err(), "{failure}");
    }
}

#[tokio::test]
async fn counted_replacement_waits_for_native_publication_and_preserves_cancel_and_failure() {
    for settlement in ["prepared", "failed", "cancelled"] {
        let temp = tempfile::tempdir().unwrap();
        let old = durable_session(temp.path().join("old.jsonl"), 1);
        let new = durable_session(temp.path().join("new.jsonl"), 2);
        let (state, mut frames, mut receiver, mut original) = fixture();
        let previous = counted_original(&state, &mut original, &old);
        call(&state, 10, "session/create", 1, &original);
        let request = receiver.try_next().unwrap();
        let replacement = ExtensionResourceOwner {
            session_id: new.resource_owner_key(),
            ..original.clone()
        };
        let (next, publication) = stage_counted(&state, &new, &replacement);
        request.respond_replacement(Ok("replacement-display-id".into()));
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if lock_std_mutex(&state.session_leaf.snapshot_publication)
                    .as_ref()
                    .unwrap()
                    .1
                    .receiver_count()
                    > 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(frames.try_recv().is_err(), "{settlement}");
        assert_eq!(
            lock_std_mutex(&state.pending)[&1].resource_owner.as_ref(),
            Some(&original),
            "{settlement}"
        );
        assert_eq!(
            lock_std_mutex(&state.session_leaf.transport).parents[&1].revision,
            previous.view_revision,
            "{settlement}"
        );
        assert!(
            lock_std_mutex(&state.issued_resource_owners).is_empty(),
            "{settlement}"
        );
        if settlement == "cancelled" {
            handle_protocol_line(
                br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":10}}"#,
                &state,
            )
            .unwrap();
        }
        state.session_leaf.finish_snapshot_publication(
            &replacement,
            publication,
            next.view_revision,
            settlement != "failed",
        );
        if settlement != "cancelled" {
            let response = reply(&mut frames).await;
            assert_eq!(response["id"], 10, "{settlement}");
            if settlement == "prepared" {
                assert_eq!(response["result"]["session_id"], "replacement-display-id");
            } else {
                assert_eq!(response["error"]["code"], -32002);
                assert!(response.get("result").is_none());
            }
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.child_work_slots.available_permits() != MAX_CHILD_WORKERS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let expected_owner = if settlement == "prepared" {
            &replacement
        } else {
            &original
        };
        let expected = if settlement == "prepared" {
            &next
        } else {
            &previous
        };
        assert_eq!(
            lock_std_mutex(&state.pending)[&1].resource_owner.as_ref(),
            Some(expected_owner),
            "{settlement}"
        );
        let store = lock_std_mutex(&state.session_leaf.transport);
        assert_eq!(&store.parents[&1].owner, expected_owner, "{settlement}");
        assert_eq!(store.parents[&1].head, expected.head, "{settlement}");
        assert_eq!(
            store.parents[&1].revision, expected.view_revision,
            "{settlement}"
        );
        drop(store);
        assert_eq!(
            lock_std_mutex(&state.issued_resource_owners).contains(&replacement),
            settlement == "prepared"
        );
        assert!(
            lock_std_mutex(&state.child_requests).is_empty(),
            "{settlement}"
        );
        assert!(frames.try_recv().is_err(), "{settlement}");
    }
}

#[tokio::test]
async fn cancelled_counted_replacement_cannot_rebind_pending_owner_or_transport_pin() {
    let temp = tempfile::tempdir().unwrap();
    let old = durable_session(temp.path().join("old.jsonl"), 1);
    let new = durable_session(temp.path().join("new.jsonl"), 2);
    let (state, mut frames, mut receiver, mut original) = fixture();
    let previous = counted_original(&state, &mut original, &old);
    call(&state, 10, "session/create", 1, &original);
    let request = receiver.try_next().unwrap();
    handle_protocol_line(
        br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":10}}"#,
        &state,
    )
    .unwrap();
    let replacement = ExtensionResourceOwner {
        session_id: new.resource_owner_key(),
        ..original.clone()
    };
    publish_counted(&state, &new, &replacement);
    request.respond_replacement(Ok("replacement-display-id".into()));
    tokio::task::yield_now().await;
    assert_eq!(
        lock_std_mutex(&state.pending)[&1].resource_owner.as_ref(),
        Some(&original)
    );
    let store = lock_std_mutex(&state.session_leaf.transport);
    assert_eq!(store.parents[&1].owner, original);
    assert_eq!(store.parents[&1].head, previous.head);
    assert_eq!(store.parents[&1].revision, previous.view_revision);
    drop(store);
    assert!(lock_std_mutex(&state.issued_resource_owners).is_empty());
    assert!(lock_std_mutex(&state.child_requests).is_empty());
    assert!(frames.try_recv().is_err());
}
