//! Checkpoints alone retain a mount/session lifetime across normal parent success.

use super::super::tests::{
    insert_test_parent, protocol_read_state_for_test, test_resource_owner, wave1_error, wave1_line,
};
#[cfg(unix)]
use super::super::tests::{trusted_descriptor, write_executable_script};
use super::*;
use pretty_assertions::assert_eq;

fn editor_state() -> (
    ProtocolReadState,
    mpsc::Receiver<WriterFrame>,
    broadcast::Receiver<ExtensionEvent>,
) {
    let (events, received) = broadcast::channel(16);
    let (mut state, frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    state.remote_ui = Arc::new(RemoteUiMailbox::new(Some(Arc::new(Notify::new()))));
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = EXTENSION_API_VERSION_0_4.into();
        protocol.features.extend([
            EXTENSION_FEATURE_COMPOSER.into(),
            EXTENSION_FEATURE_REMOTE_UI.into(),
        ]);
    }
    insert_test_parent(&state, 7, Some(test_resource_owner("session")));
    open_editor(&state);
    (state, frames, received)
}

fn open_editor(state: &ProtocolReadState) {
    let reservation = state
        .remote_ui
        .reserve(
            ExtensionRequestId::Number(90),
            test_resource_owner("session"),
            ExtensionRemoteUiOperation::Open {
                surface_id: "editor".into(),
                title: "Editor".into(),
                placement: crate::ExtensionRemoteUiPlacement::Editor,
                mouse_capture: false,
            },
            Some(7),
        )
        .unwrap();
    reservation
        .prepare_response(Some(&serde_json::json!({
            "columns":80,"rows":3,"editor_mount_id":"remote.1"
        })))
        .unwrap()
        .unwrap()
        .commit()
        .unwrap();
}

fn checkpoint_params() -> serde_json::Value {
    serde_json::json!({
        "parent_request_id":7,"resource_owner":test_resource_owner("session"),
        "text":"complete draft 🦀", "editor_checkpoint":{
            "surface_id":"editor","mount_id":"remote.1",
            "input_revision":0,"checkpoint_revision":1
        }
    })
}

fn settle_parent(state: &ProtocolReadState) {
    handle_protocol_line(br#"{"jsonrpc":"2.0","id":7,"result":{}}"#, state).unwrap();
}

#[test]
fn editor_checkpoint_success_retains_exactly_once_response_and_rejects_replay() {
    let (state, mut frames, mut events) = editor_state();
    let request = wave1_line(100, methods::COMPOSER_SET, checkpoint_params());
    handle_protocol_line(&request, &state).unwrap();
    assert!(matches!(
        events.try_recv().unwrap(),
        ExtensionEvent::ComposerRequested {
            operation: ExtensionComposerOperation::Checkpoint { .. },
            ..
        }
    ));
    let id = ExtensionRequestId::Number(100);
    assert_eq!(
        lock_std_mutex(&state.child_requests)[&id].parent_request_id,
        0
    );
    settle_parent(&state);
    assert!(
        frames.try_recv().is_err(),
        "normal success must not cancel checkpoint"
    );
    assert_eq!(
        state.remote_ui.host_owner("editor").unwrap(),
        test_resource_owner("session")
    );
    let response = serde_json::json!({"jsonrpc":"2.0","id":100,
        "result":{"input_revision":0,"checkpoint_revision":1}});
    assert_eq!(
        try_queue_child_response(
            &state.child_requests,
            &id,
            &state.writer,
            state.max_message_bytes(),
            response.clone()
        )
        .unwrap(),
        ChildResponseAdmission::Queued
    );
    assert_eq!(
        try_queue_child_response(
            &state.child_requests,
            &id,
            &state.writer,
            state.max_message_bytes(),
            response.clone()
        )
        .unwrap(),
        ChildResponseAdmission::AlreadySettled
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line).unwrap(),
        response
    );
    assert!(frames.try_recv().is_err());
    assert!(handle_protocol_line(&request, &state)
        .unwrap_err()
        .contains("reused"));
}

