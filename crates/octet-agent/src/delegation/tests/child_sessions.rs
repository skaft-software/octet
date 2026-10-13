use super::*;

#[tokio::test]
async fn native_child_events_are_ordered_replayable_and_principal_scoped() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let binding = manager.root_binding();
    let service = binding
        .extension_service("a", "parent", "root-owner")
        .unwrap();
    let foreign = binding
        .extension_service("b", "parent", "root-owner")
        .unwrap();
    let (child, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    service
        .state
        .lock()
        .unwrap()
        .owners
        .entry("root-owner".into())
        .or_default()
        .owned_agents
        .insert(child.id.clone());
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&child.id).unwrap();
        record.extension_policy = Some(test_extension_policy());
        record.resource_owner = Some("child-session".into());
    }
    manager.observe_child_event(&child.id, &AgentEvent::TurnStarted);
    manager.observe_child_event(
        &child.id,
        &AgentEvent::OutputDelta {
            channel: crate::events::OutputChannel::Text,
            text: "native output".into(),
        },
    );
    let cancellation = crate::CancellationToken::default();
    let first = service
        .events("root-owner", &child.id, 0, Duration::ZERO, &cancellation)
        .await
        .unwrap();
    assert_eq!(first["agent_id"], child.id);
    assert_eq!(first["session_id"], "child-session");
    assert_eq!(first["next_sequence"], 2);
    assert_eq!(first["events"][0]["event"]["kind"], "turn_started");
    assert_eq!(first["events"][1]["event"]["text"], "native output");
    assert_eq!(
        first,
        service
            .events("root-owner", &child.id, 0, Duration::ZERO, &cancellation)
            .await
            .unwrap()
    );
    assert!(foreign
        .events("root-owner", &child.id, 0, Duration::ZERO, &cancellation)
        .await
        .is_err());
    assert!(service
        .events("foreign-owner", &child.id, 0, Duration::ZERO, &cancellation)
        .await
        .is_err());
    assert!(service
        .events("root-owner", &child.id, 3, Duration::ZERO, &cancellation)
        .await
        .is_err());
    cancellation.cancel();
    assert!(service
        .events(
            "root-owner",
            &child.id,
            2,
            Duration::from_secs(25),
            &cancellation
        )
        .await
        .unwrap_err()
        .contains("cancelled"));
}

#[tokio::test]
async fn child_stop_is_owned_shutdown_not_settlement_or_sibling_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let binding = manager.root_binding();
    let service = binding
        .extension_service("a", "parent", "root-owner")
        .unwrap();
    let foreign = binding
        .extension_service("b", "parent", "root-owner")
        .unwrap();
    let (child, mut commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let sibling = DurableFleetRecord {
        agent_id: "agent-2".into(),
        agent_path: "/root/sibling".into(),
        parent_id: ROOT_AGENT_ID.into(),
        depth: 1,
        task_name: "sibling".into(),
        session_path: directory.path().join("sibling.jsonl"),
        status: DelegatedAgentStatus::Running,
        ..DurableFleetRecord::default()
    };
    let (sibling, _sibling_commands) = insert_fixture_record(&manager, sibling, false, true, false);
    service
        .state
        .lock()
        .unwrap()
        .owners
        .entry("root-owner".into())
        .or_default()
        .owned_agents
        .insert(child.id.clone());
    assert!(foreign.stop("root-owner", &child.id).is_err());
    assert!(service.stop("root-owner", &sibling.id).is_err());
    service.shutdown_owner("foreign-owner");
    assert!(!manager.state.lock().unwrap().records[&child.id]
        .shutdown
        .is_cancelled());
    let result = service.stop("root-owner", &child.id).unwrap();
    assert_eq!(result["shutdown_requested"], true);
    assert!(commands.try_recv().is_ok());
    let state = manager.state.lock().unwrap();
    assert!(state.records[&child.id].shutdown.is_cancelled());
    assert_eq!(
        state.records[&child.id].status,
        DelegatedAgentStatus::Running
    );
    assert!(!state.records[&sibling.id].shutdown.is_cancelled());
}
