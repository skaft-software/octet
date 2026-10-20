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

    // Settled and detached workers (a reached turn limit, a parked session)
    // are presented as active or degraded: they never block a reload.
    for state in [State::Active, State::Degraded] {
        let (directory, mut app) = crate::compaction::tests::app_for_estimate();
        let (extensions, _process, _wire) = fixture(directory.path(), Some(state)).await;
        app.executable_extensions = extensions;
        assert_eq!(busy_subagent_workers(&app), 0, "{state:?}");
        app.executable_extensions.shutdown().await;
    }
    for state in [State::Pending, State::Running] {
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
            assert_eq!(busy_subagent_workers(&app), 1);
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

fn worker_tool_turn(name: &str, arguments: serde_json::Value) -> String {
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

fn assert_no_tool_errors(app: &App) {
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

/// Native delegation on a fixture without the subagents extension. Production
/// enables it during bootstrap; the rebuilt fixture re-enables it the same way.
fn enable_native_delegation(app: &mut App, directory: &std::path::Path) {
    app.agent
        .enable_v2_delegation(octet_agent::DelegationConfig::new(
            directory.join("delegation"),
        ))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_reload_parks_a_settled_host_worker_and_keeps_it_resumable() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
                    0 => worker_tool_turn(
                        "spawn_agent",
                        serde_json::json!({
                        "task_name":"survivor", "message":"complete the first task"}),
                    ),
                    1 | 4 => worker_tool_turn("wait_agent", serde_json::json!({"timeout_ms":3000})),
                    2 | 5 => text_turn(),
                    3 => worker_tool_turn(
                        "followup_task",
                        serde_json::json!({
                        "target":"/root/survivor", "message":"continue after the reload"}),
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
    enable_native_delegation(&mut app, directory.path());
    assert_eq!(
        app.agent.complete("start survivor").await.unwrap().text,
        "done"
    );
    assert_no_tool_errors(&app);
    assert_eq!(child_turns.load(Ordering::SeqCst), 1);
    // The first task settled. Its idle task still waits for a follow-up, but
    // it runs nothing, so it must not block the reload.
    assert_eq!(app.agent.active_delegated_worker_count(), 1);
    assert_eq!(app.agent.busy_delegated_worker_count(), 0);
    assert_eq!(busy_subagent_workers(&app), 0);
    let team = app.agent.delegation_team_directory().unwrap().to_path_buf();
    let child_path = team.join("0001-survivor.jsonl");
    let owner = app.agent.session().resource_owner_key();
    let skills = app.skills.clone();
    let mut shell = InteractiveShell::test_shell();
    let mut input = EventStream::from_stream(futures_util::stream::pending());
    let mut reexec = crate::reexec::ReexecController::capture().ok();
    let (next, plan, applied) = reload_resources_with_reexec(
        app,
        &mut shell,
        &mut input,
        reexec.as_mut(),
        HostPass::ResourcesOnly,
        &mut crate::reload::ReloadSupervisor::new(Default::default()),
    )
    .await
    .unwrap();
    app = next;
    assert!(applied, "{}", shell.debug_snapshot());
    assert!(plan.is_none());
    assert!(!Arc::ptr_eq(&skills, &app.skills), "the App was rebuilt");
    assert_eq!(app.agent.session().resource_owner_key(), owner);
    let visible = shell.debug_snapshot();
    assert!(!visible.contains("reload refused"), "{visible}");

    // The rebuilt owner restores the parked worker from its durable roster
    // and an explicit follow-up resumes the same child session.
    enable_native_delegation(&mut app, directory.path());
    assert_eq!(
        app.agent.complete("continue survivor").await.unwrap().text,
        "done"
    );
    assert_no_tool_errors(&app);
    assert_eq!(child_turns.load(Ordering::SeqCst), 2);
    assert_eq!(root_turns.load(Ordering::SeqCst), 6);
    let child = std::fs::read_to_string(child_path).unwrap();
    assert!(child.contains("worker task 1 complete"));
    assert!(child.contains("worker task 2 complete"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_host_worker_refuses_plain_reload_and_force_detaches_it() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let root_turns = Arc::new(AtomicUsize::new(0));
    let roots = root_turns.clone();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let response =
                ResponseTemplate::new(200).insert_header("content-type", "text/event-stream");
            if body["system"].to_string().contains("You are /root/runner") {
                // The worker's provider call never answers in time: it is
                // still running when the reload is requested.
                return response
                    .set_body_string(text_turn())
                    .set_delay(Duration::from_secs(120));
            }
            let turn = match roots.fetch_add(1, Ordering::SeqCst) {
                0 => worker_tool_turn(
                    "spawn_agent",
                    serde_json::json!({"task_name":"runner", "message":"keep working"}),
                ),
                1 => text_turn(),
                unexpected => panic!("unexpected root turn {unexpected}"),
            };
            response.set_body_string(turn)
        })
        .mount(&server)
        .await;
    let (directory, mut app) = fast_test_app(scripted_model(&server.uri()));
    app.config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    app = rebuild_app(app, None, None, None, None).unwrap();
    enable_native_delegation(&mut app, directory.path());
    assert_eq!(
        app.agent.complete("start runner").await.unwrap().text,
        "done"
    );
    assert_no_tool_errors(&app);
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.agent.busy_delegated_worker_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let skills = app.skills.clone();
    let mut shell = InteractiveShell::test_shell();
    // The live-reload pass must not poll raw terminal input in a test.
    shell.cede_terminal_input();
    let mut input = EventStream::new().with_cede_flag(shell.terminal_input_parking());

    // Plain reload refuses a running worker and names the explicit opt-in.
    let (next, applied) = reload_resources(app, &mut shell, &mut input).await.unwrap();
    app = next;
    assert!(!applied);
    assert!(Arc::ptr_eq(&skills, &app.skills));
    assert_eq!(app.agent.busy_delegated_worker_count(), 1);
    let visible = shell.debug_snapshot();
    assert!(
        visible.contains("reload refused: 1 subagent worker(s)"),
        "{visible}"
    );
    assert!(visible.contains("/reload --force"), "{visible}");

    // `/reload --force` detaches the worker instead of refusing.
    let mut supervisor = crate::reload::ReloadSupervisor::new(Default::default());
    let plan = supervisor.force();
    let mut pending_reexec = None;
    let mut app = tokio::time::timeout(
        Duration::from_secs(30),
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
    assert!(pending_reexec.is_none());
    assert!(!supervisor.is_in_flight());
    assert!(
        !Arc::ptr_eq(&skills, &app.skills),
        "the forced pass rebuilt the App"
    );
    let visible = shell.debug_snapshot();
    assert!(
        visible.contains("detached 1 running subagent worker(s)"),
        "{visible}"
    );
    // The detached worker's durable record survives for the rebuilt owner.
    enable_native_delegation(&mut app, directory.path());
    let roster = std::fs::read_dir(directory.path().join("delegation"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("fleet-"))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect::<String>();
    assert!(roster.contains("\"task_name\":\"runner\""), "{roster}");
    assert!(roster.contains("\"state\":\"detached\""), "{roster}");
    assert_eq!(root_turns.load(Ordering::SeqCst), 2);
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

    for operation in ["resources", "model", "new", "resume", "extensions"] {
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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_mode_idle_thinking_preserves_the_existing_terminal_grant_and_process() {
    use crate::extensions::reload_lifecycle_test_support::{
        acquire_terminal, assert_revoked_before_shutdown, fixture,
    };
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    let (extensions, process, wire) = fixture(directory.path(), None).await;
    app.executable_extensions = extensions;
    let mut shell = InteractiveShell::test_shell();
    acquire_terminal(&mut app.executable_extensions, &mut shell, &process);
    let parking = shell.terminal_input_parking();
    let mut input =
        EventStream::from_stream(futures_util::stream::pending()).with_cede_flag(parking.clone());
    let mut app = transition(
        app,
        &mut shell,
        &mut input,
        Reconfig::Thinking(ReasoningConfig::Off),
    )
    .await
    .unwrap();
    assert!(parking.load(std::sync::atomic::Ordering::SeqCst));
    assert!(app.executable_extensions.terminal_grant_is_active());
    assert!(
        process.is_running(),
        "the same executable instance must survive"
    );
    assert!(!std::fs::read_to_string(&wire)
        .unwrap_or_default()
        .contains("shutdown"));
    app.executable_extensions
        .revoke_terminal_grant_for_shell(&mut shell, "test shutdown");
    app.executable_extensions.shutdown().await;
    assert_revoked_before_shutdown(&wire);
}