#[test]
fn editor_checkpoint_plain_calls_and_nonmatching_owners_keep_live_parent_lifetime() {
    for case in [
        "plain",
        "null",
        "insert",
        "get",
        "missing-owner",
        "forged-session",
        "forged-instance",
        "stale-generation",
        "different-issued-owner",
        "unissued-owner",
        "retired-surface",
        "opening-surface",
        "stale-mount",
        "non-editor",
    ] {
        let (state, mut frames, mut events) = editor_state();
        let mut params = checkpoint_params();
        let mut method = methods::COMPOSER_SET;
        let mut opening = None;
        match case {
            "plain" | "insert" => {
                params.as_object_mut().unwrap().remove("editor_checkpoint");
                if case == "insert" {
                    method = methods::COMPOSER_INSERT;
                }
            }
            "null" => params["editor_checkpoint"] = serde_json::Value::Null,
            "get" => {
                method = methods::COMPOSER_GET;
                params.as_object_mut().unwrap().remove("editor_checkpoint");
                params.as_object_mut().unwrap().remove("text");
            }
            "missing-owner" => {
                params.as_object_mut().unwrap().remove("resource_owner");
            }
            "forged-session" => params["resource_owner"]["session_id"] = "forged".into(),
            "forged-instance" => {
                params["resource_owner"]["extension_instance_id"] = "foreign".into()
            }
            "stale-generation" => params["resource_owner"]["process_generation"] = 2.into(),
            "different-issued-owner" => {
                let other = test_resource_owner("other");
                lock_std_mutex(&state.issued_resource_owners).insert(other.clone());
                params["resource_owner"] = serde_json::to_value(other).unwrap();
            }
            "unissued-owner" => lock_std_mutex(&state.issued_resource_owners).clear(),
            "stale-mount" => params["editor_checkpoint"]["mount_id"] = "retired.0".into(),
            "non-editor" => {
                state
                    .remote_ui
                    .discard_owner(&test_resource_owner("session"));
                let reservation = state
                    .remote_ui
                    .reserve(
                        ExtensionRequestId::Number(92),
                        test_resource_owner("session"),
                        ExtensionRemoteUiOperation::Open {
                            surface_id: "editor".into(),
                            title: "Not an editor".into(),
                            placement: crate::ExtensionRemoteUiPlacement::Fullscreen,
                            mouse_capture: false,
                        },
                        Some(7),
                    )
                    .unwrap();
                reservation
                    .prepare_response(Some(&serde_json::json!({"columns":80,"rows":24})))
                    .unwrap()
                    .unwrap()
                    .commit()
                    .unwrap();
            }
            "retired-surface" | "opening-surface" => {
                state
                    .remote_ui
                    .discard_owner(&test_resource_owner("session"));
                if case == "opening-surface" {
                    opening = Some(
                        state
                            .remote_ui
                            .reserve(
                                ExtensionRequestId::Number(91),
                                test_resource_owner("session"),
                                ExtensionRemoteUiOperation::Open {
                                    surface_id: "editor".into(),
                                    title: "Editor".into(),
                                    placement: crate::ExtensionRemoteUiPlacement::Editor,
                                    mouse_capture: false,
                                },
                                Some(7),
                            )
                            .unwrap(),
                    );
                }
            }
            _ => unreachable!(),
        }
        handle_protocol_line(&wave1_line(100, method, params), &state).unwrap();
        assert!(
            matches!(events.try_recv().unwrap(), ExtensionEvent::ComposerRequested { owner: Some(owner), .. }
            if owner == test_resource_owner("session")),
            "{case}: active parent must remain authoritative"
        );
        assert_eq!(
            lock_std_mutex(&state.child_requests)[&ExtensionRequestId::Number(100)]
                .parent_request_id,
            7,
            "{case}"
        );
        settle_parent(&state);
        assert!(lock_std_mutex(&state.child_requests).is_empty(), "{case}");
        let cancelled: serde_json::Value =
            serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap();
        assert_eq!(cancelled["method"], "$/cancelRequest", "{case}");
        assert_eq!(
            cancelled["params"],
            serde_json::json!({"id":100,"reason":"parent settled"}),
            "{case}"
        );
        drop(opening);
    }
}

