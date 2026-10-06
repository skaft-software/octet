//! Turn lifecycle delivery from `begin_turn` to `settle_turn`.
//!
//! The host promises an ordered pair of `turn/started` and `turn/settled`
//! deliveries to the session owner. These tests cover the production wire payloads
//! for the prompt hooks, owner binding and success-response content under API
//! 0.2, a duplicate command name routing to its owning process, a confirmed
//! presentation action funding exactly the one command that asked for
//! confirmation, and the three failure modes that matter for the drop paths —
//! dropping the `begin_turn` future mid-delivery, dropping the `settle` future,
//! and settling a turn with a non-completion outcome.

use super::support::*;
use super::*;

#[cfg(unix)]
fn recorded_turn_lifecycle(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            matches!(
                message.get("method").and_then(serde_json::Value::as_str),
                Some("turn/started" | "turn/settled")
            )
        })
        .collect()
}

#[cfg(unix)]
async fn wait_for_recorded_turn_lifecycle(path: &Path, minimum: usize) -> Vec<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let events = recorded_turn_lifecycle(path);
            if events.len() >= minimum {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("lifecycle fixture did not receive its terminal boundary")
}

#[cfg(unix)]
fn assert_one_ordered_turn_with_reason(
    events: &[serde_json::Value],
    outcome: &str,
    reason: Option<&str>,
) {
    assert_eq!(events.len(), 2, "unexpected lifecycle wire: {events:#?}");
    assert_eq!(events[0]["method"], "turn/started");
    assert_eq!(events[1]["method"], "turn/settled");
    assert_eq!(events[1]["params"]["outcome"], outcome);
    if let Some(reason) = reason {
        assert_eq!(events[1]["params"]["reason"], reason);
    } else {
        assert!(events[1]["params"]["reason"].is_null());
    }
    for id in ["session_id", "run_id", "turn_id"] {
        assert_eq!(
            events[0]["params"][id], events[1]["params"][id],
            "{id} changed across the lifecycle pair"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_begin_turn_during_started_delivery_preserves_ordered_terminal_owner() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, wire_log) = lifecycle_fixture(&temp).await;
    let control = std::sync::Arc::new(LifecycleDeliveryTestControl::default());
    control.gate_turn_started();
    extensions.lifecycle_delivery_test_control = Some(control.clone());

    let mut beginning = Box::pin(extensions.begin_turn());
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            () = control.turn_started_entered() => {}
            _ = &mut beginning => panic!("begin_turn completed while start delivery was gated"),
        }
    })
    .await
    .expect("begin_turn never transferred start delivery to its owned task");

    // Cancelling the caller here drops the already-constructed guard. Its
    // Drop owner must wait for TurnStarted and then emit one TurnSettled.
    drop(beginning);
    control.release_turn_started();
    let events = wait_for_recorded_turn_lifecycle(&wire_log, 2).await;
    assert_one_ordered_turn_with_reason(
        &events,
        "frontend_disconnected",
        Some("turn owner dropped before explicit settlement"),
    );

    extensions.shutdown().await;
    assert_one_ordered_turn_with_reason(
        &recorded_turn_lifecycle(&wire_log),
        "frontend_disconnected",
        Some("turn owner dropped before explicit settlement"),
    );
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_settle_future_keeps_owned_terminal_delivery_alive() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, wire_log) = lifecycle_fixture(&temp).await;
    let control = std::sync::Arc::new(LifecycleDeliveryTestControl::default());
    extensions.lifecycle_delivery_test_control = Some(control.clone());
    let turn = extensions.begin_turn().await;

    control.gate_turn_settled();
    let outcome = crate::modes::HostRunOutcome::Failed("fixture failure".into());
    let mut settling = Box::pin(extensions.settle_turn(turn, &outcome));
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            () = control.turn_settled_entered() => {}
            () = &mut settling => panic!("settle_turn completed while terminal delivery was gated"),
        }
    })
    .await
    .expect("settle_turn never transferred terminal delivery to its owned task");

    // The detached notification task, not this cancellable caller future,
    // owns terminal delivery after the boundary above.
    drop(settling);
    control.release_turn_settled();
    let events = wait_for_recorded_turn_lifecycle(&wire_log, 2).await;
    assert_one_ordered_turn_with_reason(&events, "failed", Some("fixture failure"));

    extensions.shutdown().await;
    assert_one_ordered_turn_with_reason(
        &recorded_turn_lifecycle(&wire_log),
        "failed",
        Some("fixture failure"),
    );
}

