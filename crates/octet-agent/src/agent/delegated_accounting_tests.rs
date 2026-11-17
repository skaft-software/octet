use super::*;
use crate::delegation::{
    AgentModelDescriptor, AgentModelResolver, AgentModelSelection, ExtensionAgentSessionPolicy,
    ExtensionDelegationSpawnRequest, ResolvedAgentModel,
};
use octet_ai::{EndpointId, ModelCatalog, ModelId};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn legacy_delegated_labels_dedupe_by_child_across_resume_and_model_changes() {
    for legacy_model in [None, Some(ModelId("parent-model".into()))] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.jsonl");
        let mut session = Session::create(&path).unwrap();
        let mut snapshot = DelegatedUsage {
            agent_id: "agent-1".into(),
            turn_count: 1,
            tool_call_count: 0,
            endpoint: None,
            model: legacy_model,
            usage: Usage {
                input_tokens: 10,
                total_tokens: 10,
                ..Default::default()
            },
            cost: Some(Cost {
                input: 7,
                total: 7,
                ..Default::default()
            }),
        };
        record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
        drop(session);
        let mut session = Session::open(&path).unwrap();
        snapshot.endpoint = Some(EndpointId("openai-codex".into()));
        snapshot.model = Some(ModelId("codex/gpt-6.1-sol".into()));
        record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
        assert_eq!(session.usage_records().len(), 1);
        snapshot.model = Some(ModelId("codex/gpt-6-astra".into()));
        snapshot.turn_count = 2;
        snapshot.usage.input_tokens = 20;
        snapshot.usage.total_tokens = 20;
        snapshot.cost = Some(Cost {
            input: 20,
            total: 20,
            ..Default::default()
        });
        record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
        record_delegated_usage_once(&mut session, snapshot).unwrap();
        assert_eq!(session.usage_records().len(), 2);
        assert_eq!(session.usage_records()[1].usage.total_tokens, 10);
        assert_eq!(session.usage_records()[1].cost.unwrap().total, 13);
        assert_eq!(session.total_cost_microdollars(), 20);
    }
}

struct WorkerModel(Model);

impl AgentModelResolver for WorkerModel {
    fn resolve(
        &self,
        selection: &AgentModelSelection,
        _: &Model,
        _: &ReasoningConfig,
    ) -> Result<ResolvedAgentModel, String> {
        assert_eq!(selection.provider, "codex");
        assert_eq!(selection.model, self.0.spec.id.0);
        Ok(ResolvedAgentModel {
            model: self.0.clone(),
            reasoning: ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
            metadata: selection.clone(),
        })
    }

    fn models(&self, _: Option<&str>, _: usize) -> Result<Vec<AgentModelDescriptor>, String> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn priced_codex_child_mirrors_its_own_route_and_cost_once() {
    let directory = tempfile::tempdir().unwrap();
    let parent_server = MockServer::start().await;
    let child_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
            .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"parent\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\ndata: [DONE]\n\n"))
        .expect(2)
        .mount(&parent_server).await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
            .set_body_string(concat!(
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"child\"}}\n\n",
                "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\"}}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"reviewed\"}\n\n",
                "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":300000,\"input_tokens_details\":{\"cached_tokens\":100000},\"output_tokens\":1000,\"total_tokens\":301000}}}\n\n"
            )))
        .expect(1)
        .mount(&child_server).await;
    let mut agent = active_tool_test_agent(
        directory.path(),
        Session::create(directory.path().join("parent.jsonl")).unwrap(),
        active_tool_test_extensions(&["read"]),
    );
    let parent_endpoint = Arc::make_mut(&mut agent.model.endpoint);
    parent_endpoint.base_url = url::Url::parse(&format!("{}/", parent_server.uri())).unwrap();
    parent_endpoint.auth = octet_ai::Auth::None;
    let mut child = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-6.1-sol".into()))
        .unwrap();
    let endpoint = Arc::make_mut(&mut child.endpoint);
    endpoint.id = EndpointId("openai-codex".into());
    endpoint.transport = octet_ai::EndpointTransport::Http;
    endpoint.base_url = url::Url::parse(&format!("{}/", child_server.uri())).unwrap();
    endpoint.auth = octet_ai::Auth::None;
    let spec = Arc::make_mut(&mut child.spec);
    spec.id = ModelId("codex/gpt-6.1-sol".into());
    spec.endpoint = endpoint.id.clone();
    spec.pricing = octet_ai::providers::pricing::pricing_for(
        &octet_ai::providers::contract::CODEX,
        "gpt-6.1-sol",
    );
    agent.set_delegation_model_resolver(Arc::new(WorkerModel(child.clone())));
    let team = agent
        .enable_v2_delegation_extension_only(DelegationConfig::new(directory.path()))
        .unwrap();
    let service = agent
        .delegation
        .as_ref()
        .unwrap()
        .extension_service(
            "accounting-fixture",
            &agent.session_id,
            &agent.resource_owner,
        )
        .unwrap();
    service
        .spawn(
            &agent.resource_owner,
            ExtensionDelegationSpawnRequest {
                task_name: "review".into(),
                profile: Some("review".into()),
                fingerprint: None,
                message: "review this fixture".into(),
                idempotency_key: "review-once".into(),
                policy: ExtensionAgentSessionPolicy {
                    model_selection: Some(AgentModelSelection {
                        provider: "codex".into(),
                        model: child.spec.id.0.clone(),
                        reasoning: "high".into(),
                    }),
                    resolved_model: None,
                    resolved_reasoning: None,
                    tools: vec!["read".into()],
                    max_depth: 1,
                    max_concurrent_children: 16,
                    max_turns: Some(1),
                    max_tokens: None,
                    max_cost_microdollars: None,
                    max_output_bytes: 1024,
                    timeout_ms: Some(30_000),
                },
            },
        )
        .unwrap();
    let settled = service
        .wait(
            &agent.resource_owner,
            Duration::from_secs(10),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(settled["timed_out"], false, "{settled}");
    assert_eq!(
        settled["snapshot"]["agents"][0]["status"]["state"], "completed",
        "{settled}"
    );
    let child_path = std::fs::read_dir(&team)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                && path.file_name().unwrap() != "provenance.jsonl"
        })
        .unwrap();
    let session = Session::open_read_only(child_path).unwrap();
    assert_eq!(session.usage_records().len(), 1);
    let child_usage = &session.usage_records()[0];
    assert!(matches!(
        child_usage.kind,
        UsageRecordKind::AssistantTurn { .. }
    ));
    assert_eq!(child_usage.cost.unwrap().total, 835_000);
    assert_eq!(child_usage.model.as_ref(), Some(&child.spec.id));
    for _ in 0..2 {
        assert!(matches!(
            agent.complete("settle parent").await.unwrap().reason,
            FinishReason::Completed
        ));
        let mirrors = agent
            .session()
            .usage_records()
            .iter()
            .filter(|record| matches!(record.kind, UsageRecordKind::DelegatedAgent { .. }))
            .collect::<Vec<_>>();
        assert_eq!(mirrors.len(), 1);
        assert_eq!(mirrors[0].endpoint, child_usage.endpoint);
        assert_eq!(mirrors[0].model, child_usage.model);
        assert_eq!(mirrors[0].usage, child_usage.usage);
        assert_eq!(mirrors[0].cost, child_usage.cost);
        assert!(!agent.session().has_unpriced_usage());
        assert!(!agent.session().has_uncertain_usage());
    }
}