#[test]
fn editor_checkpoint_independence_requires_negotiation_frontend_and_valid_bounds() {
    for case in [
        "old-api",
        "no-remote-ui",
        "no-composer",
        "no-frontend",
        "closed",
        "draining",
        "bad-surface",
        "bad-mount",
        "future-input",
        "zero-checkpoint",
        "unknown-field",
        "oversized-text",
        "control-text",
        "insert",
    ] {
        let (mut state, mut frames, mut events) = editor_state();
        let mut params = checkpoint_params();
        let mut method = methods::COMPOSER_SET;
        let expected = match case {
            "old-api" => {
                write_std_lock(&state.protocol).version = "0.2".into();
                Some("unsupported_feature")
            }
            "no-remote-ui" => {
                write_std_lock(&state.protocol)
                    .features
                    .remove(EXTENSION_FEATURE_REMOTE_UI);
                Some("unsupported_feature")
            }
            "no-composer" => {
                write_std_lock(&state.protocol)
                    .features
                    .remove(EXTENSION_FEATURE_COMPOSER);
                Some("unsupported_feature")
            }
            "no-frontend" => {
                state.remote_ui = Arc::new(RemoteUiMailbox::new(None));
                open_editor(&state);
                None
            }
            "closed" => {
                state.closed.store(true, Ordering::Release);
                None
            }
            "draining" => {
                state.draining.store(true, Ordering::Release);
                None
            }
            "bad-surface" => {
                params["editor_checkpoint"]["surface_id"] = "bad id".into();
                Some("invalid_request")
            }
            "bad-mount" => {
                params["editor_checkpoint"]["mount_id"] = "bad id".into();
                Some("invalid_request")
            }
            "future-input" => {
                params["editor_checkpoint"]["input_revision"] =
                    serde_json::json!(9007199254740992u64);
                Some("bounds_exceeded")
            }
            "zero-checkpoint" => {
                params["editor_checkpoint"]["checkpoint_revision"] = 0.into();
                Some("bounds_exceeded")
            }
            "unknown-field" => {
                params["editor_checkpoint"]["extra"] = true.into();
                Some("invalid_request")
            }
            "oversized-text" => {
                params["text"] = "x".repeat(MAX_EXTENSION_COMPOSER_TEXT_BYTES + 1).into();
                Some("bounds_exceeded")
            }
            "control-text" => {
                params["text"] = "\u{1b}[2J".into();
                Some("invalid_request")
            }
            "insert" => {
                method = methods::COMPOSER_INSERT;
                Some("invalid_request")
            }
            _ => unreachable!(),
        };
        handle_protocol_line(&wave1_line(100, method, params), &state).unwrap();
        if let Some(expected) = expected {
            assert_eq!(
                wave1_error(&frames.try_recv().unwrap()).1,
                expected,
                "{case}"
            );
            assert!(events.try_recv().is_err(), "{case}");
            assert!(lock_std_mutex(&state.child_requests).is_empty(), "{case}");
        } else {
            // These internal states cannot negotiate remote_ui in a real process;
            // even with a preloaded surface they must never gain independence.
            assert_eq!(
                lock_std_mutex(&state.child_requests)[&ExtensionRequestId::Number(100)]
                    .parent_request_id,
                7,
                "{case}"
            );
        }
    }
}