#[cfg(unix)]
#[tokio::test]
async fn settle_turn_reports_non_completion_outcomes_with_matching_extension_reason() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, wire_log) = lifecycle_fixture(&temp).await;

    let cases = [
        (
            crate::modes::HostRunOutcome::Aborted,
            "cancelled",
            Some("run aborted before completing"),
        ),
        (
            crate::modes::HostRunOutcome::Failed("fixture failure".into()),
            "failed",
            Some("fixture failure"),
        ),
        (
            crate::modes::HostRunOutcome::MaxTurns,
            "limit_reached",
            Some("run hit max turns before completing"),
        ),
        (
            crate::modes::HostRunOutcome::stream_lost(),
            "frontend_disconnected",
            Some(crate::modes::RUN_STREAM_LOST_MESSAGE),
        ),
        (
            crate::modes::HostRunOutcome::Shutdown,
            "shutdown",
            Some(crate::modes::RUN_SHUTDOWN_MESSAGE),
        ),
    ];

    for case in cases.iter() {
        let (outcome, expected_outcome, expected_reason) = case;
        let before = recorded_turn_lifecycle(&wire_log).len();
        let turn = extensions.begin_turn().await;
        extensions.settle_turn(turn, outcome).await;
        let events = wait_for_recorded_turn_lifecycle(&wire_log, before + 2).await;
        assert_one_ordered_turn_with_reason(&events[before..], expected_outcome, *expected_reason);
    }

    extensions.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn prompt_hooks_use_the_production_wire_payloads() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let log_path = temp.path().join("prompt-hooks.jsonl");
    let fixture = temp.path().join("prompt-hooks.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
set -eu
log=$1

read_request() {
  IFS= read -r request
  printf '%s\n' "$request" >> "$log"
}

request_id() {
  printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'
}

read_request
id=$(request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.1","tools":[],"commands":[]}}\n' "$id"

read_request
id=$(request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"disposition":{"action":"continue"},"context":[],"notifications":[]}}\n' "$id"

read_request
id=$(request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"disposition":{"action":"continue"},"context":[],"notifications":[]}}\n' "$id"

