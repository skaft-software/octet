use super::*;

const FLEET_TOOL_COUNT: usize = 117;
const FLEET_SCHEMA_BYTES: usize = 161_125;

struct FleetTool(ToolDef);

#[async_trait::async_trait]
impl Tool for FleetTool {
    fn definition(&self) -> ToolDef {
        self.0.clone()
    }

    async fn execute(
        &self,
        _: serde_json::Value,
        _: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        panic!("the schema admission fixture must not execute tools")
    }
}

fn fleet_definitions() -> Vec<ToolDef> {
    let mut tools = (0..FLEET_TOOL_COUNT)
        .map(|index| ToolDef {
            async_execution: false,
            name: format!("fleet_{index:03}"),
            description: "Synthetic fleet tool 🙂 ".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"payload": {"type": "string", "enum": ["a", "b"]}},
                "required": ["payload"],
                "additionalProperties": false
            }),
            constrained_sampling: None,
        })
        .collect::<Vec<_>>();
    let padding = FLEET_SCHEMA_BYTES - tool_schema_bytes(&tools);
    for (index, tool) in tools.iter_mut().enumerate() {
        tool.description.push_str(
            &"x".repeat(
                padding / FLEET_TOOL_COUNT + usize::from(index < padding % FLEET_TOOL_COUNT),
            ),
        );
    }
    assert_eq!(tool_schema_bytes(&tools), FLEET_SCHEMA_BYTES);
    tools
}

fn fleet_agent(directory: &std::path::Path, tools: &[ToolDef]) -> Agent {
    let mut extensions = ExtensionHost::new();
    for tool in tools {
        extensions.tool(FleetTool(tool.clone()));
    }
    active_tool_test_agent(
        directory,
        Session::create(directory.join("fleet.jsonl")).unwrap(),
        extensions,
    )
}

fn capture_fleet(agent: &Agent, tools: Vec<ToolDef>) -> Arc<ToolBudgetTransport> {
    let transport = Arc::new(ToolBudgetTransport {
        calls: std::sync::atomic::AtomicUsize::new(0),
        expected_tools: tools,
    });
    agent
        .client
        .register_host_stream_transport(agent.model.endpoint.id.clone(), transport.clone());
    transport
}

#[tokio::test]
async fn default_and_exact_schema_quota_admit_the_entire_117_tool_fleet_unchanged() {
    for quota in [None, Some(FLEET_SCHEMA_BYTES)] {
        let directory = tempfile::tempdir().unwrap();
        let tools = fleet_definitions();
        let mut agent = fleet_agent(directory.path(), &tools);
        if let Some(quota) = quota {
            agent.set_tool_schema_budget_bytes(quota);
        }
        let transport = capture_fleet(&agent, tools);
        assert!(matches!(
            agent
                .complete("keep every definition")
                .await
                .unwrap()
                .reason,
            FinishReason::Completed
        ));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn explicit_schema_quota_refuses_the_fleet_before_prompt_persistence_or_provider_io() {
    let directory = tempfile::tempdir().unwrap();
    let tools = fleet_definitions();
    let mut agent = fleet_agent(directory.path(), &tools);
    let transport = capture_fleet(&agent, tools);
    agent.set_tool_schema_budget_bytes(128 * 1024);
    assert!(matches!(
        agent.prompt("retryable fleet draft").await,
        Err(AgentError::ToolSchemaBudgetExceeded {
            actual_bytes: FLEET_SCHEMA_BYTES,
            tool_count: FLEET_TOOL_COUNT,
            max_bytes: 131_072,
        })
    ));
    assert!(agent.session().entries().is_empty());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn default_schema_policy_still_refuses_a_fleet_that_exceeds_the_model_context() {
    let directory = tempfile::tempdir().unwrap();
    let tools = fleet_definitions();
    let mut agent = fleet_agent(directory.path(), &tools);
    Arc::make_mut(&mut agent.model.spec).limits.context_window = 32_768;
    agent
        .set_compaction_token_mode(AgentCompactionMode::Disabled, 1.0, 1)
        .unwrap();
    let transport = capture_fleet(&agent, tools.clone());
    assert!(matches!(
        agent.complete("irreducible request").await.unwrap_err(),
        AgentError::ContextExceeded { estimate, budget } if estimate > budget
    ));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        serde_json::to_value(agent.registered_tool_definitions()).unwrap(),
        serde_json::to_value(tools).unwrap()
    );
}

#[tokio::test]
async fn dynamic_schema_growth_across_the_former_default_cliff_preserves_every_definition() {
    for quota in [None, Some(128 * 1024)] {
        let directory = tempfile::tempdir().unwrap();
        let tools = fleet_definitions();
        let mut agent = fleet_agent(directory.path(), &[]);
        if let Some(quota) = quota {
            agent.set_tool_schema_budget_bytes(quota);
        }
        let mut publisher = agent.extensions.clone();
        let transport = capture_fleet(&agent, tools.clone());
        // prompt() freezes an empty preflight surface. A real registry revision
        // published before the next model turn must receive the same policy.
        let mut run = agent.prompt("late fleet registration").await.unwrap();
        let _registration = publisher
            .dynamic_tools(
                "fleet-owner",
                tools
                    .into_iter()
                    .map(|tool| Arc::new(FleetTool(tool)) as Arc<dyn Tool>)
                    .collect(),
            )
            .unwrap();
        let mut terminal = None;
        while let Some(event) = run.next().await {
            if let AgentEvent::RunFinished { reason, .. } = event {
                terminal = Some(reason);
            }
        }
        if quota.is_some() {
            assert!(matches!(
                terminal,
                Some(FinishReason::Failed(AgentError::ToolSchemaBudgetExceeded {
                    actual_bytes: FLEET_SCHEMA_BYTES,
                    tool_count: FLEET_TOOL_COUNT,
                    max_bytes: 131_072,
                }))
            ));
            assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        } else {
            assert!(matches!(terminal, Some(FinishReason::Completed)));
            assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        }
    }
}
