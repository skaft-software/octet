//! The snapshots the host builds for the shell: chrome, prompts, and queues.
//!
//! Covers `session_manager` snapshot composition from cached host state, the
//! admitted follow-up queue depth reported as pending messages, `system_prompt`
//! snapshot refusing rather than silently truncating over-bound text, the
//! header/footer status surfaces that reach the shell, and the character-boundary
//! truncation of bounded surface text.

use super::*;

#[test]
fn session_manager_snapshot_is_composed_from_cached_host_state() {
    let mut extensions = ExecutableExtensions::default();
    extensions.session_id = Some("session-1".into());
    extensions.workspace = PathBuf::from("/workspace/root");
    {
        let mut state = extensions.host_state.lock().unwrap();
        state.session_name = Some("Refactor".into());
        state.model = Some("gpt-test".into());
        state.reasoning = Some(serde_json::Value::String("High".into()));
        state.active_skills = vec![octet_agent::extension_process::ExtensionActiveSkill {
            id: "skill-1".into(),
            name: "Skill One".into(),
            version: None,
        }];
    }
    match extensions.context_session_manager_outcome() {
        ExtensionRequestOutcome::Ok(value) => {
            assert_eq!(value["session_id"], "session-1");
            assert_eq!(value["name"], "Refactor");
            assert_eq!(value["model"], "gpt-test");
            assert_eq!(value["reasoning"], "High");
            assert_eq!(value["cwd"], "/workspace/root");
            assert_eq!(value["active_skills"][0]["id"], "skill-1");
            assert_eq!(value["active_skills"][0]["name"], "Skill One");
        }
        other => panic!("expected a session snapshot, got {other:?}"),
    }

    // No foreground session is a typed refusal, never a fabricated id.
    let none = ExecutableExtensions::default();
    match none.context_session_manager_outcome() {
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::InvalidRequest, _) => {}
        other => panic!("expected an invalid_request refusal, got {other:?}"),
    }
}

#[test]
fn pending_messages_counts_the_admitted_follow_up_queue() {
    let mut shell = InteractiveShell::test_shell();
    match ExecutableExtensions::context_pending_messages_outcome(&shell) {
        ExtensionRequestOutcome::Ok(value) => assert_eq!(value["pending"], 0),
        other => panic!("expected an empty queue, got {other:?}"),
    }
    shell.queue_follow_up(ComposedInput::from_text("first".to_string()));
    shell.queue_follow_up(ComposedInput::from_text("second".to_string()));
    match ExecutableExtensions::context_pending_messages_outcome(&shell) {
        ExtensionRequestOutcome::Ok(value) => assert_eq!(value["pending"], 2),
        other => panic!("expected two pending messages, got {other:?}"),
    }
}

#[tokio::test]
async fn system_prompt_snapshot_refuses_rather_than_truncates_over_bound_text() {
    use octet_agent::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig};
    use octet_ai::{AiClient, CacheRetention};

    let temp = tempfile::tempdir().unwrap();
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: "composed system prompt".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: None,
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    match ExecutableExtensions::context_system_prompt_outcome(&agent) {
        ExtensionRequestOutcome::Ok(value) => {
            assert_eq!(value["text"], "composed system prompt");
        }
        other => panic!("expected the composed prompt, got {other:?}"),
    }

    let bound = octet_agent::extension_process::MAX_EXTENSION_SYSTEM_PROMPT_BYTES;
    agent.set_system_prompt("x".repeat(bound + 1));
    match ExecutableExtensions::context_system_prompt_outcome(&agent) {
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::BoundsExceeded, _) => {}
        other => panic!("expected a bounds refusal, got {other:?}"),
    }
}