#[test]
fn editor_checkpoint_child_cancellation_and_cancelled_terminal_preserve_fences() {
    let (state, mut frames, mut events) = editor_state();
    handle_protocol_line(
        &wave1_line(100, methods::COMPOSER_SET, checkpoint_params()),
        &state,
    )
    .unwrap();
    events.try_recv().unwrap();
    handle_protocol_line(
        br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":100}}"#,
        &state,
    )
    .unwrap();
    assert_eq!(
        try_queue_child_response(
            &state.child_requests,
            &ExtensionRequestId::Number(100),
            &state.writer,
            state.max_message_bytes(),
            serde_json::json!({"jsonrpc":"2.0","id":100,"result":{}})
        )
        .unwrap(),
        ChildResponseAdmission::AlreadySettled
    );
    assert!(frames.try_recv().is_err());
    handle_protocol_line(
        &wave1_line(101, methods::COMPOSER_SET, checkpoint_params()),
        &state,
    )
    .unwrap();
    events.try_recv().unwrap();
    handle_protocol_line(
        br#"{"jsonrpc":"2.0","id":7,"error":{"code":-32800,"message":"cancelled"}}"#,
        &state,
    )
    .unwrap();
    assert!(validate_explicit_request_owner(&state, &test_resource_owner("session")).is_err());
    assert!(state.remote_ui.host_owner("editor").is_err());
    // The queued child may still receive a typed refusal, but has no current
    // owner/surface with which to pass the product's mutation fence.
    handle_protocol_line(
        &wave1_line(102, methods::COMPOSER_SET, checkpoint_params()),
        &state,
    )
    .unwrap();
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()).1,
        "not_foreground_owner"
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn editor_checkpoint_deferred_forged_or_retired_owner_is_refused() {
    for case in ["session", "instance", "generation", "retired"] {
        let (state, mut frames, mut events) = editor_state();
        settle_parent(&state);
        let mut params = checkpoint_params();
        match case {
            "session" => params["resource_owner"]["session_id"] = "forged".into(),
            "instance" => params["resource_owner"]["extension_instance_id"] = "foreign".into(),
            "generation" => params["resource_owner"]["process_generation"] = 2.into(),
            "retired" => {
                lock_std_mutex(&state.issued_resource_owners).clear();
                state
                    .remote_ui
                    .discard_owner(&test_resource_owner("session"));
            }
            _ => unreachable!(),
        }
        handle_protocol_line(&wave1_line(100, methods::COMPOSER_SET, params), &state).unwrap();
        assert_eq!(
            wave1_error(&frames.try_recv().unwrap()).1,
            "not_foreground_owner",
            "{case}"
        );
        assert!(events.try_recv().is_err(), "{case}");
        assert!(lock_std_mutex(&state.child_requests).is_empty(), "{case}");
    }
}

#[test]
fn editor_checkpoint_cancelled_parent_cannot_fall_back_to_a_reissued_owner() {
    let (state, mut frames, mut events) = editor_state();
    lock_std_mutex(&state.pending).remove(&7);
    lock_std_mutex(&state.tombstones).insert(7, Duration::from_secs(60));
    // A genuinely issued owner and surface still cannot bypass this tombstone.
    handle_protocol_line(
        &wave1_line(100, methods::COMPOSER_SET, checkpoint_params()),
        &state,
    )
    .unwrap();
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()).1,
        "not_foreground_owner"
    );
    assert!(events.try_recv().is_err());
    assert!(lock_std_mutex(&state.child_requests).is_empty());
}

#[cfg(unix)]
async fn next_event(events: &mut broadcast::Receiver<ExtensionEvent>) -> ExtensionEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap()
}

