use super::*;

#[test]
fn session_runtime_rebuild_refuses_live_workers_before_creating_a_team() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (identity, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut(&identity.id)
        .unwrap()
        .live_task = true;
    let before = std::fs::read_dir(directory.path()).unwrap().count();
    assert!(manager
        .root_binding()
        .rebuild_for_session(&directory.path().join("new.jsonl"))
        .is_err());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), before);
    assert_eq!(manager.root_binding().active_worker_count(), 1);
}

#[test]
fn session_runtime_rebuild_mirrors_late_usage_and_uncertainty_only_to_old_owner_once() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (identity, _commands) = insert_test_record(
        &manager,
        DelegatedAgentStatus::Completed {
            output: "settled".into(),
        },
    );
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&identity.id).unwrap();
        record.live_task = false;
        record.extension_principal = Some("fixture-extension".into());
        record.usage.input_tokens = 7;
        record.usage.output_tokens = 3;
        record.usage.total_tokens = 10;
        record.turn_count = 1;
        record.usage_uncertain = true;
        record.usage_exposure = None;
    }
    let old_path = directory.path().join("old-root.jsonl");
    let mut root = Agent::new(AgentConfig {
        client: manager.template.client.clone(),
        model: manager.template.model(),
        session: Session::create(&old_path).unwrap(),
        system: "test".into(),
        sandbox: manager.template.sandbox.clone(),
        effect_broker: manager.template.effect_broker.clone(),
        extensions: manager.template.extensions.clone(),
        max_turns: Some(4),
        reasoning: manager.template.reasoning.read().unwrap().clone(),
        reasoning_mode: manager.template.reasoning_mode,
        cache_retention: manager.template.cache_retention,
        session_id: None,
    })
    .unwrap();
    root.set_delegation_binding(manager.root_binding()).unwrap();
    let mut calls = 0;
    assert!(root
        .replace_session_rebuilding_delegation_at_idle(
            Session::create(directory.path().join("failed-root.jsonl")).unwrap(),
            |_| {
                calls += 1;
                if calls == 1 {
                    Err(Box::new(AgentError::Delegation(
                        "fixture rebind failure".into(),
                    )))
                } else {
                    Ok(())
                }
            }
        )
        .is_err());
    assert_eq!(root.session().path(), old_path);
    assert_eq!(root.session().usage_records().len(), 1);
    assert_eq!(root.session().usage_uncertainty_records().len(), 1);
    root.replace_session_rebuilding_delegation_at_idle(
        Session::create(directory.path().join("new-root.jsonl")).unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    assert!(root.session().usage_records().is_empty());
    assert!(!root.session().has_uncertain_usage());
    let old = Session::open(&old_path).unwrap();
    assert_eq!(old.usage_records().len(), 1);
    assert_eq!(old.usage_records()[0].usage.total_tokens, 10);
    assert_eq!(old.usage_uncertainty_records().len(), 1);
    assert!(old.has_uncertain_usage());
}

#[test]
fn session_runtime_rebuild_discards_a_replacement_that_never_became_current() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let teams = || {
        let mut names: Vec<String> = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("team-"))
            .collect();
        names.sort();
        names
    };
    let old_path = directory.path().join("old-root.jsonl");
    let mut root = Agent::new(AgentConfig {
        client: manager.template.client.clone(),
        model: manager.template.model(),
        session: Session::create(&old_path).unwrap(),
        system: "test".into(),
        sandbox: manager.template.sandbox.clone(),
        effect_broker: manager.template.effect_broker.clone(),
        extensions: manager.template.extensions.clone(),
        max_turns: Some(4),
        reasoning: manager.template.reasoning.read().unwrap().clone(),
        reasoning_mode: manager.template.reasoning_mode,
        cache_retention: manager.template.cache_retention,
        session_id: None,
    })
    .unwrap();
    root.set_delegation_binding(manager.root_binding()).unwrap();
    // A replacement runtime is created through the production activation path,
    // so it owns a durable `team-*` directory of its own.
    root.replace_session_rebuilding_delegation_at_idle(
        Session::create(directory.path().join("first-root.jsonl")).unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    let current = teams();
    assert_eq!(current.len(), 1, "{current:?}");
    // A rebuild that cannot rebind rolls the session back and must remove the
    // runtime it created, instead of leaving an orphaned team directory and
    // provenance journal behind for every failed switch.
    let error = root
        .replace_session_rebuilding_delegation_at_idle(
            Session::create(directory.path().join("failed-root.jsonl")).unwrap(),
            |_| {
                Err(Box::new(AgentError::Delegation(
                    "fixture rebind failure".into(),
                )))
            },
        )
        .expect_err("the refused rebind must roll back");
    assert!(
        error.to_string().contains("fixture rebind failure"),
        "{error}"
    );
    assert_eq!(
        root.session().path(),
        directory.path().join("first-root.jsonl")
    );
    assert_eq!(teams(), current);
    for name in &current {
        let journal = directory.path().join(name).join("provenance.jsonl");
        assert!(journal.exists(), "{}", journal.display());
    }
}

#[test]
fn session_runtime_rebuild_keeps_root_collaboration_tools_out_of_the_child_template() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager_with_core_tools(directory.path());
    let rebuilt = manager
        .root_binding()
        .rebuild_for_session(&directory.path().join("new-root.jsonl"))
        .unwrap();
    let (_, template_tools) = rebuilt.manager.template.extensions.tool_snapshot();
    let template_names: BTreeSet<String> = template_tools
        .iter()
        .map(|tool| tool.definition().name)
        .collect();
    assert!(
        !template_names.is_empty(),
        "children still inherit the parent's standard tools"
    );
    for name in COLLABORATION_TOOL_NAMES {
        assert!(
            !template_names.contains(name),
            "{name} is a root surface and must never enter a child template"
        );
    }
    // The root surface is rebound to the new runtime instead of being copied
    // into it, so a child cannot reach a retired manager's collaboration tools.
    let rebound: BTreeSet<String> = rebuilt
        .root_tools()
        .iter()
        .map(|tool| tool.definition().name)
        .collect();
    assert_eq!(rebound.len(), COLLABORATION_TOOL_NAMES.len());
    assert!(COLLABORATION_TOOL_NAMES
        .iter()
        .all(|name| rebound.contains(*name)));
}
