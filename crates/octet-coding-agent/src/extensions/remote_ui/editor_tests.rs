//! Deterministic product arbitration with the real process transport and shell.
//! Frames never acknowledge input; tests explicitly choose the checkpoint race.
//!
//! Test waits observe named transport/file barriers, never grant an editor an
//! arbitrary grace period before rescue.
use super::*;
use crate::extensions::{HostRequestOperation, PendingHostRequest};
use octet_agent::extension_process::{
    ExtensionComposerOperation, ExtensionRequestId, ExtensionRequestOutcome,
};

fn input(extensions: &mut ExecutableExtensions, frontend: &mut Frontend, character: char) {
    assert!(extensions.route_remote_ui_event(
        &mut frontend.shell,
        &key(
            KeyCode::Char(character),
            KeyEventKind::Press,
            KeyModifiers::NONE
        ),
    ));
}

fn rescue(extensions: &mut ExecutableExtensions, frontend: &mut Frontend) {
    assert!(extensions.route_remote_ui_event(
        &mut frontend.shell,
        &key(
            KeyCode::Char('g'),
            KeyEventKind::Press,
            KeyModifiers::CONTROL
        ),
    ));
    assert!(extensions.remote_ui.is_empty());
}

fn checkpoint(
    extensions: &ExecutableExtensions,
    input_revision: u64,
    checkpoint_revision: u64,
) -> (ExtensionResourceOwner, ExtensionEditorCheckpoint) {
    let mount = &extensions.remote_ui.mounts[0];
    (
        mount.owner.clone(),
        ExtensionEditorCheckpoint {
            surface_id: mount.surface_id.clone(),
            mount_id: mount.view.id.clone(),
            input_revision,
            checkpoint_revision,
        },
    )
}

async fn wait_wire(
    extensions: &mut ExecutableExtensions,
    frontend: &mut Frontend,
    path: &PathBuf,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            extensions.drain_events_for_shell(&mut frontend.shell);
            if let Some(message) = wire(path).into_iter().find(&predicate) {
                return message;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the fixture must observe the exact wire event, not a grace period")
}

#[tokio::test]
async fn remote_editor_wire_rescue_restores_acknowledged_draft_and_rejects_late_write() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, log) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    command(&mut extensions, &mut frontend, &["editor", "checkpoint"]).await;
    let mount_id = extensions.remote_ui.mounts[0].view.id.clone();
    input(&mut extensions, &mut frontend, 'c');
    let accepted = wait_wire(&mut extensions, &mut frontend, &log, |m| {
        m["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("checkpoint-"))
            && m["result"]["input_revision"] == 1
    })
    .await;
    assert_eq!(accepted["result"]["checkpoint_revision"], 1);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "acknowledged"
    );

    input(&mut extensions, &mut frontend, 'x');
    let delivered = wait_wire(&mut extensions, &mut frontend, &log, |m| {
        m["method"] == "ui/key" && m["params"]["key"] == "x"
    })
    .await;
    assert_eq!(delivered["params"]["editor_input"]["mount_id"], mount_id);
    assert_eq!(delivered["params"]["editor_input"]["input_revision"], 2);
    // The extension has the next key but has deliberately NOT checkpointed it.
    // Rescue and subsequent native typing perform no await or replay.
    rescue(&mut extensions, &mut frontend);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "acknowledged"
    );
    assert!(frontend
        .shell
        .debug_snapshot()
        .contains("1 input events were not acknowledged"));
    frontend.shell.extension_paste_editor("-native".into());
    let native = frontend.shell.extension_editor_snapshot();
    assert_eq!(native.text, "acknowledged-native");
    let refused = wait_wire(&mut extensions, &mut frontend, &log, |m| {
        m["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("checkpoint-"))
            && m.get("error").is_some()
    })
    .await;
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("retired"));
    assert_eq!(frontend.shell.extension_editor_snapshot().text, native.text);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().revision,
        native.revision
    );
    frontend.shell.extension_paste_editor("!".into());
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "acknowledged-native!"
    );
    extensions.shutdown().await;
}