#[cfg(unix)]
async fn wait_notice(events: &mut broadcast::Receiver<ExtensionEvent>, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::Notification { notification } = events.recv().await.unwrap() {
                if notification.message == expected {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn editor_checkpoint_real_process_settlement_cancellation_and_retirement() {
    for mode in [
        "success",
        "plain",
        "cancel",
        "cancelled-terminal",
        "rescue",
        "replace",
        "reload",
        "writer-full",
        "child-cancel",
        "cancel-first",
        "commit-first",
    ] {
        let temp = tempfile::TempDir::new().unwrap();
        write_executable_script(
            &temp.path().join("extension.py"),
            include_str!("fixtures/editor-checkpoint-lifetime.py"),
        );
        let manifest = ExtensionManifest::parse(
            r#"
name = "editor-checkpoint-lifetime"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
commands = ["probe"]
notifications = true
"#,
        )
        .unwrap();
        let mut config = ExtensionRuntimeConfig::new(temp.path());
        config.remote_ui = Some(Arc::new(Notify::new()));
        config.supervise = false;
        config.request_timeout = Duration::from_secs(5);
        let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
            .await
            .unwrap();
        let mut events = process.subscribe();
        let context = process.current_context_for_resource_owner("session");
        let owner = context.resource_owner.clone().unwrap();
        let call = tokio::spawn({
            let process = process.clone();
            async move {
                process
                    .execute_command("probe", vec![mode.into()], context)
                    .await
            }
        });
        let (open_id, generation) = loop {
            if let ExtensionEvent::RemoteUiRequested {
                request_id,
                generation,
                operation,
                ..
            } = next_event(&mut events).await
            {
                assert!(matches!(
                    operation,
                    ExtensionRemoteUiOperation::Open {
                        placement: crate::ExtensionRemoteUiPlacement::Editor,
                        ..
                    }
                ));
                break (request_id, generation);
            }
        };
        process
            .respond_to_extension_request(
                open_id,
                generation,
                ExtensionRequestOutcome::Ok(
                    serde_json::json!({"columns":80,"rows":3,"editor_mount_id":"remote.1"}),
                ),
            )
            .await
            .unwrap();
        let checkpoint_id = ExtensionRequestId::String("checkpoint".into());
        let mut checkpoint_seen = false;
        let mut wire_checkpoint = None;
        let (parent_id, parent_terminal) = loop {
            if let ExtensionEvent::ComposerRequested {
                request_id,
                generation: event_generation,
                owner: event_owner,
                operation,
            } = next_event(&mut events).await
            {
                assert_eq!(event_generation, generation);
                assert_eq!(event_owner, Some(owner.clone()));
                if request_id == checkpoint_id {
                    assert!(!checkpoint_seen);
                    checkpoint_seen = true;
                    match operation {
                        ExtensionComposerOperation::Checkpoint {
                            text,
                            owner: operation_owner,
                            checkpoint,
                        } if mode != "plain" => {
                            assert_eq!(text, "complete draft 🦀");
                            assert_eq!(operation_owner, owner);
                            assert_eq!(checkpoint.mount_id, "remote.1");
                            assert_eq!(
                                (checkpoint.input_revision, checkpoint.checkpoint_revision),
                                (0, 1)
                            );
                            wire_checkpoint = Some(checkpoint);
                        }
                        ExtensionComposerOperation::Set { text } if mode == "plain" => {
                            assert_eq!(text, "complete draft 🦀")
                        }
                        _ => panic!("unexpected checkpoint operation: {operation:?}"),
                    }
                } else {
                    assert_eq!(request_id, ExtensionRequestId::String("barrier".into()));
                    assert_eq!(operation, ExtensionComposerOperation::Get);
                    assert!(checkpoint_seen, "barrier follows real checkpoint admission");
                    let connection = read_std_lock(&process.inner.connection).clone();
                    let child_parent = lock_std_mutex(&connection.child_requests)[&checkpoint_id]
                        .parent_request_id;
                    assert_eq!(child_parent == 0, mode != "plain");
                    let parent_id =
                        lock_std_mutex(&connection.child_requests)[&request_id].parent_request_id;
                    let parent_terminal =
                        Arc::clone(&lock_std_mutex(&connection.pending)[&parent_id].terminal);
                    process
                        .respond_to_extension_request(
                            request_id,
                            generation,
                            ExtensionRequestOutcome::Ok(serde_json::json!({"text":""})),
                        )
                        .await
                        .unwrap();
                    break (parent_id, parent_terminal);
                }
            }
        };
        if mode == "cancel-first" {
            let connection = read_std_lock(&process.inner.connection).clone();
            // Block cleanup immediately after its terminal transition. Unlike
            // the old race, cancellation must still own pending while the live
            // surface is visible. No sleep controls this interleaving.
            let tombstones = lock_std_mutex(&connection.tombstones);
            let runtime = tokio::runtime::Handle::current();
            let cancelling = std::thread::spawn({
                let connection = Arc::clone(&connection);
                move || {
                    let _runtime = runtime.enter();
                    connection.cancel_request(parent_id, "test cancellation");
                }
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while parent_terminal.load(Ordering::Acquire) != REQUEST_CANCELLED {
                assert!(
                    Instant::now() < deadline,
                    "cancellation never acquired disposition"
                );
                std::thread::yield_now();
            }
            assert!(connection.pending.try_lock().is_err());
            assert!(connection.remote_ui.host_owner("editor").is_ok());
            drop(tombstones);
            cancelling.join().unwrap();
        } else if mode == "commit-first" {
            let (entered, observed) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel();
            let mutations = Arc::new(AtomicUsize::new(0));
            let committing = std::thread::spawn({
                let process = process.clone();
                let owner = owner.clone();
                let checkpoint = wire_checkpoint.clone().unwrap();
                let checkpoint_id = checkpoint_id.clone();
                let mutations = Arc::clone(&mutations);
                move || {
                    process.commit_editor_checkpoint(
                        &checkpoint_id,
                        generation,
                        &owner,
                        &checkpoint,
                        || {
                            entered.send(()).unwrap();
                            released.recv_timeout(Duration::from_secs(5)).unwrap();
                            mutations.fetch_add(1, Ordering::AcqRel);
                            Ok(())
                        },
                    )
                }
            });
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
            let connection = read_std_lock(&process.inner.connection).clone();
            assert!(connection.pending.try_lock().is_err());
            assert!(connection.child_requests.try_lock().is_err());
            let runtime = tokio::runtime::Handle::current();
            let cancelling = std::thread::spawn(move || {
                let _runtime = runtime.enter();
                connection.cancel_request(parent_id, "test cancellation");
            });
            release.send(()).unwrap();
            committing.join().unwrap().unwrap();
            cancelling.join().unwrap();
            assert_eq!(mutations.load(Ordering::Acquire), 1);
        }
        if mode == "cancel" {
            call.abort();
            assert!(call.await.unwrap_err().is_cancelled());
            wait_notice(&mut events, "parent-cancelled").await;
        } else {
            let result = call.await.unwrap();
            if matches!(mode, "cancelled-terminal" | "cancel-first" | "commit-first") {
                assert!(result.is_err());
                if mode != "cancelled-terminal" {
                    wait_notice(&mut events, "parent-cancelled").await;
                }
            } else {
                assert_eq!(result.unwrap().text, "settled");
            }
        }
        match mode {
            "plain" => {
                wait_notice(&mut events, "plain-cancelled").await;
                let connection = read_std_lock(&process.inner.connection).clone();
                assert!(!lock_std_mutex(&connection.child_requests).contains_key(&checkpoint_id));
            }
            "commit-first" => {
                wait_notice(&mut events, "checkpoint-ack").await;
                assert!(!process.remote_ui_surface_is_current(&owner, "editor"));
                let connection = read_std_lock(&process.inner.connection).clone();
                assert!(!lock_std_mutex(&connection.child_requests).contains_key(&checkpoint_id));
            }
            "child-cancel" => {
                let mut mutated = false;
                assert!(process
                    .commit_editor_checkpoint(
                        &checkpoint_id,
                        generation,
                        &owner,
                        wire_checkpoint.as_ref().unwrap(),
                        || {
                            mutated = true;
                            Ok(())
                        }
                    )
                    .is_err());
                assert!(!mutated);
                assert!(process.remote_ui_surface_is_current(&owner, "editor"));
            }
            "success" | "writer-full" => {
                assert!(process.remote_ui_surface_is_current(&owner, "editor"));
                let connection = read_std_lock(&process.inner.connection).clone();
                let checkpoint = wire_checkpoint.as_ref().unwrap();
                let mut mutations = 0;
                if mode == "writer-full" {
                    let mut permits = Vec::new();
                    while let Ok(permit) = connection.writer.try_reserve() {
                        permits.push(permit);
                    }
                    // The parent reply follows receipt of our barrier response;
                    // every writer slot must now be held, not merely unavailable
                    // momentarily because an older frame is still draining.
                    assert_eq!(permits.len(), connection.writer.max_capacity());
                    let refusal = process
                        .commit_editor_checkpoint(
                            &checkpoint_id,
                            generation,
                            &owner,
                            checkpoint,
                            || {
                                mutations += 1;
                                Ok(())
                            },
                        )
                        .unwrap_err();
                    assert_eq!(refusal.0, ExtensionRequestFailure::InvalidRequest);
                    assert!(refusal.1.contains("writer admission unavailable"));
                    assert_eq!(mutations, 0);
                    assert_eq!(
                        lock_std_mutex(&connection.child_requests)[&checkpoint_id]
                            .response_state
                            .state
                            .load(Ordering::Acquire),
                        CHILD_ACTIVE
                    );
                    drop(permits);
                }
                process
                    .commit_editor_checkpoint(
                        &checkpoint_id,
                        generation,
                        &owner,
                        checkpoint,
                        || {
                            mutations += 1;
                            Ok(())
                        },
                    )
                    .unwrap();
                assert_eq!(mutations, 1);
                let ack = serde_json::json!({"input_revision":0,"checkpoint_revision":1});
                wait_notice(&mut events, "checkpoint-ack").await;
                assert_eq!(
                    connection
                        .send_child_response_admitted(checkpoint_id.clone(), &ack)
                        .await
                        .unwrap(),
                    ChildResponseAdmission::AlreadySettled
                );
            }
            _ => {
                if mode == "rescue" {
                    process
                        .notify_remote_ui_closed(ExtensionRemoteUiClosed {
                            surface_id: "editor".into(),
                            reason: "host dismissed".into(),
                        })
                        .unwrap();
                } else if mode == "replace" {
                    let mut host = process.current_context().host;
                    host.session_id = Some("replacement".into());
                    process.set_host_state(host);
                } else if mode == "reload" {
                    assert_eq!(process.reload().await.unwrap().generation, generation + 1);
                }
                assert!(
                    !process.remote_ui_surface_is_current(&owner, "editor"),
                    "{mode}"
                );
                let connection = read_std_lock(&process.inner.connection).clone();
                if mode != "rescue" {
                    assert!(
                        !lock_std_mutex(&connection.issued_resource_owners).contains(&owner),
                        "{mode}"
                    );
                }
                let mut mutated = false;
                assert!(process
                    .commit_editor_checkpoint(
                        &checkpoint_id,
                        generation,
                        &owner,
                        wire_checkpoint.as_ref().unwrap(),
                        || {
                            mutated = true;
                            Ok(())
                        }
                    )
                    .is_err());
                assert!(
                    !mutated,
                    "{mode}: retired disposition must precede mutation"
                );
                let refused = process
                    .respond_to_extension_request(
                        checkpoint_id,
                        generation,
                        ExtensionRequestOutcome::Failed(
                            ExtensionRequestFailure::NotForegroundOwner,
                            "editor mount retired before commit".into(),
                        ),
                    )
                    .await;
                if mode == "reload" {
                    assert!(refused.is_err());
                } else {
                    refused.unwrap();
                    wait_notice(&mut events, "checkpoint-refused").await;
                }
            }
        }
        assert!(process.shutdown().await, "{mode}");
    }
}
