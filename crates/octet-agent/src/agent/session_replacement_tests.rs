use super::*;

fn replacement_agent(directory: &std::path::Path, name: &str, root_tools: bool) -> Agent {
    let mut agent = active_tool_test_agent(
        directory,
        Session::create(directory.join(name)).unwrap(),
        ExtensionHost::new(),
    );
    let config = DelegationConfig::new(directory.join("delegation"));
    if root_tools {
        agent.enable_v2_delegation(config).unwrap();
    } else {
        agent.enable_v2_delegation_extension_only(config).unwrap();
    }
    agent
}

#[tokio::test]
async fn session_runtime_rebuild_keeps_surface_and_changes_owner_without_retargeting() {
    for root_tools in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = replacement_agent(directory.path(), "old.jsonl", root_tools);
        let old_team = agent.delegation_team_directory().unwrap().to_owned();
        let old_owner = agent.resource_owner.clone();
        let tools = agent.registered_tool_names();
        let system = agent.system_prompt().to_owned();
        agent.prompt_display_text = Some("old display".into());
        assert!(agent
            .replace_session_at_idle(
                Session::create(directory.path().join("refused.jsonl")).unwrap()
            )
            .is_err());
        assert_eq!(
            agent.detach_delegated_workers(Duration::from_secs(1)).await,
            0
        );
        let replacement = Session::create(directory.path().join("new.jsonl")).unwrap();
        let new_owner = replacement.resource_owner_key();
        let mut rebound = Vec::new();
        agent
            .replace_session_rebuilding_delegation_at_idle(replacement, |agent| {
                rebound.push(agent.resource_owner.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(rebound, std::slice::from_ref(&new_owner));
        assert_eq!(agent.resource_owner, new_owner);
        assert_ne!(agent.resource_owner, old_owner);
        assert_ne!(agent.delegation_team_directory().unwrap(), old_team);
        assert_eq!(agent.registered_tool_names(), tools);
        assert_eq!(agent.system_prompt(), system);
        assert!(agent.prompt_display_text.is_none());
        assert!(old_team.exists(), "durable old runtime must be retained");
    }
}

#[tokio::test]
async fn session_runtime_rebuild_rolls_back_owner_services_and_display_on_rebind_failure() {
    for root_tools in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = replacement_agent(directory.path(), "old.jsonl", root_tools);
        let old_team = agent.delegation_team_directory().unwrap().to_owned();
        let old_owner = agent.resource_owner.clone();
        let old_path = agent.session().path().to_owned();
        let old_bytes = std::fs::read(&old_path).unwrap();
        let tools = agent.registered_tool_names();
        agent.prompt_display_text = Some("retained display".into());
        assert_eq!(
            agent.detach_delegated_workers(Duration::from_secs(1)).await,
            0
        );
        let replacement = Session::create(directory.path().join("new.jsonl")).unwrap();
        let new_owner = replacement.resource_owner_key();
        let mut rebound = Vec::new();
        let result = agent.replace_session_rebuilding_delegation_at_idle(replacement, |agent| {
            rebound.push(agent.resource_owner.clone());
            if rebound.len() == 1 {
                Err(Box::new(AgentError::Delegation(
                    "fixture rebind failure".into(),
                )))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(rebound, [new_owner, old_owner.clone()]);
        assert_eq!(agent.resource_owner, old_owner);
        assert_eq!(agent.session().path(), old_path);
        assert_eq!(agent.delegation_team_directory().unwrap(), old_team);
        assert_eq!(agent.registered_tool_names(), tools);
        assert_eq!(
            agent.prompt_display_text.as_deref(),
            Some("retained display")
        );
        assert_eq!(std::fs::read(old_path).unwrap(), old_bytes);
        agent.reattach_delegated_workers().unwrap();
    }
}

#[tokio::test]
async fn session_runtime_rebuild_refuses_another_live_fleet_owner_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let mut agent = replacement_agent(directory.path(), "old.jsonl", false);
    let peer = replacement_agent(directory.path(), "owned.jsonl", false);
    let old_owner = agent.resource_owner.clone();
    let old_team = agent.delegation_team_directory().unwrap().to_owned();
    assert_eq!(
        agent.detach_delegated_workers(Duration::from_secs(1)).await,
        0
    );
    let replacement = Session::open(peer.session().path()).unwrap();
    assert!(agent
        .replace_session_rebuilding_delegation_at_idle(replacement, |_| {
            panic!("a lease refusal must precede service rebinding")
        })
        .is_err());
    assert_eq!(agent.resource_owner, old_owner);
    assert_eq!(agent.delegation_team_directory().unwrap(), old_team);
    agent.reattach_delegated_workers().unwrap();
}