#[tokio::test]
async fn remote_editor_replacement_cannot_inherit_a_retired_mount_checkpoint() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, _) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    command(&mut extensions, &mut frontend, &["editor", "same"]).await;
    input(&mut extensions, &mut frontend, 'a');
    let (owner, old) = checkpoint(&extensions, 1, 1);
    extensions
        .remote_ui
        .checkpoint_editor(&owner, &old, "committed".into(), &mut frontend.shell)
        .unwrap();
    rescue(&mut extensions, &mut frontend);
    assert!(!frontend
        .shell
        .debug_snapshot()
        .contains("were not acknowledged"));
    command(&mut extensions, &mut frontend, &["editor", "same"]).await;
    let (_, new) = checkpoint(&extensions, 0, 1);
    assert_eq!(new.surface_id, old.surface_id);
    assert_ne!(new.mount_id, old.mount_id);
    assert_eq!(frontend.shell.extension_editor_snapshot().text, "committed");
    assert!(extensions
        .remote_ui
        .checkpoint_editor(
            &owner,
            &old,
            "retired overwrite".into(),
            &mut frontend.shell,
        )
        .is_err());
    extensions
        .remote_ui
        .checkpoint_editor(&owner, &new, "replacement".into(), &mut frontend.shell)
        .unwrap();
    rescue(&mut extensions, &mut frontend);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "replacement"
    );
    extensions.shutdown().await;
}

#[tokio::test]
async fn remote_editor_checkpoints_fence_owner_mount_revision_and_native_changes() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, _) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    command(&mut extensions, &mut frontend, &["editor"]).await;
    for character in ['a', 'b', 'c'] {
        input(&mut extensions, &mut frontend, character);
    }
    let (owner, valid) = checkpoint(&extensions, 2, 1);
    let mut other_session = owner.clone();
    other_session.session_id.push_str("-other");
    let mut other_instance = owner.clone();
    other_instance.extension_instance_id.push_str("-other");
    let mut other_generation = owner.clone();
    other_generation.process_generation += 1;
    for foreign in [other_session, other_instance, other_generation] {
        assert!(extensions
            .remote_ui
            .checkpoint_editor(&foreign, &valid, "foreign".into(), &mut frontend.shell,)
            .is_err());
    }
    let mut wrong_mount = valid.clone();
    wrong_mount.mount_id.push_str("-other");
    let mut future = valid.clone();
    future.input_revision = 4;
    future.checkpoint_revision = 900;
    for invalid in [wrong_mount, future] {
        assert!(extensions
            .remote_ui
            .checkpoint_editor(&owner, &invalid, "invalid".into(), &mut frontend.shell,)
            .is_err());
    }
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    extensions
        .remote_ui
        .checkpoint_editor(&owner, &valid, "ab".into(), &mut frontend.shell)
        .unwrap();
    let editor = extensions.remote_ui.mounts[0].editor.as_ref().unwrap();
    assert_eq!(
        (
            editor.acknowledged_input_revision,
            editor.checkpoint_revision
        ),
        (2, 1)
    );
    let mut old_input = valid.clone();
    old_input.input_revision = 1;
    old_input.checkpoint_revision = 2;
    for invalid in [valid.clone(), old_input] {
        assert!(extensions
            .remote_ui
            .checkpoint_editor(&owner, &invalid, "stale".into(), &mut frontend.shell,)
            .is_err());
    }
    let (_, next) = checkpoint(&extensions, 3, 2);
    // A host action modified the hidden draft. An adapter's old native revision
    // is no longer authority to overwrite that newer host-owned state.
    frontend
        .shell
        .extension_set_editor("new native draft".into());
    assert!(extensions
        .remote_ui
        .checkpoint_editor(&owner, &next, "abc".into(), &mut frontend.shell,)
        .is_err());
    rescue(&mut extensions, &mut frontend);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "new native draft"
    );
    extensions.shutdown().await;
}

#[tokio::test]
async fn remote_editor_noop_and_release_checkpoints_are_not_lost_edit_claims() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, _) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    command(&mut extensions, &mut frontend, &["editor"]).await;
    for kind in [
        KeyEventKind::Press,
        KeyEventKind::Repeat,
        KeyEventKind::Release,
    ] {
        assert!(extensions.route_remote_ui_event(
            &mut frontend.shell,
            &key(KeyCode::Left, kind, KeyModifiers::NONE),
        ));
    }
    let (owner, ack) = checkpoint(&extensions, 3, 1);
    extensions
        .remote_ui
        .checkpoint_editor(&owner, &ack, "untouched draft".into(), &mut frontend.shell)
        .unwrap();
    // Timer-originated text may checkpoint the same fully handled input revision.
    let mut timer = ack;
    timer.checkpoint_revision += 1;
    extensions
        .remote_ui
        .checkpoint_editor(&owner, &timer, "timer edit".into(), &mut frontend.shell)
        .unwrap();
    rescue(&mut extensions, &mut frontend);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "timer edit"
    );
    assert!(!frontend
        .shell
        .debug_snapshot()
        .contains("were not acknowledged"));
    extensions.shutdown().await;
}

