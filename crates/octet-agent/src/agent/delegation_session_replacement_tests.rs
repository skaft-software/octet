use super::*;
use crate::delegation::{
    AgentModelDescriptor, AgentModelResolver, AgentModelSelection, ResolvedAgentModel,
};
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

#[derive(Default)]
struct ReplacementModelResolver(AtomicUsize);

impl AgentModelResolver for ReplacementModelResolver {
    fn resolve(
        &self,
        _: &AgentModelSelection,
        _: &octet_ai::Model,
        _: &ReasoningConfig,
    ) -> Result<ResolvedAgentModel, String> {
        Err("unused fixture resolution".into())
    }

    fn models(&self, _: Option<&str>, _: usize) -> Result<Vec<AgentModelDescriptor>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn idle_session_replacement_preserves_delegation_configuration_and_surface() {
    for root_tools in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.jsonl");
        let mut agent = active_tool_test_agent(
            directory.path(),
            Session::create(&original).unwrap(),
            active_tool_test_extensions(&["read", "host_tool"]),
        );
        let resolver = Arc::new(ReplacementModelResolver::default());
        agent.set_delegation_model_resolver(resolver.clone());
        let mut config = DelegationConfig::new(directory.path().join("delegation")).proactive();
        config.limits.max_concurrent_agents = 3;
        config.limits.max_depth = 1;
        config.limits.max_total_agents = 5;
        agent
            .enable_v2_delegation_with_surface(config.clone(), root_tools)
            .unwrap();
        agent
            .set_active_tool_names(Some(BTreeSet::from(["read".into()])))
            .unwrap();
        let system = agent.system_prompt().to_owned();
        let registered = agent.registered_tool_names();
        let active = agent.extension_tool_snapshot();
        let mut team = agent.delegation_team_directory().unwrap().to_owned();
        for name in ["new.jsonl", "switched.jsonl", "original.jsonl"] {
            assert_eq!(
                agent
                    .detach_delegated_workers(Duration::from_secs(10))
                    .await,
                0
            );
            let path = directory.path().join(name);
            let session = if path == original {
                Session::open(&path).unwrap()
            } else {
                Session::create(&path).unwrap()
            };
            agent.replace_session_at_idle(session).unwrap();
            assert_eq!(agent.session().path(), path);
            assert_ne!(agent.delegation_team_directory().unwrap(), team);
            team = agent.delegation_team_directory().unwrap().to_owned();
            assert_eq!(agent.system_prompt(), system);
            assert_eq!(agent.registered_tool_names(), registered);
            assert_eq!(agent.extension_tool_snapshot(), active);
            let binding = agent.delegation.as_ref().unwrap();
            let (actual, actual_root_tools) = binding.root_configuration();
            assert_eq!(actual.session_directory, config.session_directory);
            assert_eq!(actual.mode, config.mode);
            assert_eq!(actual.limits.max_depth, 1);
            assert_eq!(actual.limits.max_concurrent_agents, 3);
            assert_eq!(actual.limits.max_total_agents, 5);
            assert_eq!(actual_root_tools, root_tools);
            assert_eq!(binding.base_system(), "system");
            // Exercise the resolver through the newly created runtime, rather
            // than just checking the root's retained resolver field.
            binding
                .extension_service("fixture", &agent.session_id, &agent.resource_owner)
                .unwrap()
                .models(&agent.resource_owner, None, 1)
                .unwrap();
        }
        assert_eq!(resolver.0.load(Ordering::SeqCst), 3);
    }
}

#[tokio::test]
async fn failed_delegation_rebind_keeps_the_outgoing_session_and_surface() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.jsonl");
    let mut agent = active_tool_test_agent(
        directory.path(),
        Session::create(&original).unwrap(),
        ExtensionHost::new(),
    );
    let storage = directory.path().join("delegation");
    agent
        .enable_v2_delegation(DelegationConfig::new(&storage))
        .unwrap();
    let team = agent.delegation_team_directory().unwrap().to_owned();
    let system = agent.system_prompt().to_owned();
    let tools = agent.registered_tool_names();
    let bytes = std::fs::read(&original).unwrap();
    assert_eq!(
        agent
            .detach_delegated_workers(Duration::from_secs(10))
            .await,
        0
    );
    // Keep the original team's descriptor-backed storage intact, but make
    // creating a replacement team fail at the configured directory boundary.
    std::fs::rename(&storage, directory.path().join("parked-delegation")).unwrap();
    std::fs::write(&storage, "not a directory").unwrap();
    let replacement = Session::create(directory.path().join("new.jsonl")).unwrap();
    assert!(agent.replace_session_at_idle(replacement).is_err());
    assert_eq!(agent.session().path(), original);
    assert_eq!(std::fs::read(&original).unwrap(), bytes);
    assert_eq!(agent.delegation_team_directory().unwrap(), team);
    assert_eq!(agent.system_prompt(), system);
    assert_eq!(agent.registered_tool_names(), tools);
}
