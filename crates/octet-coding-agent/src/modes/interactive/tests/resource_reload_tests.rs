//! Resource reload: which component problems survive a pass, when a reload may prompt
//! for consent, and what a forced watch pass does to extension state.
//! Separate because these probes drive the reload supervisor and the extension
//! presentation boundary rather than the interactive loop itself.

use super::*;

use super::support::*;

#[test]
fn explicit_resource_reload_clears_only_reinitialized_extension_problems() {
    use crate::reload::{ReloadComponent, ReloadSettings, ReloadSupervisor};
    let mut reload = ReloadSupervisor::new(ReloadSettings::default());
    let before: std::collections::BTreeMap<String, (String, u64)> =
        std::collections::BTreeMap::from([
            ("replaced".into(), ("old".into(), 1)),
            ("retained".into(), ("shared".into(), 1)),
            ("stopped".into(), ("old-stopped".into(), 1)),
        ]);
    let after = std::collections::BTreeMap::from([
        ("replaced".into(), ("new".into(), 1)),
        ("retained".into(), ("shared".into(), 1)),
    ]);
    let problem = || vec!["fixture failure".to_owned()];
    for name in before.keys() {
        reload.checked_problems(ReloadComponent::Extension(name.clone()), problem(), false);
    }
    reload.checked_problems(ReloadComponent::Host, problem(), false);
    remember_rebuilt_extensions(&mut reload, &before, &after);
    assert!(!reload
        .checked_problems(
            ReloadComponent::Extension("replaced".into()),
            problem(),
            false
        )
        .is_empty());
    for name in ["retained", "stopped"] {
        assert!(reload
            .checked_problems(ReloadComponent::Extension(name.into()), problem(), false)
            .is_empty());
    }
    assert!(reload
        .checked_problems(ReloadComponent::Host, problem(), false)
        .is_empty());
}

#[test]
fn automatic_reload_cannot_prompt_for_binary_or_worker_consent() {
    assert!(!HostPass::ResourcesOnly.may_prompt());
    assert!(!HostPass::Allowed {
        redirect_confirmed: false
    }
    .may_prompt());
    assert!(HostPass::Allowed {
        redirect_confirmed: true
    }
    .may_prompt());
}