#[tokio::test]
async fn remote_editor_rescue_never_waits_for_an_unresponsive_extension() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, _) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    command(&mut extensions, &mut frontend, &["editor"]).await;
    input(&mut extensions, &mut frontend, 'h');
    tokio::time::timeout(Duration::from_secs(3), async {
        while !temp.path().join("editor-hung").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture must stop servicing its transport before rescue");
    assert!(!temp.path().join("editor-release").exists());
    // No timeout, sleep, replay or extension response precedes restoration.
    // The extension cannot resume until this synchronous rescue returns.
    rescue(&mut extensions, &mut frontend);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    assert!(frontend
        .shell
        .debug_snapshot()
        .contains("1 input events were not acknowledged"));
    frontend.shell.extension_paste_editor("-native".into());
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft-native"
    );
    std::fs::write(temp.path().join("editor-release"), "resume fixture").unwrap();
    extensions.shutdown().await;
}

#[tokio::test]
async fn remote_editor_queued_checkpoint_refences_foreground_and_rejects_unfenced_mutations() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, _) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    command(&mut extensions, &mut frontend, &["editor"]).await;
    let process = extensions.processes[0].clone();
    let (owner, ack) = checkpoint(&extensions, 0, 1);
    for (id, operation) in [
        (
            701,
            ExtensionComposerOperation::Set {
                text: "unfenced set".into(),
            },
        ),
        (
            702,
            ExtensionComposerOperation::Insert {
                text: "unfenced insert".into(),
            },
        ),
    ] {
        extensions
            .pending_host_requests
            .push_back(PendingHostRequest {
                process: process.clone(),
                request_id: ExtensionRequestId::Number(id),
                generation: owner.process_generation,
                operation: HostRequestOperation::Composer(operation),
            });
    }
    extensions.drain_host_requests_into_shell(&mut frontend.shell);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    // Admission happened before the foreground moved; drain must check again.
    extensions
        .pending_host_requests
        .push_back(PendingHostRequest {
            process: process.clone(),
            request_id: ExtensionRequestId::Number(703),
            generation: owner.process_generation,
            operation: HostRequestOperation::Composer(ExtensionComposerOperation::Checkpoint {
                owner,
                checkpoint: ack,
                text: "wrong session".into(),
            }),
        });
    extensions.resource_owner = Some("different-session".into());
    extensions.drain_host_requests_into_shell(&mut frontend.shell);
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    extensions.shutdown().await;
}