#[test]
fn header_and_footer_status_surfaces_project_into_shell_chrome() {
    let mut extensions = ExecutableExtensions::default();
    extensions.record_status_surface(
        "fixture".into(),
        "instance-a".into(),
        3,
        ExtensionStatusContribution {
            surface: ExtensionUiSurface::Header,
            text: "HEADER".into(),
            style_role: Some("extension.pi.accent".into()),
            priority: 1,
        },
    );
    extensions.record_status_surface(
        "fixture".into(),
        "instance-a".into(),
        3,
        ExtensionStatusContribution {
            surface: ExtensionUiSurface::Footer,
            text: "FOOTER".into(),
            style_role: None,
            priority: 0,
        },
    );
    let projected = ExecutableExtensions::project_semantic_ui(&extensions.semantic_ui);
    assert_eq!(projected.header.len(), 1);
    assert_eq!(projected.header[0].text, "HEADER");
    assert_eq!(
        projected.header[0].style_role.as_deref(),
        Some("extension.pi.accent")
    );
    assert_eq!(projected.footer.len(), 1);
    assert_eq!(projected.footer[0].text, "FOOTER");

    // An empty text clears the surface rather than leaving a stale row.
    extensions.record_status_surface(
        "fixture".into(),
        "instance-a".into(),
        3,
        ExtensionStatusContribution {
            surface: ExtensionUiSurface::Header,
            text: String::new(),
            style_role: None,
            priority: 0,
        },
    );
    let projected = ExecutableExtensions::project_semantic_ui(&extensions.semantic_ui);
    assert!(projected.header.is_empty());
    assert_eq!(projected.footer.len(), 1);
}

#[test]
fn bounded_surface_text_truncates_on_a_character_boundary() {
    assert_eq!(bounded_surface_text("plain"), "plain");
    let long = "é".repeat(5000);
    let bounded = bounded_surface_text(&long);
    assert!(bounded.len() <= 8 * 1024);
    assert!(bounded.is_char_boundary(bounded.len()));
    assert_eq!(bounded.chars().count(), 4096);
}

#[test]
fn pending_messages_snapshot_tracks_real_shell_queues_and_clears_after_delivery() {
    let mut extensions = ExecutableExtensions::default();
    let mut shell = InteractiveShell::test_shell();
    assert!(extensions
        .host_state
        .lock()
        .unwrap()
        .has_pending_messages
        .is_none());
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(false)
    );
    shell.queue_follow_up(ComposedInput::from_text("follow-up".into()));
    extensions.drain_events_for_shell(&mut shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(true)
    );
    shell.settle_queued_follow_ups(true);
    assert!(shell.take_ready_follow_up(false).is_some());
    extensions.drain_events_for_shell(&mut shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(false)
    );
    let (prepared, receipt) = octet_agent::PreparedSteering::new("steer");
    shell.queue_retractable_steering(receipt.clone(), "steer".into(), "steer".into(), vec![]);
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(true)
    );
    assert!(receipt.try_retract());
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(false)
    );
    drop(prepared);
    use octet_agent::extension_process::ExtensionMessageDelivery as Delivery;
    for (delivery, context_only, expected) in [
        (Delivery::NextTurn, false, false),
        (Delivery::Steer, true, false),
        (Delivery::Steer, false, true),
        (Delivery::FollowUp, false, true),
    ] {
        shell.queue_extension_message(crate::tui::view::PendingExtensionMessage {
            input: ComposedInput::from_text("injected".into()),
            delivery,
            wake: true,
            context_only,
        });
        extensions.refresh_frontend_pending_messages(&shell);
        assert_eq!(
            extensions.host_state.lock().unwrap().has_pending_messages,
            Some(expected)
        );
        assert_eq!(shell.take_extension_messages().len(), 1);
    }
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(false)
    );
}

#[tokio::test]
async fn pending_messages_snapshot_includes_native_run_queue_and_fences_its_owner() {
    use octet_agent::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig};
    use octet_ai::{AiClient, CacheRetention};
    let temp = tempfile::tempdir().unwrap();
    let session = Session::create(temp.path().join("queue.jsonl")).unwrap();
    let owner = session.resource_owner_key();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: "queue fixture".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: None,
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    // This unpolled run never invokes a provider.
    let run = agent.prompt("initial").await.unwrap();
    let control = run.control();
    let mut extensions = ExecutableExtensions::default();
    extensions.resource_owner = Some(owner.clone());
    extensions.bind_pending_message_control(&control);
    let shell = InteractiveShell::test_shell();
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(false)
    );
    control
        .try_follow_up("native follow-up not in shell queue")
        .unwrap();
    assert_eq!(shell.queued_follow_up_len(), 0);
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(true)
    );
    extensions.resource_owner = Some("replacement-owner".into());
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(false)
    );
    extensions.resource_owner = Some(owner);
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(true)
    );
    drop(run);
    extensions.refresh_frontend_pending_messages(&shell);
    assert_eq!(
        extensions.host_state.lock().unwrap().has_pending_messages,
        Some(false)
    );
}