read_request
id=$(request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let mut manifest = ExtensionManifest::parse(
        r#"
name = "prompt-hooks"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "prompt-hooks.sh"

[contributes]
hooks = ["before_prompt", "after_response"]
"#,
    )
    .unwrap();
    manifest.entrypoint.args = vec![log_path.to_string_lossy().into_owned()];
    let manifest_path = temp.path().join(EXTENSION_MANIFEST_FILENAME);
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.request_timeout = Duration::from_secs(2);
    runtime.shutdown_timeout = Duration::from_secs(2);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        runtime,
    )
    .await
    .unwrap();

    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    let composition = extensions
        .compose_prompt("system", "Explain the contract".into())
        .await
        .unwrap();
    assert_eq!(composition.system, "system");
    assert_eq!(composition.prompt, "Explain the contract");
    assert!(composition.notifications.is_empty());
    assert!(extensions
        .after_response("The contract is bounded.")
        .await
        .is_empty());
    extensions.shutdown().await;
    assert!(!process.is_running());

    let frames = std::fs::read_to_string(&log_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 4, "unexpected wire transcript: {frames:#?}");
    let context = serde_json::json!({
        "workspace": temp.path(),
        "execution_scope": null,
        "host": {
            "session_id": null,
            "session_name": null,
            "model": null,
            "reasoning": null,
            "active_skills": [],
            // The host reports an absent observation as explicit null so a
            // stale cached context usage is cleared rather than reused.
            "context_usage": null,
        },
    });
    assert_eq!(
        frames[1],
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "hook/run",
            "params": {
                "hook": "before_prompt",
                "payload": {"prompt": "Explain the contract"},
                "context": context,
            },
        })
    );
    assert_eq!(
        frames[2],
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "hook/run",
            "params": {
                "hook": "after_response",
                "payload": {"response": "The contract is bounded."},
                "context": context,
            },
        })
    );
    assert_eq!(
        frames[3],
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "shutdown",
            "params": {},
        })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn api_0_2_prompt_hooks_receive_owner_and_success_response_content() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let log_path = temp.path().join("hook-v02.jsonl");
    let fixture = temp.path().join("hook-v02.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
log=$1
request_id() { sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
read_request() { IFS= read -r line || exit 91; printf '%s\n' "$line" >> "$log"; }
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.2","tools":[],"commands":[],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}\n' "$id"
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"disposition":{"action":"continue"},"context":[],"notifications":[]}}\n' "$id"
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"disposition":{"action":"continue"},"context":[],"notifications":[]}}\n' "$id"
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();
    let mut manifest = ExtensionManifest::parse(
        r#"
name = "hook-v02"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "hook-v02.sh"
[contributes]
hooks = ["before_prompt", "after_response"]
"#,
    )
    .unwrap();
    manifest.entrypoint.args = vec![log_path.to_string_lossy().into_owned()];
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.resource_owner = Some("owner-v02".into());
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    extensions
        .compose_prompt("system", "user text".into())
        .await
        .unwrap();
    extensions.after_response("assistant text").await;
    extensions.shutdown().await;

    let frames = std::fs::read_to_string(log_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(frames[1]["params"]["hook"], "before_prompt");
    assert_eq!(frames[1]["params"]["payload"]["prompt"], "user text");
    assert_eq!(frames[2]["params"]["hook"], "after_response");
    assert_eq!(frames[2]["params"]["payload"]["response"], "assistant text");
    for frame in &frames[1..=2] {
        let owner = &frame["params"]["context"]["resource_owner"];
        assert_eq!(owner["session_id"], "owner-v02");
        assert_eq!(owner["process_generation"], 1);
        assert!(owner["extension_instance_id"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn api_0_2_command_parent_binds_owner_for_reverse_services() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("command-owner-v02.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
request_id() { sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
IFS= read -r initialize
id=$(printf '%s' "$initialize" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.2","tools":[],"commands":[{"name":"owned","description":"Exercise an owner-bound reverse service","usage":"/owned"}],"protocol":{"version":"0.2","features":["request_cancellation","content_parts","artifacts"],"limits":{"max_concurrent_requests":1}}}}\n' "$id"
IFS= read -r command
parent=$(printf '%s' "$command" | request_id)
printf '{"jsonrpc":"2.0","id":"artifact-child","method":"artifact/publish","params":{"parent_request_id":%s,"mime_type":"image/png","size":21,"sha256":"8c423f0980ff76637fe84cd6f9f8c63922b665c90e5608f37d995dca965f5fee","data":{"encoding":"base64","data":"iVBORw0KGgpvd25lci1maXh0dXJl"}}}\n' "$parent"
IFS= read -r artifact
case "$artifact" in
  *'"id":"artifact-child"'*'"artifact_id"'*) ;;
  *) exit 42 ;;
esac
printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"owner-bound artifact published","notifications":[],"context":[]}}\n' "$parent"
IFS= read -r shutdown
id=$(printf '%s' "$shutdown" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();
    let manifest = ExtensionManifest::parse(
        r#"
name = "command-owner-v02"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "command-owner-v02.sh"
[contributes]
commands = ["owned"]
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.resource_owner = Some("owner-command-v02".into());
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    let mut confirmations = RecordingConfirmationHandler::default();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_command_with_confirmation("owned", Vec::new(), &mut confirmations),
    )
    .await
    .expect("owner-bound artifact publication timed out")
    .unwrap()
    .unwrap();
    assert_eq!(output, "owner-bound artifact published");
    assert!(confirmations.calls.is_empty());
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn presentation_action_routes_duplicate_command_name_to_owning_process() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("duplicate-command.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
label=$1
request_id() { sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
IFS= read -r initialize
id=$(printf '%s' "$initialize" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.1","tools":[],"commands":[{"name":"shared","description":"Shared fixture command","usage":"/shared"}]}}\n' "$id"
IFS= read -r command
id=$(printf '%s' "$command" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"%s","notifications":[],"context":[]}}\n' "$id" "$label"
IFS= read -r shutdown
id=$(printf '%s' "$shutdown" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let mut processes = Vec::new();
    for name in ["first-owner", "second-owner"] {
        let mut manifest = ExtensionManifest::parse(&format!(
            r#"
name = {name:?}
version = "0.1.0"
api_version = "0.1"
[entrypoint]
command = "duplicate-command.sh"
[contributes]
commands = ["shared"]
"#
        ))
        .unwrap();
        manifest.entrypoint.args = vec![name.into()];
        processes.push(
            ExtensionProcess::start(
                DiscoveredExtension {
                    manifest,
                    manifest_path: temp.path().join(format!("{name}.toml")),
                    source: ExtensionSource::Explicit,
                    activation: octet_agent::extension_process::ExtensionActivation {
                        enabled: true,
                        trust: ExtensionTrust::Trusted,
                    },
                },
                ExtensionRuntimeConfig::new(temp.path()),
            )
            .await
            .unwrap(),
        );
    }

    let mut snapshot: ExtensionPresentationSnapshot = serde_json::from_str(include_str!(
        "../../../fixtures/extension-presentation.json"
    ))
    .unwrap();
    snapshot.actions.truncate(1);
    snapshot.actions[0].id = "route".into();
    snapshot.actions[0].command = "shared".into();
    snapshot.actions[0].arguments.clear();
    snapshot.actions[0].destructive = false;
    let generation = processes[1].health_snapshot().generation;
    let extension_instance_id = processes[1].extension_instance_id().to_owned();
    let mut extensions = ExecutableExtensions::default();
    extensions.processes = processes;
    extensions.presentations.insert(
        "second-owner".into(),
        ExtensionPresentationView {
            extension: "second-owner".into(),
            generation,
            extension_instance_id,
            resource_owner: None,
            snapshot,
        },
    );
    let mut confirmations = RecordingConfirmationHandler::default();
    let output = extensions
        .execute_presentation_action_with_confirmation("second-owner", "route", &mut confirmations)
        .await
        .unwrap();
    assert_eq!(output, "second-owner");
    assert!(confirmations.calls.is_empty());
    extensions.shutdown().await;
}