#[test]
fn automatic_extension_reload_silences_success_but_preserves_each_event() {
    use crate::extensions::{ExtensionReloadReport, ExtensionRescanReport};
    use crate::reload::{ReloadSettings, ReloadSupervisor};
    let mut reload = ReloadSupervisor::new(ReloadSettings::default());
    let success = |generation| ExtensionReloadReport {
        processes: vec![(
            "fixture".into(),
            Ok(format!("reloaded fixture generation {generation}")),
        )],
        rescans: ExtensionRescanReport {
            checked: vec![("resource:fixture".into(), Vec::new())],
            details: vec![format!("rescanned fixture generation {generation}")],
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(extension_reload_notices(&mut reload, success(1), false).is_empty());
    assert!(extension_reload_notices(&mut reload, success(2), false).is_empty());
    assert_eq!(
        extension_reload_notices(&mut reload, success(3), true).len(),
        2
    );
    let failure = || ExtensionReloadReport {
        processes: vec![("fixture".into(), Err("fixture unavailable".into()))],
        events: vec![
            "discarded host request".into(),
            "discarded host request".into(),
        ],
        ..Default::default()
    };
    assert_eq!(
        extension_reload_notices(&mut reload, failure(), false).len(),
        3
    );
    assert_eq!(
        extension_reload_notices(&mut reload, failure(), false).len(),
        2
    );
    // No process check in this pass: the previous failure remains remembered.
    extension_reload_notices(&mut reload, ExtensionReloadReport::default(), false);
    assert_eq!(
        extension_reload_notices(&mut reload, failure(), false).len(),
        2
    );
    assert_eq!(
        extension_reload_notices(&mut reload, success(4), true).len(),
        2
    );
    assert_eq!(
        extension_reload_notices(&mut reload, failure(), false).len(),
        3
    );
    assert_eq!(
        extension_reload_notices(&mut reload, failure(), true).len(),
        3
    );
}

#[cfg(unix)]
#[tokio::test]
async fn resource_reload_refusal_preserves_worker_owner_and_control() {
    use crate::extensions::reload_lifecycle_test_support::fixture;
    use octet_agent::ExtensionPresentationState as State;

    for state in [
        State::Pending,
        State::Active,
        State::Running,
        State::Degraded,
    ] {
        for host in [
            HostPass::ResourcesOnly,
            HostPass::Allowed {
                redirect_confirmed: true,
            },
        ] {
            let (directory, mut app) = crate::compaction::tests::app_for_estimate();
            let (extensions, process, wire) = fixture(directory.path(), Some(state)).await;
            app.executable_extensions = extensions;
            let before = app.executable_extensions.presentation_views();
            let path = app.agent.session().path().to_owned();
            let session = std::fs::read(&path).unwrap();
            let system = app.system.clone();
            let skills = app.skills.clone();
            let generation = process.health_snapshot().generation;
            let mut shell = InteractiveShell::test_shell();
            let mut input = EventStream::from_stream(futures_util::stream::pending());
            let mut reexec = crate::reexec::ReexecController::capture().ok();
            let (mut app, plan, applied) = reload_resources_with_reexec(
                app,
                &mut shell,
                &mut input,
                reexec.as_mut(),
                host,
                &mut crate::reload::ReloadSupervisor::new(Default::default()),
            )
            .await
            .unwrap();

            assert!(!applied);
            assert!(plan.is_none());
            assert_eq!(app.system, system);
            assert!(Arc::ptr_eq(&skills, &app.skills), "the App was rebuilt");
            assert_eq!(std::fs::read(&path).unwrap(), session);
            assert_eq!(app.executable_extensions.presentation_views(), before);
            assert_eq!(active_subagent_workers(&app), 1);
            assert!(process.is_running());
            assert_eq!(process.health_snapshot().generation, generation);
            assert!(!std::fs::read_to_string(&wire)
                .unwrap_or_default()
                .contains("shutdown"));
            assert_eq!(
                app.executable_extensions
                    .execute_command_without_confirmation("worker-control", Vec::new(),)
                    .await
                    .unwrap()
                    .as_deref(),
                Some("control retained"),
            );
            app.executable_extensions.shutdown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_reload_refuses_host_worker_without_presentation_and_retains_control() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tool_turn(name: &str, arguments: serde_json::Value) -> String {
        [
            serde_json::json!({"type":"message_start", "message":{
                "id":"reload-worker", "usage":{"input_tokens":1,"output_tokens":0}}}),
            serde_json::json!({"type":"content_block_start", "index":0,
                "content_block":{"type":"tool_use", "id":name, "name":name}}),
            serde_json::json!({"type":"content_block_delta", "index":0,
                "delta":{"type":"input_json_delta", "partial_json":arguments.to_string()}}),
            serde_json::json!({"type":"content_block_stop", "index":0}),
            serde_json::json!({"type":"message_delta", "delta":{"stop_reason":"tool_use"},
                "usage":{"output_tokens":1}}),
            serde_json::json!({"type":"message_stop"}),
        ]
        .into_iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect()
    }

    // Exercise a real host-owned worker, with all inference restricted to
    // deterministic loopback responses and no subagents extension at all.
    let server = MockServer::start().await;
    let root_turns = Arc::new(AtomicUsize::new(0));
    let child_turns = Arc::new(AtomicUsize::new(0));
    let roots = root_turns.clone();
    let children = child_turns.clone();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let response = if body["system"]
                .to_string()
                .contains("You are /root/survivor")
            {
                let turn = children.fetch_add(1, Ordering::SeqCst) + 1;
                text_turn().replace("done", &format!("worker task {turn} complete"))
            } else {
                match roots.fetch_add(1, Ordering::SeqCst) {
                    0 => tool_turn(
                        "spawn_agent",
                        serde_json::json!({
                        "task_name":"survivor", "message":"complete the first task"}),
                    ),
                    1 | 4 => tool_turn("wait_agent", serde_json::json!({"timeout_ms":3000})),
                    2 | 5 => text_turn(),
                    3 => tool_turn(
                        "followup_task",
                        serde_json::json!({
                        "target":"/root/survivor", "message":"continue after refused reload"}),
                    ),
                    unexpected => panic!("unexpected root turn {unexpected}"),
                }
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(response)
        })
        .mount(&server)
        .await;
    let (directory, mut app) = fast_test_app(scripted_model(&server.uri()));
    // Native delegation tools require explicit host authority; only the
    // fixed local script above can request tools in this fixture.
    app.config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    app = rebuild_app(app, None, None, None, None).unwrap();
    app.agent
        .enable_v2_delegation(octet_agent::DelegationConfig::new(
            directory.path().join("delegation"),
        ))
        .unwrap();
    assert_eq!(
        app.agent.complete("start survivor").await.unwrap().text,
        "done"
    );
    for entry in app.agent.session().entries() {
        if let EntryValue::Message(octet_ai::Message::User(message)) = &entry.value {
            for part in &message.content {
                if let octet_ai::UserPart::ToolResult(result) = part {
                    assert!(!result.is_error, "{result:?}");
                }
            }
        }
    }
    assert_eq!(child_turns.load(Ordering::SeqCst), 1);
    // The first task has settled, but its live receiver must still block
    // destructive reloads, even though no presentation roster exists.
    assert_eq!(app.agent.active_delegated_worker_count(), 1);
    assert!(app.executable_extensions.presentation_views().is_empty());
    assert_eq!(app.executable_extensions.active_subagent_worker_count(), 0);
    let team = app.agent.delegation_team_directory().unwrap().to_path_buf();
    let child_path = team.join("0001-survivor.jsonl");
    let child_before = std::fs::read(&child_path).unwrap();
    let root_path = app.agent.session().path().to_owned();
    let root_before = std::fs::read(&root_path).unwrap();
    let owner = app.agent.session().resource_owner_key();
    let skills = app.skills.clone();
    let mut shell = InteractiveShell::test_shell();
    let mut input = EventStream::from_stream(futures_util::stream::pending());
    let mut reexec = crate::reexec::ReexecController::capture().ok();
    for host in [
        HostPass::ResourcesOnly,
        HostPass::Allowed {
            redirect_confirmed: true,
        },
    ] {
        let (next, plan, applied) = reload_resources_with_reexec(
            app,
            &mut shell,
            &mut input,
            reexec.as_mut(),
            host,
            &mut crate::reload::ReloadSupervisor::new(Default::default()),
        )
        .await
        .unwrap();
        app = next;
        assert!(!applied);
        assert!(plan.is_none());
        assert!(Arc::ptr_eq(&skills, &app.skills));
        assert_eq!(app.agent.session().resource_owner_key(), owner);
        assert_eq!(app.agent.active_delegated_worker_count(), 1);
        assert_eq!(std::fs::read(&root_path).unwrap(), root_before);
        assert_eq!(std::fs::read(&child_path).unwrap(), child_before);
    }
    let notice = shell.debug_snapshot();
    assert!(notice.contains("host worker tasks remain attached"));
    assert!(notice.contains("then exit and resume the session to replace the owning host"));
    assert!(notice.contains("The current application and workers are unchanged"));
    // Control still targets the original child task/session, not a worker
    // reconstructed from durable records after destroying the old owner.
    assert_eq!(
        app.agent.complete("continue survivor").await.unwrap().text,
        "done"
    );
    assert_eq!(child_turns.load(Ordering::SeqCst), 2);
    assert_eq!(root_turns.load(Ordering::SeqCst), 6);
    assert_eq!(app.agent.active_delegated_worker_count(), 1);
    assert_eq!(app.agent.delegation_team_directory(), Some(team.as_path()));
    assert!(std::fs::read_to_string(child_path)
        .unwrap()
        .contains("worker task 2 complete"));
    for entry in app.agent.session().entries() {
        if let EntryValue::Message(octet_ai::Message::User(message)) = &entry.value {
            for part in &message.content {
                if let octet_ai::UserPart::ToolResult(result) = part {
                    assert!(!result.is_error, "{result:?}");
                }
            }
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn forced_watch_reload_refusal_does_not_restart_extensions_or_claim_success() {
    use crate::extensions::reload_lifecycle_test_support::fixture;
    use octet_agent::ExtensionPresentationState as State;
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    let (extensions, process, wire) = fixture(directory.path(), Some(State::Running)).await;
    app.executable_extensions = extensions;
    let skills = app.skills.clone();
    let mut shell = InteractiveShell::test_shell();
    // A refusal must not poll raw input or enter any lifecycle worker.
    shell.cede_terminal_input();
    let mut input = EventStream::new().with_cede_flag(shell.terminal_input_parking());
    let mut pending_reexec = None;
    let mut supervisor = crate::reload::ReloadSupervisor::new(Default::default());
    let plan = supervisor.force();
    let mut app = tokio::time::timeout(
        Duration::from_secs(2),
        apply_live_reload_plan(
            app,
            &mut shell,
            &mut input,
            &mut supervisor,
            plan,
            None,
            &mut pending_reexec,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!supervisor.is_in_flight());
    assert!(pending_reexec.is_none());
    assert!(Arc::ptr_eq(&skills, &app.skills));
    assert!(process.is_running());
    assert!(!std::fs::read_to_string(wire)
        .unwrap_or_default()
        .contains("shutdown"));
    let visible = shell.debug_snapshot();
    assert!(visible.contains("reload refused"), "{visible}");
    assert!(
        !visible.contains("reloaded") && !visible.contains("reload applied"),
        "{visible}"
    );
    app.executable_extensions.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_and_model_session_rebuilds_restore_granted_terminal_input() {
    use crate::extensions::reload_lifecycle_test_support::{
        acquire_terminal, assert_revoked_before_shutdown, fixture,
    };

    for operation in [
        "resources",
        "model",
        "reasoning",
        "new",
        "resume",
        "extensions",
    ] {
        let (directory, mut app) = crate::compaction::tests::app_for_estimate();
        let (extensions, process, wire) = fixture(directory.path(), None).await;
        app.executable_extensions = extensions;
        let mut shell = InteractiveShell::test_shell();
        acquire_terminal(&mut app.executable_extensions, &mut shell, &process);
        let parking = shell.terminal_input_parking();
        let mut input = EventStream::from_stream(futures_util::stream::pending())
            .with_cede_flag(parking.clone());
        let mut app = match operation {
            "resources" => {
                let (next, applied) = reload_resources(app, &mut shell, &mut input).await.unwrap();
                assert!(applied);
                next
            }
            "extensions" => {
                app.executable_extensions.revoke_terminal_grant_for_shell(
                    &mut shell,
                    "the extensions are being reloaded",
                );
                app.executable_extensions.reload().await;
                app
            }
            _ => {
                let reconfig = match operation {
                    "model" => Reconfig::Model(app.model.spec.id.clone()),
                    "reasoning" => Reconfig::Thinking(ReasoningConfig::Off),
                    "new" => Reconfig::NewSession,
                    "resume" => Reconfig::Resume(app.agent.session().path().to_owned()),
                    _ => unreachable!(),
                };
                transition(app, &mut shell, &mut input, reconfig)
                    .await
                    .unwrap()
            }
        };
        assert!(
            !parking.load(std::sync::atomic::Ordering::SeqCst),
            "{operation}"
        );
        assert!(
            !app.executable_extensions.terminal_grant_is_active(),
            "{operation}"
        );
        assert_revoked_before_shutdown(&wire);
        // Test input uses the same restored ownership flag as the live stream.
        let key = theme_picker_key(KeyCode::Char('x'));
        let mut restored =
            EventStream::from_stream(tokio_stream::iter([key])).with_cede_flag(parking);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), restored.next())
                .await
                .unwrap()
                .is_some()
        );
        app.executable_extensions.shutdown().await;
    }
}