#[tokio::test]
async fn remote_editor_real_queued_commit_couples_native_draft_and_exact_ack() {
    for mode in [
        "normal-success",
        "parent-cancel-first",
        "child-cancel-first",
        "commit-first",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let (mut extensions, log) = fixture(&temp).await;
        let mut frontend = Frontend::new([]);
        let process = extensions.processes[0].clone();
        let mut observed = process.subscribe();
        let context = process.current_context_for_resource_owner("remote-test-session");
        let call = tokio::spawn({
            let process = process.clone();
            async move {
                process
                    .execute_command(
                        "mount",
                        ["editor", "hold", "checkpoint", "barriers"]
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                        context,
                    )
                    .await
            }
        });
        wait_wire(&mut extensions, &mut frontend, &log, |message| {
            message["result"]["editor_mount_id"].is_string()
        })
        .await;
        input(&mut extensions, &mut frontend, 'c');
        // Observe the real admitted child without draining the product receiver:
        // its native mutation and ACK are now deliberately held at that queue.
        let (request_id, generation) = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let octet_agent::extension_process::ExtensionEvent::ComposerRequested {
                    request_id,
                    generation,
                    operation:
                        ExtensionComposerOperation::Checkpoint {
                            text, checkpoint, ..
                        },
                    ..
                } = observed.recv().await.unwrap()
                {
                    assert_eq!(text, "acknowledged");
                    assert_eq!(
                        (checkpoint.input_revision, checkpoint.checkpoint_revision),
                        (1, 1)
                    );
                    break (request_id, generation);
                }
            }
        })
        .await
        .unwrap();
        let before = frontend.shell.extension_editor_snapshot();
        assert_eq!(before.text, "untouched draft");
        assert!(!call.is_finished());
        match mode {
            "normal-success" => {
                input(&mut extensions, &mut frontend, 's');
                call.await.unwrap().unwrap();
            }
            "child-cancel-first" => {
                input(&mut extensions, &mut frontend, 'z');
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        if let octet_agent::extension_process::ExtensionEvent::Notification {
                            notification,
                        } = observed.recv().await.unwrap()
                        {
                            if notification.message == "checkpoint-cancelled" {
                                break;
                            }
                        }
                    }
                })
                .await
                .unwrap();
                input(&mut extensions, &mut frontend, 's');
                call.await.unwrap().unwrap();
            }
            "commit-first" => {
                extensions.drain_events_for_shell(&mut frontend.shell);
                assert_eq!(
                    frontend.shell.extension_editor_snapshot().text,
                    "acknowledged"
                );
                // No await between real product mutation/ACK admission and the
                // request to cancel its still-live parent.
                call.abort();
                assert!(call.await.unwrap_err().is_cancelled());
            }
            "parent-cancel-first" => {
                call.abort();
                assert!(call.await.unwrap_err().is_cancelled());
            }
            _ => unreachable!(),
        }
        extensions.drain_events_for_shell(&mut frontend.shell);
        let after = frontend.shell.extension_editor_snapshot();
        let committed = matches!(mode, "normal-success" | "commit-first");
        if committed {
            assert_eq!(after.text, "acknowledged", "{mode}");
            assert!(after.revision > before.revision);
            // A second response attempt cannot publish a mismatched ACK.
            process
                .respond_to_extension_request(
                    request_id.clone(),
                    generation,
                    ExtensionRequestOutcome::Ok(
                        serde_json::json!({"input_revision":99,"checkpoint_revision":99}),
                    ),
                )
                .await
                .unwrap();
            wait_wire(&mut extensions, &mut frontend, &log, |message| {
                message["id"] == serde_json::to_value(&request_id).unwrap()
                    && message.get("result").is_some()
            })
            .await;
        } else {
            assert_eq!(after.text, before.text, "{mode}");
            assert_eq!(after.revision, before.revision, "{mode}");
        }
        if !extensions.remote_ui.is_empty() {
            rescue(&mut extensions, &mut frontend);
        }
        frontend.shell.extension_paste_editor("-native".into());
        let final_draft = frontend.shell.extension_editor_snapshot();
        extensions.shutdown().await;
        let acks = wire(&log)
            .into_iter()
            .filter(|message| {
                message["id"] == serde_json::to_value(&request_id).unwrap()
                    && message.get("result").is_some()
            })
            .collect::<Vec<_>>();
        assert_eq!(acks.len(), usize::from(committed), "{mode}");
        if committed {
            assert_eq!(
                acks[0]["result"],
                serde_json::json!({"input_revision":1,"checkpoint_revision":1})
            );
        }
        assert_eq!(
            frontend.shell.extension_editor_snapshot().text,
            final_draft.text,
            "{mode}: no late native mutation"
        );
    }
}

#[tokio::test]
async fn remote_editor_revoke_reload_and_paste_disclose_unrecovered_input() {
    for reload in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (mut extensions, _) = fixture(&temp).await;
        let mut frontend = Frontend::new([]);
        command(&mut extensions, &mut frontend, &["editor"]).await;
        input(&mut extensions, &mut frontend, 'a');
        assert!(extensions
            .route_remote_ui_event(&mut frontend.shell, &Event::Paste("not delivered".into())));
        assert!(frontend
            .shell
            .debug_snapshot()
            .contains("Paste was not delivered"));
        if reload {
            extensions.processes[0].reload().await.unwrap();
            extensions.drain_events_for_shell(&mut frontend.shell);
        } else {
            extensions.revoke_terminal_grant_for_shell(&mut frontend.shell, "test owner retired");
        }
        assert!(extensions.remote_ui.is_empty());
        assert_eq!(
            frontend.shell.extension_editor_snapshot().text,
            "untouched draft"
        );
        let visible = if reload {
            frontend
                .shell
                .debug_error()
                .expect("reconciliation surfaces the recovery warning")
        } else {
            frontend.shell.debug_snapshot()
        };
        assert!(visible.contains("1 input events were not acknowledged"));
        frontend.shell.extension_paste_editor("-native".into());
        assert_eq!(
            frontend.shell.extension_editor_snapshot().text,
            "untouched draft-native"
        );
        extensions.shutdown().await;
    }
}
