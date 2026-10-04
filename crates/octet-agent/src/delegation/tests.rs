use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn delegation_agent_error_conversion_keeps_the_result_error_small() {
    let error: DelegationError = AgentError::Delegation("conversion fixture".into()).into();
    assert!(matches!(error, DelegationError::Agent(error)
            if matches!(*error, AgentError::Delegation(ref message) if message == "conversion fixture")));
    assert!(std::mem::size_of::<DelegationError>() < 128);
}

#[test]
fn delegated_session_references_are_stable_path_free_and_strict() {
    let first = Path::new("/private/sessions/.delegation/team-0123456789abcdef/0001-review.jsonl");
    let same_leaf_elsewhere = Path::new("/other/private/team-0123456789abcdef/0001-review.jsonl");
    let reference = delegated_session_reference(first).unwrap();
    assert_eq!(reference, delegated_session_reference(first).unwrap());
    assert_eq!(
        reference,
        delegated_session_reference(same_leaf_elsewhere).unwrap()
    );
    assert!(reference.starts_with("agent-session:"));
    assert_eq!(reference.len(), "agent-session:".len() + 64);
    assert!(!reference.contains("review"));
    assert!(delegated_session_reference(Path::new(
        "/private/sessions/.delegation/not-a-team/0001-review.jsonl"
    ))
    .is_none());
    assert!(delegated_session_reference(Path::new(
        "/private/sessions/.delegation/team-safe/../outside.jsonl"
    ))
    .is_none());
}

fn test_effective_tool_policy() -> EffectiveToolPolicy {
    crate::SandboxConfig::new("/workspace").effective_tool_policy(crate::EffectPolicy::Controlled)
}

fn test_extension_policy() -> ExtensionAgentSessionPolicy {
    ExtensionAgentSessionPolicy {
        model_selection: None,
        resolved_model: None,
        resolved_reasoning: None,
        tools: vec!["read".into(), "search".into()],
        max_depth: 1,
        max_concurrent_children: 2,
        max_turns: Some(4),
        max_tokens: Some(32_000),
        max_cost_microdollars: Some(200_000),
        max_output_bytes: 8 * 1024,
        timeout_ms: Some(300_000),
    }
}

fn test_extension_spawn(
    task_name: &str,
    profile: Option<&str>,
    fingerprint: Option<&str>,
    message: &str,
    idempotency_key: &str,
) -> ExtensionDelegationSpawnRequest {
    ExtensionDelegationSpawnRequest {
        task_name: task_name.into(),
        profile: profile.map(str::to_owned),
        fingerprint: fingerprint.map(str::to_owned),
        message: message.into(),
        idempotency_key: idempotency_key.into(),
        policy: test_extension_policy(),
    }
}

fn test_template(directory: &Path) -> DelegationTemplate {
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let max_output_tokens = model.spec.limits.max_output_tokens;
    DelegationTemplate {
        model_resolver: RwLock::new(None),
        client: octet_ai::AiClient::new(),
        model,
        base_system: RwLock::new("test".into()),
        sandbox: crate::SandboxConfig::new(directory),
        effect_broker: crate::EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: Some(4),
        reasoning: RwLock::new(octet_ai::ReasoningConfig::Off),
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        runtime: RwLock::new(DelegationRuntimeSettings {
            compaction_model: None,
            auto_compaction_mode: AgentCompactionMode::Local,
            auto_compaction_threshold: 1.0,
            compaction_keep_recent_tokens: 1_024,
            completion_policy: CompletionPolicy::Natural,
            output_modalities: octet_ai::OutputModalities::Text,
            max_output_tokens,
            tool_schema_budget_bytes: crate::agent::DEFAULT_TOOL_SCHEMA_BUDGET_BYTES,
            max_session_tokens: None,
            max_session_cost_microdollars: None,
            cache_warming_mode: crate::cache_warmer::CacheWarmer::default().mode_control(),
            provider_retries_enabled: true,
            max_network_wait: None,
        }),
    }
}

/// Builds a manager through the production construction point.
///
/// Fixtures go through [`DelegationManager::assemble`] and claim the durable
/// fleet lease exactly as `create_with_journal` does, so a new field can
/// never be missing from a fixture's second copy of the initializer. The
/// only fixture-specific input is the journal file, the template, and the
/// child-slot bound the tests were written against.
fn fixture_manager(
    directory: &Path,
    file: File,
    template: DelegationTemplate,
) -> Arc<DelegationManager> {
    let mut config = DelegationConfig::new(directory);
    // Three child slots, as the fixtures expect, with every other host limit
    // left at its production default.
    config.limits.max_concurrent_agents = 4;
    let root_session = PathBuf::new();
    let manager = DelegationManager::assemble(
        config,
        true,
        directory.to_path_buf(),
        None,
        ProvenanceJournal {
            file: Mutex::new(file),
        },
        template,
        root_session.clone(),
    );
    // Mirror production: claim the session's durable fleet and record the
    // bounded reason when another live owner already holds it.
    let (lease, lease_refusal) = match FleetLease::try_acquire(directory, &root_session) {
        Ok(lease) => (Some(lease), None),
        Err(reason) => (None, Some(reason)),
    };
    *manager
        .lease
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = lease;
    *manager
        .lease_refusal
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = lease_refusal;
    manager
}

fn manager_with_journal(file: File, directory: &Path) -> Arc<DelegationManager> {
    fixture_manager(directory, file, test_template(directory))
}

fn read_only_journal(directory: &Path) -> File {
    let path = directory.join("read-only-journal");
    std::fs::write(&path, b"").unwrap();
    File::open(path).unwrap()
}

fn writable_manager(directory: &Path) -> Arc<DelegationManager> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(directory.join("provenance.jsonl"))
        .unwrap();
    manager_with_journal(file, directory)
}

fn writable_manager_with_core_tools(directory: &Path) -> Arc<DelegationManager> {
    let mut manager = writable_manager(directory);
    let manager_mut = Arc::get_mut(&mut manager).expect("new manager is uniquely owned");
    manager_mut.template.extensions.load(&crate::CoreTools);
    manager
}

struct AlternateModelResolver {
    model: octet_ai::Model,
}
impl AgentModelResolver for AlternateModelResolver {
    fn resolve(
        &self,
        selection: &AgentModelSelection,
        parent: &octet_ai::Model,
        reasoning: &octet_ai::ReasoningConfig,
    ) -> Result<ResolvedAgentModel, String> {
        let model = match selection.model.as_str() {
            "inherit" => parent.clone(),
            id if id == self.model.spec.id.0 => self.model.clone(),
            _ => return Err("unsupported_model: not configured".into()),
        };
        if selection.reasoning != "inherit" && selection.reasoning != "off" {
            return Err("unsupported_reasoning: unsupported fixture level".into());
        }
        Ok(ResolvedAgentModel {
            metadata: AgentModelSelection {
                provider: model.spec.endpoint.0.clone(),
                model: model.spec.id.0.clone(),
                reasoning: "off".into(),
            },
            model,
            reasoning: reasoning.clone(),
        })
    }
    fn models(
        &self,
        _query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AgentModelDescriptor>, String> {
        Ok((0..limit)
            .map(|_| AgentModelDescriptor {
                model: self.model.spec.id.0.clone(),
                provider: self.model.spec.endpoint.0.clone(),
                display_name: None,
                reasoning: vec!["off".into()],
                context_window: self.model.spec.limits.context_window,
                max_output_tokens: self.model.spec.limits.max_output_tokens,
            })
            .collect())
    }
}

#[tokio::test]
async fn finite_child_ceilings_refuse_spawn_and_continuation_before_provider_dispatch() {
    for token_ceiling in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let team = directory.path().join("team-finiteceilings");
        std::fs::create_dir(&team).unwrap();
        let server = continuation_test_server().await;
        let manager = continuation_test_manager(&team, &server);
        let binding = manager.root_binding();
        let service = binding
            .extension_service("ceiling-fixture", "session", "owner")
            .unwrap();
        let mut request = test_extension_spawn("bounded", None, None, "work", "bounded-key");
        request.policy.max_tokens = token_ceiling.then_some(64_000);
        request.policy.max_cost_microdollars =
            (!token_ceiling).then_some(MAX_EXTENSION_COST_MICRODOLLARS);
        let requested_policy = request.policy.clone();
        let spawned = service.spawn("owner", request).unwrap();
        let id = spawned["agent_id"].as_str().unwrap();
        let failed = DelegatedAgentStatus::Failed {
            error: AgentError::InputLimitUnavailable.to_string(),
        };
        wait_for_worker_status(&manager, id, failed.clone()).await;
        service
            .follow_up("owner", id, "continue with pinned limits".into())
            .await
            .unwrap();
        wait_for_worker_status(&manager, id, failed).await;
        assert!(server.received_requests().await.unwrap().is_empty());
        let state = manager.state.lock().unwrap();
        let record = &state.records[id];
        let policy = record.extension_policy.as_ref().unwrap();
        assert_eq!(policy.max_tokens, requested_policy.max_tokens);
        assert_eq!(
            policy.max_cost_microdollars,
            requested_policy.max_cost_microdollars
        );
        let child = Session::open_read_only(&record.session_path).unwrap();
        assert!(child.usage_records().is_empty());
        assert!(!child.has_uncertain_usage());
        drop(state);
        binding.request_shutdown();
    }
}

#[tokio::test]
async fn configured_model_routes_spawn_and_continuation_and_pins_durable_policy() {
    let directory = tempfile::tempdir().unwrap();
    let team = directory.path().join("team-multimodel");
    std::fs::create_dir(&team).unwrap();
    let parent_server = MockServer::start().await;
    let alternate_server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
            .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"alternate answer\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":5,\"total_tokens\":25}}\n\ndata: [DONE]\n\n"))
            .expect(2).mount(&alternate_server).await;
    let mut manager = writable_manager_with_core_tools(&team);
    let inner = Arc::get_mut(&mut manager).unwrap();
    Arc::make_mut(&mut inner.template.model.endpoint).base_url =
        url::Url::parse(&format!("{}/", parent_server.uri())).unwrap();
    let mut alternate = inner.template.model.clone();
    Arc::make_mut(&mut inner.template.model.spec).pricing = None;
    let spec = Arc::make_mut(&mut alternate.spec);
    spec.id = octet_ai::ModelId("fixture-alternate".into());
    spec.api_name = "alternate-wire-model".into();
    spec.limits.max_output_tokens = 1024;
    let endpoint = Arc::make_mut(&mut alternate.endpoint);
    endpoint.base_url = url::Url::parse(&format!("{}/", alternate_server.uri())).unwrap();
    endpoint.auth = octet_ai::Auth::None;
    *inner.template.model_resolver.get_mut().unwrap() =
        Some(Arc::new(AlternateModelResolver { model: alternate }));
    let binding = manager.root_binding();
    let telemetry = manager.attach_telemetry();
    let service = binding
        .extension_service("extension-routing", "parent-session", "root-owner")
        .unwrap();
    let request = || {
        let mut request =
            test_extension_spawn("routing", None, None, "use alternate", "routing-key");
        // Routing/continuation is exercised independently of unsupported hard
        // input ceilings; the finite-ceiling denial contract has its own test.
        request.policy.max_tokens = None;
        request.policy.max_cost_microdollars = None;
        request.policy.model_selection = Some(AgentModelSelection {
            model: "fixture-alternate".into(),
            ..Default::default()
        });
        request
    };
    let first = service.spawn("root-owner", request()).unwrap();
    assert_eq!(first["resolved_model"]["model"], "fixture-alternate");
    assert_eq!(first["resolved_model"]["reasoning"], json!({"type":"off"}));
    assert_eq!(
        first["policy"]["resolved_model"]["reasoning"],
        json!({"type":"off"})
    );
    assert!(first["policy"].get("resolved_reasoning").is_none());
    let id = first["agent_id"].as_str().unwrap();
    for run in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = manager.state.lock().unwrap().records[id].status.clone();
                if matches!(state, DelegatedAgentStatus::Completed { .. }) {
                    break;
                }
                assert!(
                    !matches!(state, DelegatedAgentStatus::Failed { .. }),
                    "{state:?}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if run == 0 {
            service
                .follow_up("root-owner", id, "continue same route".into())
                .await
                .unwrap();
        }
    }
    assert!(parent_server.received_requests().await.unwrap().is_empty());
    for request in alternate_server.received_requests().await.unwrap() {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], "alternate-wire-model");
        assert!(
            body["max_completion_tokens"]
                .as_u64()
                .or_else(|| body["max_tokens"].as_u64())
                .unwrap()
                <= 1024
        );
    }
    service.state.lock().unwrap().owners.clear();
    assert_eq!(
        service.spawn("root-owner", request()).unwrap()["agent_id"],
        id
    );
    let listed = service.list("root-owner").unwrap();
    assert_eq!(
        listed["agents"][0]["policy"]["resolved_model"]["reasoning"],
        json!({"type":"off"})
    );
    let replay = service.spawn("root-owner", request()).unwrap();
    assert_eq!(
        replay["policy"]["resolved_model"]["reasoning"],
        json!({"type":"off"})
    );
    let mut changed = request();
    changed.policy.model_selection.as_mut().unwrap().reasoning = "off".into();
    assert!(service
        .spawn("root-owner", changed)
        .unwrap_err()
        .contains("different input"));
    let state = manager.state.lock().unwrap();
    assert!(state.records[id].cost_microdollars.is_some());
    assert_eq!(
        telemetry.borrow().as_ref().unwrap().children[0].model,
        "fixture-alternate"
    );
    let fleet: Value =
        serde_json::from_slice(&std::fs::read(fleet_roster_path(&team, Path::new(""))).unwrap())
            .unwrap();
    assert!(fleet.to_string().contains("fixture-alternate"));
    let policy = state.records[id].extension_policy.as_ref().unwrap();
    let encoded = serde_json::to_vec(policy).unwrap();
    let restored: ExtensionAgentSessionPolicy = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        manager
            .template
            .resolve_model(Some(&restored))
            .unwrap()
            .model
            .spec
            .id
            .0,
        "fixture-alternate"
    );
    assert_eq!(
        resolved_model_json(Some(&restored))["model"],
        "fixture-alternate"
    );
    let mut changed_reasoning = restored.clone();
    changed_reasoning.resolved_reasoning = Some(octet_ai::ReasoningConfig::On);
    assert!(manager
        .template
        .resolve_model(Some(&changed_reasoning))
        .is_err());
    drop(state);
    let catalog = service.models("root-owner", None, 1).unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["truncated"], true);
    assert!(service.models("wrong-owner", None, 1).is_err());
    assert!(service.models("root-owner", None, 101).is_err());
    assert!(service
        .models("root-owner", Some(&"x".repeat(129)), 1)
        .is_err());
    binding.request_shutdown();
}

#[test]
fn legacy_fleet_roster_loads_and_upgrades_without_routing_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let session_path = root.join("legacy-child.jsonl");
    Session::create(&session_path).unwrap();
    {
        let manager = writable_manager(&root);
        insert_durable_detached_record(
            &manager,
            "agent-1",
            "/root/legacy",
            session_path,
            DelegatedAgentStatus::Completed {
                output: "old result".into(),
            },
        );
        manager.persist_durable_fleet_locked(&mut manager.state.lock().unwrap());
    }
    let path = fleet_roster_path(&root, Path::new(""));
    let mut fleet: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    fleet["version"] = json!(1);
    std::fs::write(&path, serde_json::to_vec(&fleet).unwrap()).unwrap();
    let manager = writable_manager(&root);
    manager.restore_durable_fleet();
    let mut state = manager.state.lock().unwrap();
    assert_eq!(state.records.len(), 1);
    assert!(state.records["agent-1"].extension_policy.is_none());
    manager.persist_durable_fleet_locked(&mut state);
    let upgraded: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(upgraded["version"], 2);
}

#[tokio::test]
async fn configured_route_survives_fleet_reconstruction_with_a_different_parent() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let alternate_server = MockServer::start().await;
    let changed_parent_server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"route persisted\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
            .expect(2).mount(&alternate_server).await;
    let mut alternate = test_template(&root).model;
    Arc::make_mut(&mut alternate.spec).id = octet_ai::ModelId("saved-alternate".into());
    Arc::make_mut(&mut alternate.spec).api_name = "saved-wire-model".into();
    Arc::make_mut(&mut alternate.endpoint).base_url =
        url::Url::parse(&format!("{}/", alternate_server.uri())).unwrap();
    Arc::make_mut(&mut alternate.endpoint).auth = octet_ai::Auth::None;
    let session_path = root.join("saved-child.jsonl");
    {
        let manager = writable_manager_with_core_tools(&root);
        *manager.template.model_resolver.write().unwrap() =
            Some(Arc::new(AlternateModelResolver {
                model: alternate.clone(),
            }));
        let session = Session::create(&session_path).unwrap();
        insert_durable_detached_record(
            &manager,
            "agent-1",
            "/root/saved",
            session_path.clone(),
            DelegatedAgentStatus::Completed {
                output: "first run".into(),
            },
        );
        let mut policy = test_extension_policy();
        policy.max_tokens = None;
        policy.max_cost_microdollars = None;
        policy.model_selection = Some(AgentModelSelection {
            model: "saved-alternate".into(),
            ..Default::default()
        });
        let resolved = manager.template.resolve_model(Some(&policy)).unwrap();
        policy.resolved_model = Some(resolved.metadata);
        policy.resolved_reasoning = Some(resolved.reasoning);
        let identity = manager.state.lock().unwrap().records["agent-1"]
            .identity
            .clone();
        let mut child = manager
            .build_child_agent(session, &identity, Some(&policy))
            .unwrap();
        child.complete("original child task").await.unwrap();
        let mut state = manager.state.lock().unwrap();
        state.records.get_mut("agent-1").unwrap().extension_policy = Some(policy);
        manager.persist_durable_fleet_locked(&mut state);
    }
    let fleet: Value =
        serde_json::from_slice(&std::fs::read(fleet_roster_path(&root, Path::new(""))).unwrap())
            .unwrap();
    assert_eq!(
        fleet["version"], 2,
        "old v1 hosts must reject routed rosters"
    );
    // The old manager and child are gone. Rebuild from disk with a different
    // parent binding and run the ordinary durable follow-up path.
    let mut manager = writable_manager_with_core_tools(&root);
    let template = &mut Arc::get_mut(&mut manager).unwrap().template;
    Arc::make_mut(&mut template.model.spec).id = octet_ai::ModelId("different-parent".into());
    Arc::make_mut(&mut template.model.endpoint).base_url =
        url::Url::parse(&format!("{}/", changed_parent_server.uri())).unwrap();
    *template.model_resolver.get_mut().unwrap() =
        Some(Arc::new(AlternateModelResolver { model: alternate }));
    manager.restore_durable_fleet();
    assert_eq!(
        manager.state.lock().unwrap().records["agent-1"]
            .extension_policy
            .as_ref()
            .unwrap()
            .resolved_model
            .as_ref()
            .unwrap()
            .model,
        "saved-alternate"
    );
    manager.prepare_owning_run(&root_identity()).unwrap();
    manager
        .follow_up(
            &root_identity(),
            FollowUpRequest {
                target: "agent-1".into(),
                message: "continue after restart".into(),
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = manager.state.lock().unwrap().records["agent-1"]
                .status
                .clone();
            if matches!(status, DelegatedAgentStatus::Completed { .. }) {
                break;
            }
            assert!(
                !matches!(status, DelegatedAgentStatus::Failed { .. }),
                "{status:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(changed_parent_server
        .received_requests()
        .await
        .unwrap()
        .is_empty());
    let requests = alternate_server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["model"], "saved-wire-model");
    assert!(body.to_string().contains("original child task"));
    let transcript = Session::open(&session_path).unwrap();
    assert!(transcript.entries().iter().any(|entry| matches!(&entry.value,
            crate::session::EntryValue::Config { model: Some(model), reasoning: Some(reasoning), .. }
            if model == "saved-alternate" && reasoning == "off")));
    manager.root_binding().request_shutdown();
}

#[test]
fn model_selection_refuses_before_admission_without_a_resolver() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager_with_core_tools(directory.path());
    let service = manager
        .root_binding()
        .extension_service("extension-routing", "parent-session", "root-owner")
        .unwrap();
    for selection in [
        AgentModelSelection {
            model: "unknown".into(),
            ..Default::default()
        },
        AgentModelSelection {
            reasoning: "high".into(),
            ..Default::default()
        },
    ] {
        let mut request =
            test_extension_spawn("routing", None, None, "must not run", "routing-key");
        request.policy.model_selection = Some(selection);
        assert!(service
            .spawn("root-owner", request)
            .unwrap_err()
            .starts_with("unsupported_"));
        assert!(manager.state.lock().unwrap().records.is_empty());
    }
}

#[tokio::test]
async fn delegated_astra_ultra_v2_child_uses_xhigh_wire_effort() {
    let directory = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(
                        b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_test\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"ok\"}\n\ndata: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n",
                    ),
            )
            .expect(1)
            .mount(&server)
            .await;

    let mut manager = writable_manager(directory.path());
    {
        let manager_mut = Arc::get_mut(&mut manager).expect("new manager is uniquely owned");
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-6-astra".into()))
            .unwrap();
        let spec = Arc::make_mut(&mut model.spec);
        spec.id = octet_ai::ModelId("codex/gpt-6-astra".into());
        spec.capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
        let reasoning = spec
            .capabilities
            .reasoning
            .as_mut()
            .expect("Astra reasoning capability");
        reasoning.max_effort = octet_ai::ReasoningEffort::Ultra;
        let endpoint = Arc::make_mut(&mut model.endpoint);
        endpoint.base_url = url::Url::parse(&format!("{}/", server.uri())).unwrap();
        endpoint.auth = octet_ai::Auth::None;
        endpoint.transport = octet_ai::EndpointTransport::Http;
        manager_mut
            .template
            .runtime
            .get_mut()
            .unwrap()
            .max_output_tokens = model.spec.limits.max_output_tokens;
        manager_mut.template.model = model;
        *manager_mut.template.reasoning.get_mut().unwrap() =
            octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra);
    }

    let (identity, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let session = Session::create(directory.path().join("astra-child.jsonl")).unwrap();
    let mut child = manager.build_child_agent(session, &identity, None).unwrap();
    child
        .complete("verify delegated wire effort")
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["reasoning"]["effort"], "xhigh");
}

#[cfg(unix)]
fn writable_manager_with_workspace(directory: &Path, workspace: &Path) -> Arc<DelegationManager> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(directory.join("provenance.jsonl"))
        .unwrap();
    fixture_manager(directory, file, test_template(workspace))
}

#[tokio::test]
async fn telemetry_snapshots_are_monotonic_and_spawn_is_not_a_tool_use() {
    let directory = tempfile::tempdir().unwrap();
    let team_directory = directory.path().join("team-test");
    std::fs::create_dir(&team_directory).unwrap();
    let manager = writable_manager_with_core_tools(&team_directory);
    let root = manager.root_binding();
    let mut telemetry = root.telemetry_receiver().expect("root telemetry stream");
    telemetry.changed().await.expect("initial snapshot");
    let first = telemetry
        .borrow_and_update()
        .clone()
        .expect("initial snapshot");
    let service = root
        .extension_service("principal", "parent-session", "owner")
        .unwrap();
    let result = service
        .spawn(
            "owner",
            test_extension_spawn("explore", Some("explore"), None, "read", "telemetry-1"),
        )
        .unwrap();
    let agent_id = result["agent_id"].as_str().unwrap().to_owned();
    telemetry.changed().await.expect("spawn snapshot");
    let spawned = telemetry
        .borrow_and_update()
        .clone()
        .expect("spawn snapshot");
    let child = spawned
        .children
        .iter()
        .find(|child| child.child_id == agent_id)
        .unwrap();
    assert!(spawned.revision > first.revision);
    assert_eq!(child.tool_use_count, 0);
    assert_eq!(child.task_name, "explore");
    assert_eq!(
        child.effective_tool_policy.effect_policy.value,
        crate::EffectPolicy::Controlled
    );
    assert_eq!(
        child.orchestration_provenance.approval_authority,
        DelegationPolicySource::ParentInherited
    );
    assert_eq!(
        child.orchestration_provenance.tool_scope,
        DelegationPolicySource::ChildOverride
    );
    assert_eq!(
        child.orchestration_provenance.execution_limits,
        DelegationPolicySource::ChildOverride
    );

    manager.update_agent_tool_started(
        &agent_id,
        "tool-1",
        "read".into(),
        "path=src/lib.rs".to_owned(),
    );
    telemetry.changed().await.expect("tool-start snapshot");
    let using_tool = telemetry
        .borrow_and_update()
        .clone()
        .expect("tool-start snapshot");
    let child = using_tool
        .children
        .iter()
        .find(|child| child.child_id == agent_id)
        .unwrap();
    assert!(using_tool.revision > spawned.revision);
    assert_eq!(child.tool_use_count, 1);
    assert_eq!(child.current_tool.as_deref(), Some("read"));
}

#[tokio::test]
async fn telemetry_stream_coalesces_slow_consumer_updates_to_the_latest_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let root = manager.root_binding();
    let mut telemetry = root.telemetry_receiver().expect("root telemetry stream");
    telemetry.changed().await.expect("initial snapshot");
    let initial_revision = telemetry
        .borrow_and_update()
        .as_ref()
        .expect("initial snapshot")
        .revision;

    for revision in 0..128 {
        manager.publish_external_failure("test", &format!("failure-{revision}"));
    }

    telemetry.changed().await.expect("coalesced snapshot");
    let latest = telemetry
        .borrow_and_update()
        .clone()
        .expect("coalesced snapshot");
    assert_eq!(latest.revision, initial_revision + 128);
    assert_eq!(latest.failure_reason.as_deref(), Some("failure-127"));
}

#[test]
fn owning_run_restart_reactivates_root_without_recycling_session_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let root = manager.root_binding().identity;
    let old_permits = manager.current_permits();
    let _old_slots = (0..3)
        .map(|_| Arc::clone(&old_permits).try_acquire_owned().unwrap())
        .collect::<Vec<_>>();
    {
        let mut state = manager.state.lock().unwrap();
        state.root_mailbox.push_back(MailboxMessage {
            kind: "message",
            from: "agent-old".into(),
            task_name: None,
            message: "durable root message".into(),
            evictable: false,
            continued: false,
            leased: false,
        });
        state.root_mailbox.push_back(MailboxMessage {
            kind: "task_status",
            from: "agent-old".into(),
            task_name: Some("old".into()),
            message: "stale status".into(),
            evictable: true,
            continued: false,
            leased: false,
        });
    }

    manager.request_shutdown_descendants(ROOT_AGENT_ID);
    assert!(manager.list_value_for(&root).is_err());

    manager.prepare_owning_run(&root).unwrap();
    assert!(manager.list_value_for(&root).is_ok());
    // Session-scoped lifetime keeps the cap honest: reactivating the root
    // never hands back execution slots a surviving worker still holds, so
    // the bound cannot drift up across the turn boundary.
    assert!(manager.current_permits().try_acquire_owned().is_err());
    let state = manager
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(state.root_active);
    assert_eq!(state.total_agents, 1);
    assert!(state.records.is_empty());
    assert_eq!(state.root_mailbox.len(), 2);
    assert_eq!(state.root_mailbox[0].message, "durable root message");
    assert_eq!(state.root_mailbox[1].message, "stale status");
}

#[test]
fn child_owning_run_restart_cancels_descendants_and_preserves_durable_mail() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _child_commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let grandchild = AgentIdentity {
        id: "agent-2".into(),
        path: "/root/child/grandchild".into(),
        depth: 2,
    };
    let grandchild_shutdown = crate::CancellationToken::default();
    let (command_tx, _grandchild_commands) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    {
        let mut state = manager.state.lock().unwrap();
        let child_record = state.records.get_mut(&child.id).unwrap();
        child_record.mailbox.push_back(MailboxMessage {
            kind: "message",
            from: ROOT_AGENT_ID.into(),
            task_name: None,
            message: "leased message from the prior owning run".into(),
            evictable: false,
            continued: false,
            leased: true,
        });
        child_record.mailbox_delivery = Some(MailboxDeliveryPlan {
            id: 1,
            complete_messages: 1,
            partial_bytes: 0,
            touched_messages: 1,
        });
        child_record.mailbox.push_back(MailboxMessage {
            kind: "task_status",
            from: "agent-old".into(),
            task_name: Some("old".into()),
            message: "stale automatic status".into(),
            evictable: true,
            continued: false,
            leased: false,
        });
        child_record.mailbox.push_back(MailboxMessage {
            kind: "message",
            from: ROOT_AGENT_ID.into(),
            task_name: None,
            message: "unleased durable message".into(),
            evictable: false,
            continued: false,
            leased: false,
        });
        let mut record = fixture_record(
            DurableFleetRecord {
                agent_id: grandchild.id.clone(),
                agent_path: grandchild.path.clone(),
                parent_id: child.id.clone(),
                depth: grandchild.depth,
                task_name: "grandchild".into(),
                session_path: manager.team_directory.join("grandchild.jsonl"),
                status: DelegatedAgentStatus::Running,
                created_at_ms: 1,
                started_at_ms: Some(1),
                ..DurableFleetRecord::default()
            },
            false,
            false,
            command_tx,
            None,
        );
        // This fixture asserts the descendant's own shutdown token, so the
        // token is supplied here rather than by the shared mapping.
        record.shutdown = grandchild_shutdown.clone();
        state.records.insert(grandchild.id.clone(), record);
        state.total_agents += 1;
    }

    manager.prepare_owning_run(&child).unwrap();

    let state = manager.state.lock().unwrap();
    assert!(grandchild_shutdown.is_cancelled());
    assert!(!state.records.contains_key(&grandchild.id));
    let child_record = &state.records[&child.id];
    assert_eq!(child_record.mailbox.len(), 3);
    assert_eq!(
        child_record
            .mailbox
            .iter()
            .map(|message| message.message.as_str())
            .collect::<Vec<_>>(),
        [
            "leased message from the prior owning run",
            "stale automatic status",
            "unleased durable message"
        ]
    );
    assert!(child_record.mailbox.front().unwrap().leased);
    assert!(child_record.mailbox_delivery.is_some());
}

#[tokio::test]
async fn prompt_failure_after_durable_task_append_is_not_retried() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _child_commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let session_path = directory.path().join("prompt-classification.jsonl");
    let session_file = secure_fs::create_regular_file_for_append(&session_path).unwrap();
    let session = Session::create_with_file(&session_path, session_file).unwrap();
    let mut agent = manager.build_child_agent(session, &child, None).unwrap();
    {
        let mut state = manager.state.lock().unwrap();
        state.records.get_mut(&child.id).unwrap().session_path = session_path.clone();
        state.persistence_error = Some("forced owning-run preparation failure".into());
    }
    let (_command_tx, mut commands) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let shutdown = crate::CancellationToken::default();

    let execution = manager
        .execute_child_run(
            &mut agent,
            "task accepted before startup failed".into(),
            ChildRunContext {
                queued_delivery_ids: BTreeSet::new(),
                identity: &child,
                commands: &mut commands,
                shutdown: &shutdown,
                extension_policy: None,
                deadline: None,
            },
        )
        .await;

    assert!(execution.task_delivered);
    match execution.outcome {
        WorkerOutcome::Failed(error) => {
            assert!(
                error.contains("delegated run could not start after the task was durably accepted")
            )
        }
        _ => panic!("expected startup failure"),
    }
    let snapshot = Session::open_read_only(&session_path).unwrap();
    assert_eq!(
        snapshot
            .entries()
            .iter()
            .filter(|entry| matches!(
                &entry.value,
                crate::session::EntryValue::Message(octet_ai::Message::User(message))
                    if message.content.len() == 1
                        && matches!(
                            &message.content[0],
                            octet_ai::UserPart::Text(text)
                                if text == "task accepted before startup failed"
                        )
            ))
            .count(),
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn prompt_failure_inspection_stays_bound_to_the_original_session_descriptor() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _child_commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let session_path = directory.path().join("prompt-replacement.jsonl");
    let session_file = secure_fs::create_regular_file_for_append(&session_path).unwrap();
    let session = Session::create_with_file(&session_path, session_file).unwrap();
    let mut agent = manager.build_child_agent(session, &child, None).unwrap();
    {
        let mut state = manager.state.lock().unwrap();
        state.records.get_mut(&child.id).unwrap().session_path = session_path.clone();
        state.persistence_error = Some("forced owning-run preparation failure".into());
    }

    let original_path = session_path.with_extension("jsonl.original");
    std::fs::rename(&session_path, &original_path).unwrap();
    drop(Session::create(&session_path).unwrap());

    let (_command_tx, mut commands) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let shutdown = crate::CancellationToken::default();
    let execution = manager
        .execute_child_run(
            &mut agent,
            "task persisted through the original descriptor".into(),
            ChildRunContext {
                queued_delivery_ids: BTreeSet::new(),
                identity: &child,
                commands: &mut commands,
                shutdown: &shutdown,
                extension_policy: None,
                deadline: None,
            },
        )
        .await;

    assert!(execution.task_delivered);
    assert!(matches!(execution.outcome, WorkerOutcome::Failed(_)));
    let original = Session::open_read_only(&original_path).unwrap();
    assert!(original.entries().iter().any(|entry| matches!(
        &entry.value,
        crate::session::EntryValue::Message(octet_ai::Message::User(message))
            if message.content.len() == 1
                && matches!(
                    &message.content[0],
                    octet_ai::UserPart::Text(text)
                        if text == "task persisted through the original descriptor"
                )
    )));
    assert!(Session::open_read_only(&session_path)
        .unwrap()
        .entries()
        .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn worker_reopen_failure_retains_initial_and_follow_up_work() {
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let manager = writable_manager_with_workspace(directory.path(), &workspace);
    let root = manager.root_binding().identity;
    let spawned = manager
        .spawn(
            &root,
            SpawnRequest {
                task_name: "reopen-failure".into(),
                display_task_name: None,
                message: "initial task must remain first".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap();
    let child_id = spawned["agent_id"].as_str().unwrap().to_owned();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let failed = {
                let state = manager.state.lock().unwrap();
                matches!(
                    &state.records[&child_id].status,
                    DelegatedAgentStatus::Failed { error }
                        if error.contains("task retained for retry")
                )
            };
            if failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial child startup did not fail");

    let session_path = {
        manager.state.lock().unwrap().records[&child_id]
            .session_path
            .clone()
    };
    let saved_session = session_path.with_extension("jsonl.saved");
    std::fs::rename(&session_path, &saved_session).unwrap();
    let outside = directory.path().join("outside-session");
    std::fs::write(&outside, b"outside must not be opened\n").unwrap();
    symlink(&outside, &session_path).unwrap();
    std::fs::create_dir(&workspace).unwrap();

    manager
        .follow_up(
            &root,
            FollowUpRequest {
                target: child_id.clone(),
                message: "accepted follow-up must remain behind initial".into(),
            },
        )
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let failed = {
                let state = manager.state.lock().unwrap();
                matches!(
                    &state.records[&child_id].status,
                    DelegatedAgentStatus::Failed { error }
                        if error.contains("task retained for retry")
                ) && state.records[&child_id].queued_follow_ups.messages == 1
            };
            if failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("descriptor-bound child reopen did not fail");

    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"outside must not be opened\n"
    );
    assert!(std::fs::symlink_metadata(&session_path)
        .unwrap()
        .file_type()
        .is_symlink());
    let state = manager.state.lock().unwrap();
    let record = &state.records[&child_id];
    assert_eq!(record.queued_follow_ups.messages, 1);
    assert!(matches!(
        &record.status,
        DelegatedAgentStatus::Failed { error }
            if error.contains("task retained for retry")
    ));
    drop(state);

    manager.request_shutdown_descendants(ROOT_AGENT_ID);
    std::fs::remove_file(&session_path).unwrap();
    std::fs::rename(saved_session, session_path).unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn failed_team_activation_removes_the_allocated_team_directory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let teams = root.join("teams");
    let root_session = root.join("root.jsonl");
    let result = DelegationManager::create_with_journal(
        DelegationConfig::new(&teams),
        test_template(&root),
        &root_session,
        true,
        |directory| {
            let path = directory.path().join("provenance.jsonl");
            drop(directory.create_regular_file_for_append(&path)?);
            Ok(ProvenanceJournal {
                file: Mutex::new(directory.open_regular_file_for_read(&path)?),
            })
        },
    );

    let error = result
        .err()
        .expect("read-only journal must fail activation");
    assert!(error.to_string().contains("delegation persistence failed"));
    assert!(teams.exists());
    assert_eq!(std::fs::read_dir(teams).unwrap().count(), 0);
}

#[cfg(any(unix, windows))]
#[test]
fn failed_team_activation_does_not_remove_a_replacement_directory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let teams = root.join("teams");
    let original = teams.join("original-team");
    let mut replacement_marker = None;
    let result = DelegationManager::create_with_journal(
        DelegationConfig::new(&teams),
        test_template(&root),
        &root.join("root.jsonl"),
        true,
        |directory| {
            std::fs::rename(directory.path(), &original).unwrap();
            secure_fs::create_private_directory_all(directory.path()).unwrap();
            let marker = directory.path().join("replacement-marker");
            std::fs::write(&marker, b"replacement").unwrap();
            replacement_marker = Some(marker);
            Err(DelegationError::InvalidConfig(
                "forced activation failure".into(),
            ))
        },
    );

    let error = result.err().expect("activation must fail");
    assert!(matches!(error, DelegationError::ActivationRollback { .. }));
    assert!(original.exists());
    assert_eq!(
        std::fs::read(replacement_marker.unwrap()).unwrap(),
        b"replacement"
    );
}

#[cfg(any(unix, windows))]
// NTFS refuses to rename a directory with open descendants, and the
// live manager holds its team directory (and lease files) open, so this
// replacement scenario cannot be set up on Windows. Unix-only.
#[cfg(unix)]
#[test]
fn child_session_creation_rejects_a_replaced_team_directory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let teams = root.join("teams");
    let manager = DelegationManager::create(
        DelegationConfig::new(&teams),
        test_template(&root),
        &root.join("root.jsonl"),
        true,
    )
    .unwrap();
    let team_directory = manager.team_directory.clone();
    let original = teams.join("original-team");
    std::fs::rename(&team_directory, &original).unwrap();
    secure_fs::create_private_directory_all(&team_directory).unwrap();
    let marker = team_directory.join("replacement-marker");
    std::fs::write(&marker, b"replacement").unwrap();

    let error = manager
        .spawn(
            &root_identity(),
            SpawnRequest {
                task_name: "child".into(),
                display_task_name: None,
                message: "do work".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();

    assert!(error.contains("changed"), "unexpected error: {error}");
    assert_eq!(std::fs::read(&marker).unwrap(), b"replacement");
    assert!(!team_directory.join("0001-child.jsonl").exists());
    assert!(original.join("provenance.jsonl").exists());
}

fn root_identity() -> AgentIdentity {
    AgentIdentity {
        id: ROOT_AGENT_ID.into(),
        path: ROOT_AGENT_PATH.into(),
        depth: 0,
    }
}

/// Builds a fixture worker record from the shared durable-record mapping.
///
/// `detached`/`live_task` are stated by each fixture because the mapping's
/// `detached: true` is the roster-restore default, and `commands` lets a
/// fixture store a restored receiver or keep steering local.
fn fixture_record(
    durable: DurableFleetRecord,
    detached: bool,
    live_task: bool,
    command_tx: mpsc::Sender<WorkerCommand>,
    commands: Option<mpsc::Receiver<WorkerCommand>>,
) -> AgentRecord {
    // Fixtures name the exact lifecycle status they mean: the shared
    // mapping's `pending|running -> detached` rewrite is roster-restore
    // behavior and must not leak into a fixture that stands for a live,
    // pending, or running worker.
    let requested_status = durable.status.clone();
    let mut record = DelegationManager::agent_record_from_durable(
        durable,
        test_effective_tool_policy(),
        None,
        command_tx,
        commands,
    );
    record.status = requested_status;
    record.detached = detached;
    record.live_task = live_task;
    record
}

/// Inserts one fixture worker derived from the same mapping the roster
/// restore uses.
///
/// Returns the identity and, when the fixture keeps steering local
/// (`store_receiver == false`), the receiver paired with the record's
/// command sender. With `store_receiver == true` the record owns its
/// receiver, exactly like a restored or parked worker.
fn insert_fixture_record(
    manager: &DelegationManager,
    durable: DurableFleetRecord,
    detached: bool,
    live_task: bool,
    store_receiver: bool,
) -> (AgentIdentity, Option<mpsc::Receiver<WorkerCommand>>) {
    let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (stored, kept) = if store_receiver {
        (Some(command_rx), None)
    } else {
        (None, Some(command_rx))
    };
    let record = fixture_record(durable, detached, live_task, command_tx, stored);
    let identity = record.identity.clone();
    let mut state = manager.state.lock().unwrap();
    state.records.insert(identity.id.clone(), record);
    state.total_agents += 1;
    (identity, kept)
}

/// A never-run, attached-nowhere fixture worker, as the reattach tests
/// describe it.
fn insert_test_record(
    manager: &DelegationManager,
    status: DelegatedAgentStatus,
) -> (AgentIdentity, mpsc::Receiver<WorkerCommand>) {
    let durable = DurableFleetRecord {
        agent_id: "agent-1".into(),
        agent_path: "/root/child".into(),
        parent_id: ROOT_AGENT_ID.into(),
        depth: 1,
        task_name: "child".into(),
        session_path: manager.team_directory.join("child.jsonl"),
        status,
        created_at_ms: 1,
        started_at_ms: Some(1),
        ..DurableFleetRecord::default()
    };
    let (identity, commands) = insert_fixture_record(manager, durable, false, false, false);
    (
        identity,
        commands.expect("an attached fixture keeps its command receiver"),
    )
}

#[test]
fn active_worker_count_uses_host_liveness_without_presentation() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let binding = manager.root_binding();
    assert_eq!(binding.active_worker_count(), 0);
    for (index, status) in [
        DelegatedAgentStatus::Pending,
        DelegatedAgentStatus::Running,
        DelegatedAgentStatus::Idle,
        DelegatedAgentStatus::Interrupted,
        DelegatedAgentStatus::TimedOut,
        DelegatedAgentStatus::Shutdown,
    ]
    .into_iter()
    .enumerate()
    {
        let durable = DurableFleetRecord {
            agent_id: format!("agent-{index}"),
            status,
            ..DurableFleetRecord::default()
        };
        let (identity, _commands) = insert_fixture_record(&manager, durable, false, true, false);
        manager
            .state
            .lock()
            .unwrap()
            .records
            .get_mut(&identity.id)
            .unwrap()
            .shutdown
            .cancel();
    }
    assert_eq!(
        binding.active_worker_count(),
        6,
        "pending, idle, and cancelled-but-unwinding tasks still block reload"
    );
    {
        let mut state = manager.state.lock().unwrap();
        state.records.get_mut("agent-0").unwrap().live_task = false;
        state.records.get_mut("agent-1").unwrap().live_task = false;
    }
    assert_eq!(
        binding.active_worker_count(),
        4,
        "stale pending/running labels never manufacture host liveness"
    );
    assert!(manager.telemetry.lock().unwrap().sender.is_none());
}

/// A durable record reconstructed from the roster: detached, with its
/// parked command receiver owned by the record.
fn insert_durable_detached_record(
    manager: &DelegationManager,
    id: &str,
    path: &str,
    session_path: PathBuf,
    status: DelegatedAgentStatus,
) {
    let durable = DurableFleetRecord {
        agent_id: id.into(),
        agent_path: path.into(),
        parent_id: ROOT_AGENT_ID.into(),
        depth: 1,
        task_name: path.rsplit('/').next().unwrap_or("child").into(),
        session_path,
        status,
        created_at_ms: 1,
        started_at_ms: Some(1),
        ..DurableFleetRecord::default()
    };
    let (_identity, stored) = insert_fixture_record(manager, durable, true, false, true);
    assert!(
        stored.is_none(),
        "a detached fixture owns the receiver the record will use on reattach"
    );
}

#[tokio::test]
async fn reattachment_is_bounded_by_the_remaining_execution_slots() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    for index in 0..4 {
        let session_path = manager
            .team_directory
            .join(format!("reattach-{index}.jsonl"));
        Session::create(&session_path).unwrap();
        insert_durable_detached_record(
            &manager,
            &format!("agent-{}", index + 1),
            &format!("/root/worker-{index}"),
            session_path,
            DelegatedAgentStatus::Detached,
        );
    }
    // Only genuinely queued work needs an execution slot on reattachment.
    for record in manager.state.lock().unwrap().records.values_mut() {
        let QueuedTask::Initial(task) = QueuedTask::initial("undelivered work".into()) else {
            unreachable!()
        };
        record.pending_initial_task = Some(task);
    }
    assert_eq!(manager.current_permits().available_permits(), 3);

    manager.prepare_owning_run(&root_identity()).unwrap();

    let state = manager.state.lock().unwrap();
    let reattached = state
        .records
        .values()
        .filter(|record| {
            !record.detached && record.status == DelegatedAgentStatus::Pending && record.live_task
        })
        .count();
    let still_detached = state
        .records
        .values()
        .filter(|record| record.detached && record.status == DelegatedAgentStatus::Detached)
        .count();
    // The bound is authoritative: the excess record stays visibly detached
    // instead of oversubscribing the fleet, and no duplicate worker is
    // started for any record.
    assert_eq!(reattached, 3);
    assert_eq!(still_detached, 1);
    assert_eq!(state.records.len(), 4);
    // A record that could not take a slot now names why instead of
    // disappearing into a silent skip.
    let refused = state
        .records
        .values()
        .find(|record| record.detached && record.status == DelegatedAgentStatus::Detached)
        .and_then(|record| record.durable_diagnostic.as_deref())
        .expect("the bounded-out record names its refusal");
    assert!(refused.contains("no free execution slot"), "{refused}");
}

#[tokio::test]
async fn delivered_only_records_do_not_reserve_slots_a_runnable_sibling_needs() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    for index in 1..=4 {
        let session_path = manager
            .team_directory
            .join(format!("delivered-{index}.jsonl"));
        Session::create(&session_path).unwrap();
        insert_durable_detached_record(
            &manager,
            &format!("agent-{index}"),
            &format!("/root/delivered-{index}"),
            session_path,
            DelegatedAgentStatus::Detached,
        );
    }
    let session_path = manager.team_directory.join("undelivered.jsonl");
    Session::create(&session_path).unwrap();
    insert_durable_detached_record(
        &manager,
        "agent-5",
        "/root/undelivered",
        session_path,
        DelegatedAgentStatus::Detached,
    );
    let QueuedTask::Initial(task) = QueuedTask::initial("new work".into()) else {
        unreachable!()
    };
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut("agent-5")
        .unwrap()
        .pending_initial_task = Some(task);
    manager.prepare_owning_run(&root_identity()).unwrap();
    let state = manager.state.lock().unwrap();
    assert!(state.records["agent-5"].live_task);
    assert_eq!(
        state.records["agent-5"].status,
        DelegatedAgentStatus::Pending
    );
    assert_eq!(manager.current_permits().available_permits(), 2);
    for index in 1..=4 {
        let record = &state.records[&format!("agent-{index}")];
        assert_eq!(record.status, DelegatedAgentStatus::Interrupted);
        assert!(!record.live_task);
    }
    drop(state);
    let events = std::fs::read_to_string(manager.team_directory.join("provenance.jsonl")).unwrap();
    assert!(events.contains("\"state\":\"pending\""), "{events}");
    assert!(events.contains("\"state\":\"interrupted\""), "{events}");
    assert!(!events.contains("\"state\":\"idle\""), "{events}");
    manager.request_shutdown_descendants(ROOT_AGENT_ID);
}

#[tokio::test]
async fn reattachment_fails_closed_when_the_child_session_is_gone() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    insert_durable_detached_record(
        &manager,
        "agent-1",
        "/root/ghost",
        manager.team_directory.join("missing.jsonl"),
        DelegatedAgentStatus::Detached,
    );

    manager.prepare_owning_run(&root_identity()).unwrap();

    let state = manager.state.lock().unwrap();
    let record = &state.records["agent-1"];
    assert_eq!(record.status, DelegatedAgentStatus::Detached);
    assert!(record.detached);
    let diagnostic = record
        .durable_diagnostic
        .as_deref()
        .expect("a record whose session is gone must carry a bounded diagnostic");
    assert!(diagnostic.contains("was not reattached"), "{diagnostic}");
    assert!(diagnostic.contains("could not be reopened"), "{diagnostic}");
    drop(state);
    let listed = manager.list_value_for(&root_identity()).unwrap();
    let agent = &listed["agents"][0];
    assert_eq!(agent["status"]["state"], "detached");
    assert_eq!(agent["detached"], true);
    assert!(agent["diagnostic"]
        .as_str()
        .unwrap()
        .contains("was not reattached"));
}

#[tokio::test]
async fn reattachment_reopen_failure_does_not_strand_later_workers() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let missing = manager.team_directory.join("missing.jsonl");
    for (id, session_path) in [
        ("agent-1", missing.clone()),
        ("agent-2", manager.team_directory.join("healthy.jsonl")),
    ] {
        if id == "agent-2" {
            Session::create(&session_path).unwrap();
        }
        insert_durable_detached_record(
            &manager,
            id,
            &format!("/root/{id}"),
            session_path,
            DelegatedAgentStatus::Detached,
        );
    }
    // Only an undelivered task starts a recovered worker. A transcript
    // with no remaining work is interrupted for explicit continuation.
    for record in manager.state.lock().unwrap().records.values_mut() {
        record.pending_initial_task = Some(match QueuedTask::initial("uncommitted work".into()) {
            QueuedTask::Initial(task) => task,
            _ => unreachable!(),
        });
    }
    manager.prepare_owning_run(&root_identity()).unwrap();
    {
        let state = manager.state.lock().unwrap();
        let failed = &state.records["agent-1"];
        assert!(!failed.live_task);
        assert!(failed.detached_commands.is_some());
        assert!(!failed.command_tx.is_closed());
        let healthy = &state.records["agent-2"];
        assert!(healthy.live_task);
        assert_eq!(healthy.status, DelegatedAgentStatus::Pending);
        assert!(!healthy.command_tx.is_closed());
    }
    let roster: Value =
        serde_json::from_slice(&std::fs::read(manager.roster_path.as_ref().unwrap()).unwrap())
            .unwrap();
    assert_eq!(roster["records"][0]["status"]["state"], "detached");
    assert_eq!(roster["records"][1]["status"]["state"], "pending");
    // Exercise the later receiver, not just its pre-start liveness flag.
    let interrupted = manager
        .interrupt(&root_identity(), "agent-2")
        .await
        .unwrap();
    assert_eq!(interrupted["interrupt_requested"], true);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager.state.lock().unwrap().records["agent-2"].status
                == DelegatedAgentStatus::Interrupted
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Repair only the failed child and retry the next turn. Its receiver
    // was retained, so this starts exactly that worker rather than a ghost.
    Session::create(&missing).unwrap();
    manager.prepare_owning_run(&root_identity()).unwrap();
    let state = manager.state.lock().unwrap();
    assert!(state.records["agent-1"].live_task);
    assert_eq!(
        state.records["agent-1"].status,
        DelegatedAgentStatus::Pending
    );
    assert_eq!(
        state.records["agent-2"].status,
        DelegatedAgentStatus::Interrupted
    );
    drop(state);
    manager.request_shutdown_descendants(ROOT_AGENT_ID);
}

#[tokio::test]
async fn reattachment_journal_failure_keeps_unstarted_workers_recoverable() {
    for refuse_reopen in [false, true] {
        let server = continuation_test_server().await;
        let directory = tempfile::tempdir().unwrap();
        {
            let manager =
                manager_with_journal(read_only_journal(directory.path()), directory.path());
            for id in ["agent-1", "agent-2"] {
                let session_path = manager.team_directory.join(format!("{id}.jsonl"));
                if !refuse_reopen {
                    Session::create(&session_path).unwrap();
                }
                insert_durable_detached_record(
                    &manager,
                    id,
                    &format!("/root/{id}"),
                    session_path,
                    DelegatedAgentStatus::Detached,
                );
            }
            // Model the durable roster a real restored owner begins with.
            {
                let mut state = manager.state.lock().unwrap();
                manager.persist_durable_fleet_locked(&mut state);
            }
            // With missing transcripts the first failed append is the
            // refusal event; otherwise it is the selected worker status.
            let error = manager.prepare_owning_run(&root_identity()).unwrap_err();
            assert!(
                error.contains("could not persist reattachment provenance"),
                "{error}"
            );
            let state = manager.state.lock().unwrap();
            assert!(state.persistence_error.is_some());
            for record in state.records.values() {
                assert!(!record.live_task);
                assert!(record.detached);
                assert!(record.detached_commands.is_some());
                assert!(!record.command_tx.is_closed());
            }
            assert_eq!(manager.current_permits().available_permits(), 3);
            drop(state);
            assert!(
                manager.prepare_owning_run(&root_identity()).is_err(),
                "a provenance failure remains fail-closed in this manager"
            );
        }
        // Recovery is a fresh owner after the storage fault is repaired,
        // not an unsafe clearing of the failed manager's persistence gate.
        let recovered = continuation_test_manager(directory.path(), &server);
        if refuse_reopen {
            for id in ["agent-1", "agent-2"] {
                Session::create(recovered.team_directory.join(format!("{id}.jsonl"))).unwrap();
            }
        }
        recovered.restore_durable_fleet();
        recovered.prepare_owning_run(&root_identity()).unwrap();
        for id in ["agent-1", "agent-2"] {
            let accepted = recovered
                .follow_up(
                    &root_identity(),
                    FollowUpRequest {
                        target: id.into(),
                        message: format!("retry {id} after repairing storage"),
                    },
                )
                .await
                .unwrap();
            assert_eq!(accepted["delivery"], "new_run");
            wait_for_worker_status(
                &recovered,
                id,
                DelegatedAgentStatus::Completed {
                    output: "continued".into(),
                },
            )
            .await;
            assert_eq!(
                recovered.state.lock().unwrap().records[id]
                    .queued_follow_ups
                    .messages,
                0
            );
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        for id in ["agent-1", "agent-2"] {
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| String::from_utf8_lossy(&request.body)
                        .contains(&format!("retry {id} after repairing storage")))
                    .count(),
                1
            );
        }
        recovered.request_shutdown_descendants(ROOT_AGENT_ID);
    }
}

#[tokio::test(start_paused = true)]
async fn reattachment_enforces_the_persisted_wall_deadline_for_undelivered_work() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;
    for remaining_ms in [0, 1_000] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let session_path = root.join("budgeted.jsonl");
        Session::create(&session_path).unwrap();
        let absolute_deadline = u64::try_from(timestamp_ms()).unwrap() + remaining_ms;
        {
            let manager = continuation_test_manager(root, &server);
            insert_durable_detached_record(
                &manager,
                "agent-1",
                "/root/budgeted",
                session_path.clone(),
                DelegatedAgentStatus::Detached,
            );
            let mut state = manager.state.lock().unwrap();
            let record = state.records.get_mut("agent-1").unwrap();
            record.deadline_at_ms = Some(absolute_deadline);
            record.pending_initial_task =
                Some(match QueuedTask::initial("uncommitted task".into()) {
                    QueuedTask::Initial(task) => task,
                    _ => unreachable!(),
                });
            manager.persist_durable_fleet_locked(&mut state);
        }
        let manager = continuation_test_manager(root, &server);
        manager.restore_durable_fleet();
        manager.prepare_owning_run(&root_identity()).unwrap();
        if remaining_ms > 0 {
            assert_eq!(
                manager.state.lock().unwrap().records["agent-1"].status,
                DelegatedAgentStatus::Pending
            );
        }
        tokio::time::advance(Duration::from_millis(remaining_ms + 1)).await;
        for _ in 0..100 {
            if manager.state.lock().unwrap().records["agent-1"].status
                == DelegatedAgentStatus::TimedOut
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        {
            let state = manager.state.lock().unwrap();
            let record = &state.records["agent-1"];
            assert_eq!(record.status, DelegatedAgentStatus::TimedOut);
            assert!(record.live_task);
            assert_eq!(record.deadline_at_ms, Some(absolute_deadline));
            assert_eq!(record.turn_count, 0);
            assert!(!Session::open(&session_path)
                .unwrap()
                .entries()
                .iter()
                .any(|entry| matches!(
                    entry.value,
                    crate::EntryValue::Message(octet_ai::Message::Assistant(_))
                )));
        }
        manager.request_shutdown_descendants(ROOT_AGENT_ID);
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn reattached_follow_up_requires_a_free_execution_slot_before_acceptance() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let session_path = manager.team_directory.join("queued.jsonl");
    Session::create(&session_path).unwrap();
    insert_durable_detached_record(
        &manager,
        "agent-1",
        "/root/queued",
        session_path.clone(),
        DelegatedAgentStatus::Detached,
    );
    let deadline = u64::try_from(timestamp_ms()).unwrap() + 1_000;
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut("agent-1")
        .unwrap()
        .deadline_at_ms = Some(deadline);
    manager.prepare_owning_run(&root_identity()).unwrap();
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records["agent-1"];
        assert_eq!(record.status, DelegatedAgentStatus::Interrupted);
        assert_eq!(record.deadline_at_ms, Some(deadline));
    }
    // An interrupted, restored worker has no live receiver. Claiming all
    // slots must reject an explicit follow-up *before* journaling or queueing.
    let slots = manager.current_permits().try_acquire_many_owned(3).unwrap();
    let error = manager
        .follow_up(
            &root_identity(),
            FollowUpRequest {
                target: "agent-1".into(),
                message: "continue under the original budget".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("no free execution slot"), "{error}");
    assert!(manager.state.lock().unwrap().records["agent-1"]
        .pending_follow_ups
        .is_empty());
    assert!(Session::open(&session_path).unwrap().entries().is_empty());
    drop(slots);
    // A later explicit continuation is admitted, retaining the original
    // deadline rather than silently extending its wall budget.
    let result = manager
        .follow_up(
            &root_identity(),
            FollowUpRequest {
                target: "agent-1".into(),
                message: "continue under the original budget".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result["delivery"], "new_run");
    let state = manager.state.lock().unwrap();
    assert_eq!(state.records["agent-1"].deadline_at_ms, Some(deadline));
    assert_eq!(state.records["agent-1"].pending_follow_ups.len(), 1);
    drop(state);
    manager.request_shutdown_descendants(ROOT_AGENT_ID);
}

fn continuation_test_manager(directory: &Path, server: &MockServer) -> Arc<DelegationManager> {
    let mut manager = writable_manager_with_core_tools(directory);
    let template = &mut Arc::get_mut(&mut manager).unwrap().template;
    let endpoint = Arc::make_mut(&mut template.model.endpoint);
    endpoint.base_url = url::Url::parse(&format!("{}/", server.uri())).unwrap();
    endpoint.auth = octet_ai::Auth::None;
    manager
}

async fn continuation_test_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"continued\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
                    "data: [DONE]\n\n",
                )))
            .mount(&server).await;
    server
}

async fn wait_for_worker_status(
    manager: &DelegationManager,
    id: &str,
    status: DelegatedAgentStatus,
) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if manager.state.lock().unwrap().records[id].status == status {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {status:?}, observed {:?}",
            manager.state.lock().unwrap().records[id].status
        )
    });
}

#[tokio::test]
async fn timed_out_worker_continues_in_its_durable_session_without_resetting_policy() {
    terminal_worker_continuation(DelegatedAgentStatus::TimedOut).await;
}

#[tokio::test]
async fn cancelled_worker_continues_in_its_durable_session_without_resetting_policy() {
    terminal_worker_continuation(DelegatedAgentStatus::Interrupted).await;
}

async fn terminal_worker_continuation(terminal: DelegatedAgentStatus) {
    let server = continuation_test_server().await;
    let directory = tempfile::tempdir().unwrap();
    let manager = continuation_test_manager(directory.path(), &server);
    let session_path = manager.team_directory.join("continued.jsonl");
    let mut session = Session::create(&session_path).unwrap();
    session
        .append(crate::session::EntryValue::Message(
            octet_ai::Message::User(octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("prior durable evidence".into())],
            }),
        ))
        .unwrap();
    session.add_cost(17).unwrap();
    drop(session);
    insert_durable_detached_record(
        &manager,
        "agent-1",
        "/root/continued",
        session_path.clone(),
        DelegatedAgentStatus::Detached,
    );
    // Lifecycle recovery preserves wall/turn/tool policy without pretending a
    // finite input ceiling can dispatch on this route.
    let mut policy = test_extension_policy();
    policy.max_tokens = None;
    policy.max_cost_microdollars = None;
    let expired = terminal == DelegatedAgentStatus::TimedOut;
    let original_deadline = if expired {
        1
    } else {
        u64::try_from(timestamp_ms()).unwrap() + 300_000
    };
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut("agent-1").unwrap();
        record.extension_policy = Some(policy.clone());
        record.turn_limit = policy.max_turns;
        record.deadline_at_ms = Some(original_deadline);
        record.pending_initial_task = Some(match QueuedTask::initial("uncommitted work".into()) {
            QueuedTask::Initial(task) => task,
            _ => unreachable!(),
        });
    }
    manager.prepare_owning_run(&root_identity()).unwrap();
    if !expired {
        manager
            .interrupt(&root_identity(), "agent-1")
            .await
            .unwrap();
    }
    wait_for_worker_status(&manager, "agent-1", terminal).await;
    assert!(server.received_requests().await.unwrap().is_empty());
    let resumed = manager
        .follow_up(
            &root_identity(),
            FollowUpRequest {
                target: "agent-1".into(),
                message: "new work after settlement".into(),
            },
        )
        .await
        .expect("a terminal worker must accept durable continuation");
    assert_eq!(resumed["delivery"], "new_run");
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records["agent-1"];
        assert_eq!(record.status, DelegatedAgentStatus::Pending);
        assert!(record.completed_at_ms.is_none());
        assert_eq!(record.turn_limit, policy.max_turns);
        assert_eq!(
            serde_json::to_value(&record.extension_policy).unwrap(),
            serde_json::to_value(&policy).unwrap()
        );
        if expired {
            assert!(record.deadline_at_ms.unwrap() > u64::try_from(timestamp_ms()).unwrap());
        } else {
            assert_eq!(record.deadline_at_ms, Some(original_deadline));
        }
    }
    wait_for_worker_status(
        &manager,
        "agent-1",
        DelegatedAgentStatus::Completed {
            output: "continued".into(),
        },
    )
    .await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(request.to_string().contains("prior durable evidence"));
    assert!(request.to_string().contains("new work after settlement"));
    let session = Session::open_read_only(&session_path).unwrap();
    assert!(session.total_cost_microdollars() >= 17);
    let journal = std::fs::read_to_string(manager.team_directory.join("provenance.jsonl")).unwrap();
    assert!(journal
        .lines()
        .any(|line| line.contains("new work after settlement") && line.contains("follow_up")));
    let state = manager.state.lock().unwrap();
    assert_eq!(state.records["agent-1"].queued_follow_ups.messages, 0);
    assert_eq!(state.records["agent-1"].session_path, session_path);
    drop(state);
    manager.request_shutdown_descendants(ROOT_AGENT_ID);
}

#[tokio::test(start_paused = true)]
async fn host_deadline_adoption_initializes_missing_local_budget_without_extending_it() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (identity, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Idle);
    let mut deadline = None;
    let mut deadline_ms = None;
    manager.adopt_host_deadline(&identity, &mut deadline, &mut deadline_ms);
    assert!(deadline.is_none(), "an unlimited worker remains unlimited");
    let host = u64::try_from(timestamp_ms()).unwrap() + 10_000;
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut(&identity.id)
        .unwrap()
        .deadline_at_ms = Some(host);
    manager.adopt_host_deadline(&identity, &mut deadline, &mut deadline_ms);
    assert_eq!(deadline_ms, Some(host));
    let initial = deadline.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    manager.adopt_host_deadline(&identity, &mut deadline, &mut deadline_ms);
    assert_eq!(
        deadline,
        Some(initial),
        "polling never grants more wall time"
    );
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut(&identity.id)
        .unwrap()
        .deadline_at_ms = Some(host + 10_000);
    manager.adopt_host_deadline(&identity, &mut deadline, &mut deadline_ms);
    assert_eq!(deadline_ms, Some(host + 10_000));
    assert!(deadline.unwrap() > initial);
}

/// A restart with the same durable store: a fresh manager rebuilds the
/// fleet, reattaches the worker under a new claim generation, and keeps its
/// task, limits, and accounting.
#[tokio::test]
async fn restart_reattaches_the_durable_worker_with_its_accounting_and_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let session_path = root.join("reattach-child.jsonl");
    let mut child_session = Session::create(&session_path).unwrap();
    let model = test_template(&root).model;
    child_session
        .record_compaction_usage(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage {
                input_tokens: 11,
                output_tokens: 5,
                total_tokens: 16,
                ..Usage::default()
            },
            Some(Cost {
                total: 42,
                input: 42,
                ..Cost::default()
            }),
        )
        .unwrap();
    child_session
        .record_usage_uncertainty(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            "interrupted_child",
        )
        .unwrap();
    drop(child_session);
    let old_claim = DurableFleetClaim {
        generation: 1,
        instance: "old-owner".into(),
    };
    let provenance = root.join("provenance.jsonl");
    {
        let manager = writable_manager(&root);
        insert_durable_detached_record(
            &manager,
            "agent-1",
            "/root/survivor",
            session_path.clone(),
            DelegatedAgentStatus::Detached,
        );
        {
            let mut state = manager.state.lock().unwrap();
            let record = state.records.get_mut("agent-1").unwrap();
            record.turn_count = 3;
            record.tool_call_count = 7;
            record.usage = Usage {
                input_tokens: 11,
                output_tokens: 5,
                total_tokens: 16,
                ..Usage::default()
            };
            record.usage_uncertain = true;
            record.cost_microdollars = Some(42);
            record.turn_limit = Some(9);
            record.deadline_at_ms = Some(1);
            record.claim = Some(old_claim.clone());
            manager.persist_durable_fleet_locked(&mut state);
        }
    }
    // The old process is gone: the next manager takes the next generation.
    let manager = writable_manager(&root);
    manager.restore_durable_fleet();
    manager.prepare_owning_run(&root_identity()).unwrap();
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records["agent-1"];
        assert_eq!(record.status, DelegatedAgentStatus::Interrupted);
        assert!(!record.detached);
        assert!(!record.live_task);
        assert!(record
            .durable_diagnostic
            .as_deref()
            .unwrap()
            .contains("subagent_continue"));
        let claim = record.claim.as_ref().expect("reattached record is claimed");
        assert!(claim.generation > old_claim.generation, "{claim:?}");
        assert_ne!(claim.instance, old_claim.instance);
        // Turn, limit, and cost/usage accounting survive the restart.
        assert_eq!(record.turn_count, 3);
        assert_eq!(record.tool_call_count, 7);
        assert_eq!(record.usage.input_tokens, 11);
        assert_eq!(record.usage.total_tokens, 16);
        assert!(record.usage_uncertain);
        assert_eq!(record.cost.unwrap().total, 42);
        assert_eq!(
            record.cost_microdollars, None,
            "known cost is only a subtotal"
        );
        assert!(record.usage_exposure.is_none());
        assert_eq!(record.turn_limit, Some(9));
        assert_eq!(record.deadline_at_ms, Some(1));
    }
    let events = std::fs::read_to_string(&provenance).unwrap();
    assert!(
        events.contains("\"event\":\"run_reattached\""),
        "reattachment is an explicit lifecycle boundary: {events}"
    );
    assert!(
        events.contains("\"agent_id\":\"agent-1\""),
        "the reattached worker is named in the journal: {events}"
    );
}

/// A worker parked at the approval boundary is rediscovered, never resumed
/// by reattachment, and resumed only by an explicit follow-up decision.
#[tokio::test]
async fn a_worker_parked_on_approval_stays_parked_until_a_decision_arrives() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let session_path = root.join("parked-child.jsonl");
    Session::create(&session_path).unwrap();
    {
        let manager = writable_manager(&root);
        insert_durable_detached_record(
            &manager,
            "agent-1",
            "/root/parked",
            session_path,
            DelegatedAgentStatus::AwaitingApproval {
                reason: "tool effect requires new authority".into(),
            },
        );
        let mut state = manager.state.lock().unwrap();
        manager.persist_durable_fleet_locked(&mut state);
    }
    let manager = writable_manager(&root);
    manager.restore_durable_fleet();
    manager.prepare_owning_run(&root_identity()).unwrap();
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records["agent-1"];
        assert!(matches!(
            record.status,
            DelegatedAgentStatus::AwaitingApproval { .. }
        ));
        assert!(record.detached);
        assert!(!record.live_task, "a parked worker is not silently resumed");
        let diagnostic = record
            .durable_diagnostic
            .as_deref()
            .expect("a park names why it was not resumed");
        assert!(diagnostic.contains("explicit decision"), "{diagnostic}");
        assert!(
            diagnostic.contains("tool effect requires new authority"),
            "{diagnostic}"
        );
    }
    // The decision is supplied explicitly: the parked worker resumes.
    let resumed = manager
        .follow_up(
            &root_identity(),
            FollowUpRequest {
                target: "agent-1".into(),
                message: "approved: proceed".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(resumed["delivery"], "new_run");
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records["agent-1"];
        // The resumed worker runs concurrently; the park is what must be
        // gone, never the worker's own live status.
        assert!(
            !matches!(record.status, DelegatedAgentStatus::AwaitingApproval { .. }),
            "the explicit decision clears the park"
        );
        assert!(record.live_task, "the explicit decision resumes the worker");
        assert!(!record.detached);
        assert!(record.durable_diagnostic.is_none());
    }
}

/// The fence primitive itself: one live claimant per session, monotonic
/// generations, and no contention between sessions sharing a directory.
#[test]
fn fleet_lease_is_exclusive_per_session_and_bumps_its_generation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let session = root.join("root.jsonl");
    let first = FleetLease::try_acquire(&root, &session).expect("first claimant");
    assert!(first.is_current().is_ok());
    assert_eq!(first.claim().generation, 1);

    let error = FleetLease::try_acquire(&root, &session).unwrap_err();
    assert!(
        error.contains("another live session owner holds the durable fleet lease"),
        "{error}"
    );

    // A different root session in the same delegation directory never
    // contends: the lease is scoped per session.
    let other = FleetLease::try_acquire(&root, &root.join("other.jsonl"))
        .expect("a second session owns its own fleet");
    drop(other);

    drop(first);
    let next = FleetLease::try_acquire(&root, &session).expect("after release");
    assert!(next.is_current().is_ok());
    assert!(
        next.claim().generation > 1,
        "a new owner takes the next generation"
    );
}

#[tokio::test]
async fn admitted_running_steering_and_drained_prompt_survive_restart() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let manager = writable_manager(root);
    Session::create(manager.team_directory.join("child.jsonl")).unwrap();
    let (child, mut commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    manager.persist_durable_fleet_locked(&mut manager.state.lock().unwrap());

    manager
        .send_message(&root_identity(), &child.id, "identical".into())
        .await
        .unwrap();
    manager
        .send_message(&root_identity(), &child.id, "identical".into())
        .await
        .unwrap();
    let first = commands.recv().await.unwrap();
    assert!(matches!(first.kind, WorkerCommandKind::Message(_)));
    // Simulate the worker taking one notification and draining its pending
    // prompt without ever committing it to the child session.
    if let WorkerCommandKind::Message(message) = first.kind {
        manager.queue_reserved_message(&child.id, message);
    }
    let drained = manager.take_pending_messages(&child.id);
    assert_eq!(
        drained
            .iter()
            .map(|m| m.message.as_str())
            .collect::<Vec<_>>(),
        vec!["identical"]
    );
    // A concurrent status update can refresh the roster before this prompt
    // commits; the in-flight payload must still survive that snapshot.
    manager.persist_durable_fleet_locked(&mut manager.state.lock().unwrap());
    let durable: DurableFleet =
        serde_json::from_slice(&std::fs::read(manager.roster_path.as_ref().unwrap()).unwrap())
            .unwrap();
    assert_eq!(durable.records[0].pending_messages.len(), 2);
    let ids = durable.records[0]
        .pending_messages
        .iter()
        .map(|m| m.delivery_id.clone())
        .collect::<Vec<_>>();
    assert_ne!(
        ids[0], ids[1],
        "identical steering text is distinct accepted work"
    );
    drop(commands);
    drop(manager);

    let restarted = writable_manager(root);
    restarted.restore_durable_fleet();
    let state = restarted.state.lock().unwrap();
    let messages = &state.records[&child.id].pending_messages;
    assert_eq!(
        messages
            .iter()
            .map(|m| m.message.as_str())
            .collect::<Vec<_>>(),
        vec!["identical", "identical"]
    );
    assert_eq!(
        messages
            .iter()
            .map(|m| m.delivery_id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert!(state.records[&child.id].inflight_message_ids.is_empty());
    drop(state);
    drop(restarted);

    // If the child session committed only the first envelope before the
    // next crash, ancestry reconciliation removes exactly that delivery.
    let mut session = Session::open(root.join("child.jsonl")).unwrap();
    session
        .append(crate::EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text(format_direct_message(&drained[0]))],
            },
        )))
        .unwrap();
    drop(session);
    let reconciled = writable_manager(root);
    reconciled.restore_durable_fleet();
    let state = reconciled.state.lock().unwrap();
    let messages = &state.records[&child.id].pending_messages;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].delivery_id, ids[1]);
    assert_eq!(messages[0].message, "identical");
}

#[tokio::test]
async fn observer_cannot_admit_spawn_and_reloads_newer_owner_changes_on_takeover() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let owner = writable_manager(root);
    Session::create(owner.team_directory.join("child.jsonl")).unwrap();
    let (child, _commands) = insert_test_record(
        &owner,
        DelegatedAgentStatus::Completed {
            output: "done".into(),
        },
    );
    owner.persist_durable_fleet_locked(&mut owner.state.lock().unwrap());
    let observer = writable_manager(root);
    observer.restore_durable_fleet();
    assert!(observer.lease_refusal_reason().is_some());
    let before = std::fs::read(owner.roster_path.as_ref().unwrap()).unwrap();
    let refusal = observer
        .spawn(
            &root_identity(),
            SpawnRequest {
                task_name: "unowned".into(),
                display_task_name: None,
                message: "do not start".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();
    assert!(
        refusal.contains("another live session owner holds"),
        "{refusal}"
    );
    assert_eq!(
        std::fs::read(owner.roster_path.as_ref().unwrap()).unwrap(),
        before
    );
    assert_eq!(observer.state.lock().unwrap().records.len(), 1);
    assert!(!observer.team_directory.join("0001-unowned.jsonl").exists());

    // The observer opened before these admissions, so its snapshot is old.
    owner
        .send_message(&root_identity(), &child.id, "newer input".into())
        .await
        .unwrap();
    owner
        .send_message(&root_identity(), ROOT_AGENT_ID, "newer mailbox".into())
        .await
        .unwrap();
    let second_session = owner.team_directory.join("second.jsonl");
    Session::create(&second_session).unwrap();
    insert_durable_detached_record(
        &owner,
        "agent-2",
        "/root/second",
        second_session,
        DelegatedAgentStatus::Completed {
            output: "newer record".into(),
        },
    );
    owner.persist_durable_fleet_locked(&mut owner.state.lock().unwrap());
    let authoritative: DurableFleet =
        serde_json::from_slice(&std::fs::read(owner.roster_path.as_ref().unwrap()).unwrap())
            .unwrap();
    let delivery_id = authoritative.records[0].pending_messages[0]
        .delivery_id
        .clone();
    let old_generation = owner.current_claim().unwrap().generation;
    drop(owner);
    observer.prepare_owning_run(&root_identity()).unwrap();
    let state = observer.state.lock().unwrap();
    let recovered = &state.records[&child.id];
    assert_eq!(recovered.pending_messages[0].message, "newer input");
    assert_eq!(recovered.pending_messages[0].delivery_id, delivery_id);
    assert_eq!(state.root_mailbox[0].message, "newer mailbox");
    assert_eq!(
        state.next_mailbox_delivery,
        authoritative.next_mailbox_delivery
    );
    assert_eq!(state.records.len(), 2);
    assert!(matches!(
        state.records["agent-2"].status,
        DelegatedAgentStatus::Completed { .. }
    ));
    assert_eq!(state.next_agent_number, 3);
    assert_eq!(
        recovered.status,
        DelegatedAgentStatus::Completed {
            output: "done".into()
        }
    );
    drop(state);
    let persisted: DurableFleet =
        serde_json::from_slice(&std::fs::read(observer.roster_path.as_ref().unwrap()).unwrap())
            .unwrap();
    assert_eq!(
        persisted.records[0].pending_messages[0].delivery_id,
        delivery_id
    );
    assert!(observer.current_claim().unwrap().generation > old_generation);
}

#[tokio::test]
async fn stale_fleet_claim_refuses_admission_without_touching_the_roster() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let roster = manager.roster_path.as_ref().unwrap();
    let (lock_path, lease_path) = fleet_lease_paths(directory.path(), Path::new(""));
    assert!(lock_path.exists());
    let before = std::fs::read(&lease_path).unwrap();
    let mut claim: DurableFleetLease = serde_json::from_slice(&before).unwrap();
    claim.generation += 1;
    std::fs::write(&lease_path, serde_json::to_vec(&claim).unwrap()).unwrap();
    let error = manager
        .spawn(
            &root_identity(),
            SpawnRequest {
                task_name: "stale".into(),
                display_task_name: None,
                message: "no unowned worker".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();
    assert!(error.contains("newer owner"), "{error}");
    assert!(manager.state.lock().unwrap().records.is_empty());
    assert!(!manager.team_directory.join("0001-stale.jsonl").exists());
    assert!(!roster.exists());
}

#[tokio::test]
async fn unreadable_claimed_roster_cannot_be_overwritten_by_spawn() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let roster = manager.roster_path.as_ref().unwrap();
    std::fs::write(roster, b"corrupt authoritative roster").unwrap();
    manager.restore_durable_fleet();
    let error = manager
        .spawn(
            &root_identity(),
            SpawnRequest {
                task_name: "blocked".into(),
                display_task_name: None,
                message: "do not replace the roster".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();
    assert!(
        error.contains("delegation persistence is unavailable"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(roster).unwrap(),
        b"corrupt authoritative roster"
    );
    assert!(!manager.team_directory.join("0001-blocked.jsonl").exists());
}

/// A second claimant of the same session's durable fleet is refused by
/// name; it never starts a worker the first owner is already running.
#[tokio::test]
async fn a_second_claimant_of_the_session_fleet_is_refused_by_name() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let session_path = root.join("claimed-child.jsonl");
    Session::create(&session_path).unwrap();
    let owner = writable_manager(&root);
    insert_durable_detached_record(
        &owner,
        "agent-1",
        "/root/single-writer",
        session_path,
        DelegatedAgentStatus::Detached,
    );
    {
        let mut state = owner.state.lock().unwrap();
        let QueuedTask::Initial(task) = QueuedTask::initial("undelivered work".into()) else {
            unreachable!()
        };
        state
            .records
            .get_mut("agent-1")
            .unwrap()
            .pending_initial_task = Some(task);
        owner.persist_durable_fleet_locked(&mut state);
    }
    owner.prepare_owning_run(&root_identity()).unwrap();
    assert!(
        owner.state.lock().unwrap().records["agent-1"].live_task,
        "the first owner reattaches and owns the worker"
    );

    // A duplicate session open cannot take the same durable fleet.
    let duplicate = writable_manager(&root);
    duplicate.restore_durable_fleet();
    duplicate.prepare_owning_run(&root_identity()).unwrap();
    {
        let state = duplicate.state.lock().unwrap();
        let record = &state.records["agent-1"];
        assert!(!record.live_task, "the duplicate never starts the worker");
        assert!(record.detached, "the record stays visibly detached");
        let diagnostic = record
            .durable_diagnostic
            .as_deref()
            .expect("a refused claimant names why");
        assert!(
            diagnostic.contains("another live session owner holds the durable fleet lease"),
            "{diagnostic}"
        );
        assert!(duplicate.lease_refusal_reason().is_some());
    }
    assert!(
        owner.state.lock().unwrap().records["agent-1"].live_task,
        "the first owner's live worker is untouched"
    );
}

/// A claim that cannot be proven fresh fails closed: a record written by a
/// newer fleet generation is never started by an older owner.
#[tokio::test]
async fn reattachment_refuses_a_record_claimed_by_a_newer_generation() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let session_path = manager.team_directory.join("stale-claim.jsonl");
    Session::create(&session_path).unwrap();
    insert_durable_detached_record(
        &manager,
        "agent-1",
        "/root/stale-claim",
        session_path,
        DelegatedAgentStatus::Detached,
    );
    {
        let mut state = manager.state.lock().unwrap();
        state.records.get_mut("agent-1").unwrap().claim = Some(DurableFleetClaim {
            generation: 99,
            instance: "newer-owner".into(),
        });
    }

    manager.prepare_owning_run(&root_identity()).unwrap();

    let state = manager.state.lock().unwrap();
    let record = &state.records["agent-1"];
    assert!(!record.live_task, "a stale claim fails closed");
    assert!(record.detached);
    let diagnostic = record
        .durable_diagnostic
        .as_deref()
        .expect("a fenced record names why");
    assert!(
        diagnostic.contains("newer session fleet owner"),
        "{diagnostic}"
    );
}

/// The accepted-task modes are distinct and must not drift.
///
/// `delivery` names the task's path through the worker's queue, never a
/// fresh identity: `new_run` reopens a *settled* worker (status flipped back
/// to `pending`, completion timestamp cleared, deadline re-anchored) while
/// `follow_up` joins an already live worker (`pending`/`running`) whose queue
/// was simply empty. Both use the same identity, the same durable child
/// session, and the same accounting.
#[tokio::test]
async fn accepted_task_modes_distinguish_a_reopened_worker_from_a_live_one() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let owner = root_identity();
    let (identity, _commands) = insert_test_record(
        &manager,
        DelegatedAgentStatus::Completed {
            output: "first run settled".into(),
        },
    );
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&identity.id).unwrap();
        // The worker task is still alive in this process (a settled but
        // attached worker), so the record is not suspended.
        record.live_task = true;
        record.turn_count = 2;
    }
    let reopened = manager
        .follow_up(
            &owner,
            FollowUpRequest {
                target: identity.id.clone(),
                message: "reopen the settled worker".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(reopened["delivery"], "new_run");
    assert_eq!(reopened["agent_id"], identity.id);
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records[&identity.id];
        assert_eq!(record.status, DelegatedAgentStatus::Pending);
        assert_eq!(
            record.turn_count, 2,
            "a reopened run never resets accounting"
        );
    }

    // The same worker is now live with an empty task queue: the next task
    // joins it instead of reopening it.
    let joined = manager
        .follow_up(
            &owner,
            FollowUpRequest {
                target: identity.id.clone(),
                message: "join the live worker".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(joined["delivery"], "follow_up");
    assert_eq!(joined["agent_id"], identity.id);
    let state = manager.state.lock().unwrap();
    let record = &state.records[&identity.id];
    assert_eq!(record.status, DelegatedAgentStatus::Pending);
    assert_eq!(record.turn_count, 2);
    assert_eq!(record.queued_follow_ups.messages, 2);
}

/// A live worker whose owning session disappears parks as a recoverable
/// record instead of retiring, so the next owner can reattach it.
#[tokio::test]
async fn a_released_session_parks_its_live_worker_for_reattachment() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    Session::create(manager.team_directory.join("child.jsonl")).unwrap();
    let (identity, commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&identity.id).unwrap();
        record.detached = false;
        record.live_task = true;
        state.session_owner_released = false;
    }
    assert!(
        !manager.park_released_worker(&identity.id, None),
        "an attached session keeps its live worker attached"
    );

    manager.request_shutdown_descendants(ROOT_AGENT_ID);
    assert!(manager.session_owner_released());
    assert!(manager.state.lock().unwrap().records[&identity.id]
        .shutdown
        .is_cancelled());
    assert!(manager.park_released_worker(&identity.id, Some(commands)));
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records[&identity.id];
        assert_eq!(record.status, DelegatedAgentStatus::Detached);
        assert!(record.detached);
        assert!(!record.live_task);
        let diagnostic = record.durable_diagnostic.as_deref().unwrap();
        assert!(
            diagnostic.contains("retained for reattachment"),
            "{diagnostic}"
        );
        assert!(record.detached_commands.is_some());
    }
    // The parked record is durable: a later owner reads it back.
    let roster = std::fs::read_to_string(manager.roster_path.as_ref().unwrap()).unwrap();
    assert!(roster.contains("\"agent_id\":\"agent-1\""));
    assert!(roster.contains("\"state\":\"detached\""));
}

#[test]
fn releasing_a_settled_worker_retains_terminal_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let status = DelegatedAgentStatus::Completed {
        output: "verified result".into(),
    };
    let (identity, commands) = insert_test_record(&manager, status.clone());
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut(&identity.id)
        .unwrap()
        .completed_at_ms = Some(7);
    manager.request_shutdown_descendants(ROOT_AGENT_ID);
    assert!(manager.park_released_worker(&identity.id, Some(commands)));
    let state = manager.state.lock().unwrap();
    let record = &state.records[&identity.id];
    assert_eq!(record.status, status);
    assert_eq!(record.completed_at_ms, Some(7));
    assert!(record.detached);
    assert_eq!(durable_fleet_record(record).status, status);
}

#[tokio::test]
async fn different_roots_share_a_directory_without_overwriting_fleets() {
    let directory = tempfile::tempdir().unwrap();
    let shared = directory.path().join(".delegation");
    let first_root = directory.path().join("first.jsonl");
    let second_root = directory.path().join("second.jsonl");
    let create = |root: &Path| {
        DelegationManager::create(
            DelegationConfig::new(&shared),
            test_template(directory.path()),
            root,
            false,
        )
        .unwrap()
    };
    let first = create(&first_root);
    let second = create(&second_root);
    assert!(first.lease_held() && second.lease_held());
    assert_ne!(first.roster_path, second.roster_path);
    for (manager, output) in [(&first, "first"), (&second, "second")] {
        Session::create(manager.team_directory.join("child.jsonl")).unwrap();
        insert_test_record(
            manager,
            DelegatedAgentStatus::Completed {
                output: output.into(),
            },
        );
        manager.persist_durable_fleet_locked(&mut manager.state.lock().unwrap());
    }
    std::fs::write(
        shared.join("fleet-0000000000000000.json"),
        b"invalid unrelated roster",
    )
    .unwrap();
    for manager in [&first, &second] {
        let path = manager.state.lock().unwrap().records["agent-1"]
            .session_path
            .clone();
        let reference = delegated_session_reference(&path).unwrap();
        assert_eq!(
            resolve_launchable_child_session(&shared, &reference)
                .unwrap()
                .session_path,
            path
        );
    }
    drop(first);
    drop(second);
    for (root, expected) in [(&first_root, "first"), (&second_root, "second")] {
        let restored = create(root);
        assert_eq!(
            restored.state.lock().unwrap().records["agent-1"].status,
            DelegatedAgentStatus::Completed {
                output: expected.into()
            }
        );
    }
}

#[test]
fn legacy_roster_migrates_only_its_owner_and_never_shadows_new_state() {
    let directory = tempfile::tempdir().unwrap();
    let shared = directory.path().join(".delegation");
    let root = directory.path().join("root.jsonl");
    let create = |root: &Path| {
        DelegationManager::create(
            DelegationConfig::new(&shared),
            test_template(directory.path()),
            root,
            false,
        )
        .unwrap()
    };
    let first = create(&root);
    Session::create(first.team_directory.join("child.jsonl")).unwrap();
    insert_test_record(
        &first,
        DelegatedAgentStatus::Completed {
            output: "legacy".into(),
        },
    );
    first.persist_durable_fleet_locked(&mut first.state.lock().unwrap());
    let scoped = first.roster_path.clone().unwrap();
    drop(first);
    let legacy = shared.join(FLEET_ROSTER_FILE);
    std::fs::rename(&scoped, &legacy).unwrap();
    let original = std::fs::read(&legacy).unwrap();
    let other = create(&directory.path().join("other.jsonl"));
    assert!(other.state.lock().unwrap().records.is_empty());
    let migrated = create(&root);
    {
        let mut state = migrated.state.lock().unwrap();
        assert_eq!(
            state.records["agent-1"].status,
            DelegatedAgentStatus::Completed {
                output: "legacy".into()
            }
        );
        state.records.get_mut("agent-1").unwrap().status = DelegatedAgentStatus::Completed {
            output: "current".into(),
        };
        migrated.persist_durable_fleet_locked(&mut state);
    }
    drop(migrated);
    let reopened = create(&root);
    assert_eq!(
        reopened.state.lock().unwrap().records["agent-1"].status,
        DelegatedAgentStatus::Completed {
            output: "current".into()
        }
    );
    assert_eq!(std::fs::read(legacy).unwrap(), original);
}

#[test]
fn roster_bound_fits_admitted_outputs_and_json_escaping() {
    let record = DurableFleetRecord {
        status: DelegatedAgentStatus::Completed {
            output: "\u{0001}".repeat(MAX_PROVENANCE_TEXT_BYTES),
        },
        durable_diagnostic: Some("\u{0001}".repeat(MAX_PROVENANCE_TEXT_BYTES)),
        ..DurableFleetRecord::default()
    };
    let bytes = serde_json::to_vec(&record).unwrap().len();
    assert!(bytes * 255 + 64 * 1024 <= MAX_FLEET_ROSTER_BYTES);
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    {
        let mut state = manager.state.lock().unwrap();
        for i in 1..32 {
            let (tx, rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
            let durable = DurableFleetRecord {
                agent_id: format!("agent-{i}"),
                agent_path: format!("/root/{i}"),
                status: DelegatedAgentStatus::Completed {
                    output: "x".repeat(16 * 1024),
                },
                ..DurableFleetRecord::default()
            };
            state.records.insert(
                durable.agent_id.clone(),
                DelegationManager::agent_record_from_durable(
                    durable,
                    test_effective_tool_policy(),
                    None,
                    tx,
                    Some(rx),
                ),
            );
        }
        manager.persist_durable_fleet_locked(&mut state);
        assert!(state.persistence_error.is_none());
    }
    let bytes = std::fs::read(manager.roster_path.as_ref().unwrap()).unwrap();
    assert!(bytes.len() <= ROSTER_PROJECTION_BYTES);
    assert_eq!(
        serde_json::from_slice::<DurableFleet>(&bytes)
            .unwrap()
            .records
            .len(),
        31
    );
}

#[tokio::test]
async fn a_session_owned_worker_exposes_a_launchable_handle() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    // A host-owned delegated session lives in a private `team-*` directory;
    // its opaque reference is derived from that directory and the filename.
    let team = directory.path().join("team-launchable");
    std::fs::create_dir(&team).unwrap();
    let session_path = team.join("0001-worker.jsonl");
    Session::create(&session_path).unwrap();
    insert_durable_detached_record(
        &manager,
        "agent-1",
        "/root/worker",
        session_path.clone(),
        DelegatedAgentStatus::Detached,
    );
    let reference = delegated_session_reference(&session_path).unwrap();
    // The handle is a boring, quotable, argv-safe token: no path, no
    // secret, no shell metacharacter.
    assert_eq!(reference.len(), "agent-session:".len() + 64);
    assert!(reference.starts_with("agent-session:"));
    assert!(reference
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b':'));
    assert!(!reference.contains('/'));

    let handle = manager.launchable_child_session(&reference).unwrap();
    assert_eq!(handle.reference, reference);
    assert_eq!(handle.session_path, session_path);
    assert_eq!(handle.agent_id, "agent-1");
    assert_eq!(handle.agent_path, "/root/worker");
    assert_eq!(handle.status, "detached");

    // A live worker owns the transcript in this process: refuse.
    {
        let mut state = manager.state.lock().unwrap();
        state.records.get_mut("agent-1").unwrap().live_task = true;
    }
    let blocked = manager.launchable_child_session(&reference).unwrap_err();
    assert!(
        blocked
            .to_string()
            .contains("live worker owns this session"),
        "{blocked}"
    );

    // A parked worker must not be opened for unattended mutation.
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut("agent-1").unwrap();
        record.live_task = false;
        record.status = DelegatedAgentStatus::AwaitingApproval {
            reason: "approval is unavailable".into(),
        };
    }
    let parked = manager.launchable_child_session(&reference).unwrap_err();
    assert!(parked.to_string().contains("approval boundary"), "{parked}");

    // Unknown and malformed handles fail closed with bounded diagnostics.
    let unknown = format!("agent-session:{}", "0".repeat(64));
    assert!(manager
        .launchable_child_session(&unknown)
        .unwrap_err()
        .to_string()
        .contains("unknown worker handle"));
    assert!(manager
        .launchable_child_session("agent-session:not-hex")
        .unwrap_err()
        .to_string()
        .contains("64 lowercase hex"));
    assert!(manager
        .launchable_child_session("/root/worker")
        .unwrap_err()
        .to_string()
        .contains("must be agent-session:<sha256>"));
}

#[tokio::test]
async fn the_durable_roster_resolves_a_launchable_handle_without_a_live_agent() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let team = directory.path().join("team-roster");
    std::fs::create_dir(&team).unwrap();
    let session_path = team.join("0001-worker.jsonl");
    Session::create(&session_path).unwrap();
    insert_durable_detached_record(
        &manager,
        "agent-1",
        "/root/worker",
        session_path.clone(),
        DelegatedAgentStatus::Detached,
    );
    {
        let mut state = manager.state.lock().unwrap();
        manager.persist_durable_fleet_locked(&mut state);
    }
    let reference = delegated_session_reference(&session_path).unwrap();
    let session_directory = manager.config.session_directory.clone();

    // A separate process needs only the session directory and the handle.
    let handle = resolve_launchable_child_session(&session_directory, &reference).unwrap();
    assert_eq!(handle.session_path, session_path);
    assert_eq!(handle.agent_id, "agent-1");
    assert_eq!(handle.agent_path, "/root/worker");

    // A parked record in the roster is refused before any launch happens.
    {
        let bytes = std::fs::read(manager.roster_path.as_ref().unwrap()).unwrap();
        let parked = String::from_utf8(bytes).unwrap().replace(
            "\"state\":\"detached\"",
            "\"state\":\"awaiting_approval\",\"reason\":\"approval is unavailable\"",
        );
        secure_fs::write_private_atomic(
            manager.roster_path.as_ref().unwrap(),
            parked.as_bytes(),
            MAX_FLEET_ROSTER_BYTES,
        )
        .unwrap();
    }
    let parked = resolve_launchable_child_session(&session_directory, &reference).unwrap_err();
    assert!(parked.to_string().contains("approval boundary"), "{parked}");

    // A vanished transcript fails closed rather than fabricating a launch.
    let missing = tempfile::tempdir().unwrap();
    let bytes = std::fs::read(manager.roster_path.as_ref().unwrap()).unwrap();
    let body = String::from_utf8(bytes)
        .unwrap()
        .replace("\"state\":\"awaiting_approval\"", "\"state\":\"detached\"");
    secure_fs::write_private_atomic(
        &missing.path().join(FLEET_ROSTER_FILE),
        body.as_bytes(),
        MAX_FLEET_ROSTER_BYTES,
    )
    .unwrap();
    std::fs::remove_file(&session_path).unwrap();
    let gone = resolve_launchable_child_session(missing.path(), &reference).unwrap_err();
    assert!(gone.to_string().contains("session file is gone"), "{gone}");

    // No roster at all is an explicit refusal, not an empty success.
    let empty = tempfile::tempdir().unwrap();
    assert!(resolve_launchable_child_session(empty.path(), &reference)
        .unwrap_err()
        .to_string()
        .contains("no session-owned delegation roster"));
}

#[tokio::test]
async fn reusing_a_session_scoped_worker_name_names_the_resume_path() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    insert_test_record(
        &manager,
        DelegatedAgentStatus::Completed {
            output: "first run settled".into(),
        },
    );

    let error = manager
        .spawn(
            &root_identity(),
            SpawnRequest {
                task_name: "child".into(),
                display_task_name: None,
                message: "do it again".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();

    assert!(
        error.contains("task name already exists under /root: child"),
        "{error}"
    );
    assert!(error.contains("agent-1"), "{error}");
    assert!(error.contains("followup_task"), "{error}");
}

#[test]
fn limit_reached_status_preserves_output_budget_and_parent_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _command_rx) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&child.id).unwrap();
        record.turn_count = 2;
        record.turn_limit = Some(2);
    }

    assert!(manager.set_status(
        &child.id,
        DelegatedAgentStatus::LimitReached {
            output: "partial answer".into(),
            turn_count: 2,
            turn_limit: 2,
        },
        true,
    ));

    let state = manager.state.lock().unwrap();
    let record = &state.records[&child.id];
    assert!(matches!(
        &record.status,
        DelegatedAgentStatus::LimitReached {
            output,
            turn_count: 2,
            turn_limit: 2,
        } if output == "partial answer"
    ));
    assert!(record.completed_at_ms.is_some());
    assert_eq!(state.root_mailbox.len(), 1);
    assert_eq!(
        state.root_mailbox[0].message,
        "/root/child reached its turn limit (2/2 turns); partial output:\npartial answer"
    );
    let listed = agent_record_value(record);
    assert_eq!(listed["phase"], "limit_reached");
    assert_eq!(listed["turn_count"], 2);
    assert_eq!(listed["turn_limit"], 2);
    assert_eq!(listed["status"]["state"], "limit_reached");
    assert_eq!(listed["status"]["output"], "partial answer");
    assert_eq!(listed["status"]["turn_count"], 2);
    assert_eq!(listed["status"]["turn_limit"], 2);
}

#[test]
fn limit_reached_without_output_explains_terminal_delivery() {
    let message = status_message(
        "/root/child",
        &DelegatedAgentStatus::LimitReached {
            output: String::new(),
            turn_count: 1,
            turn_limit: 1,
        },
    );
    assert_eq!(
        message,
        "/root/child reached its turn limit (1/1 turns); no final answer was produced"
    );
}

#[test]
fn task_names_and_descendant_paths_are_strict() {
    assert!(validate_task_name("review_2").is_ok());
    assert!(validate_task_name("Review").is_err());
    assert!(validate_task_name("../escape").is_err());
    assert!(is_descendant_path("/root/a/b", "/root/a"));
    assert!(!is_descendant_path("/root/ab", "/root/a"));
}

#[test]
fn config_requires_real_bounded_child_capacity() {
    let mut config = DelegationConfig::new("ignored");
    config.limits.max_concurrent_agents = 1;
    assert!(config.validate().is_err());
    config.limits.max_concurrent_agents = 4;
    config.limits.max_total_agents = 3;
    assert!(config.validate().is_err());
}

#[test]
fn extension_child_policy_installs_only_detached_read_only_tools_and_lowers_parent_limits() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = writable_manager_with_core_tools(directory.path());
    {
        let manager_mut = Arc::get_mut(&mut manager).expect("manager remains unique");
        manager_mut.template.max_turns = Some(2);
        manager_mut
            .template
            .runtime
            .get_mut()
            .unwrap()
            .max_session_cost_microdollars = Some(50);
    }
    let identity = AgentIdentity {
        id: "agent-policy".into(),
        path: "/root/policy".into(),
        depth: 1,
    };
    let mut policy = test_extension_policy();
    policy.max_turns = Some(8);
    policy.max_cost_microdollars = Some(200);
    let allowed = policy.tools.iter().cloned().collect::<BTreeSet<_>>();
    let (_, effective) = manager
        .template
        .extensions
        .scoped_tool_snapshot(&allowed)
        .unwrap();
    policy.tools = effective;
    policy.max_turns = Some(2);
    policy.max_cost_microdollars = Some(50);
    let session = Session::create(directory.path().join("policy-child.jsonl")).unwrap();
    let child = manager
        .build_child_agent(session, &identity, Some(&policy))
        .unwrap();
    assert_eq!(
        child.registered_tool_names(),
        vec!["read".to_owned(), "search".to_owned()]
    );
    assert!(child
        .registered_tool_names()
        .iter()
        .all(|name| !COLLABORATION_TOOL_NAMES.contains(&name.as_str())));
    assert!(manager
        .template
        .extensions
        .tool_definitions()
        .iter()
        .any(|tool| tool.name == "write"));
}

#[test]
fn extension_child_rejects_unpriced_model_before_session_creation() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = writable_manager_with_core_tools(directory.path());
    let manager_mut = Arc::get_mut(&mut manager).unwrap();
    Arc::make_mut(&mut manager_mut.template.model.spec).pricing = None;
    let binding = manager.root_binding();
    let service = binding
        .extension_service("extension-policy", "parent-session", "root-owner")
        .unwrap();

    let error = service
        .spawn(
            "root-owner",
            test_extension_spawn("unpriced", None, None, "must not run", "unpriced-key"),
        )
        .unwrap_err();
    assert!(error.contains("trusted model pricing"), "{error}");
    assert!(manager.state.lock().unwrap().records.is_empty());
}

#[tokio::test]
async fn delegated_children_inherit_each_cache_warming_mode() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager_with_core_tools(directory.path());
    let control = manager
        .template
        .runtime
        .read()
        .unwrap()
        .cache_warming_mode
        .clone();
    let mut children = Vec::new();
    for (index, mode) in [
        crate::CacheWarmMode::Off,
        crate::CacheWarmMode::Streaming,
        crate::CacheWarmMode::Idle,
    ]
    .into_iter()
    .enumerate()
    {
        control.send_modify(|policy| policy.set_mode(mode));
        let session =
            Session::create(directory.path().join(format!("warm-child-{index}.jsonl"))).unwrap();
        let child = manager
            .build_child_agent(
                session,
                &AgentIdentity {
                    id: format!("agent-warm-{index}"),
                    path: format!("/root/warm-{index}"),
                    depth: 1,
                },
                None,
            )
            .unwrap();
        assert_eq!(child.cache_warming_mode(), mode);
        children.push(child);
    }
    control.send_modify(|policy| policy.set_mode(crate::CacheWarmMode::Off));
    for child in children {
        assert_eq!(child.cache_warming_mode(), crate::CacheWarmMode::Off);
    }
}

#[tokio::test]
async fn extension_child_without_parent_or_requested_token_limit_is_unlimited() {
    let directory = tempfile::tempdir().unwrap();
    let team = directory.path().join("team-inherited-token-limit");
    std::fs::create_dir(&team).unwrap();
    let manager = writable_manager_with_core_tools(&team);
    assert_eq!(
        manager.template.runtime.read().unwrap().max_session_tokens,
        None
    );
    let mut policy = test_extension_policy();
    policy.max_tokens = None;
    let session = Session::create(team.join("inherited-child.jsonl")).unwrap();
    let identity = AgentIdentity {
        id: "agent-inherited".into(),
        path: "/root/inherited".into(),
        depth: 1,
    };
    let child = manager
        .build_child_agent(session, &identity, Some(&policy))
        .unwrap();
    assert_eq!(child.max_session_tokens(), None);
    assert_eq!(
        child.max_output_tokens(),
        manager.template.runtime.read().unwrap().max_output_tokens
    );

    let binding = manager.root_binding();
    let service = binding
        .extension_service("extension-policy", "parent-session", "root-owner")
        .unwrap();
    let mut request = test_extension_spawn(
        "inherited",
        Some("review"),
        None,
        "inherit parent token policy",
        "inherited-token-key",
    );
    request.policy.max_tokens = None;
    let result = service.spawn("root-owner", request).unwrap();
    assert!(result["policy"]["max_tokens"].is_null());
    binding.request_shutdown();
}

#[tokio::test]
async fn extension_child_limits_are_clamped_to_parent_session_limits() {
    let directory = tempfile::tempdir().unwrap();
    let team = directory.path().join("team-parent-limits");
    std::fs::create_dir(&team).unwrap();
    let manager = writable_manager_with_core_tools(&team);
    let binding = manager.root_binding();
    let mut runtime = manager.template.runtime.read().unwrap().clone();
    runtime.max_session_tokens = Some(48_000);
    runtime.max_session_cost_microdollars = Some(125_000);
    binding.update_runtime_settings(runtime);
    let service = binding
        .extension_service("extension-policy", "parent-session", "root-owner")
        .unwrap();
    let mut request = test_extension_spawn(
        "bounded",
        Some("review"),
        None,
        "respect parent limits",
        "parent-limits-key",
    );
    request.policy.max_turns = Some(12);
    request.policy.max_tokens = Some(64_000);
    request.policy.max_cost_microdollars = Some(500_000);

    let result = service.spawn("root-owner", request).unwrap();

    assert_eq!(result["policy"]["max_turns"], 4);
    assert_eq!(result["turn_limit"], 4);
    assert_eq!(result["policy"]["max_tokens"], 48_000);
    assert_eq!(result["policy"]["max_cost_microdollars"], 125_000);
    binding.request_shutdown();
}

#[tokio::test]
async fn extension_service_enforces_concurrency_depth_deadline_and_list_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let team = directory.path().join("team-extension-policy");
    std::fs::create_dir(&team).unwrap();
    let manager = writable_manager_with_core_tools(&team);
    let binding = manager.root_binding();
    let service = binding
        .extension_service("extension-policy", "parent-session", "root-owner")
        .unwrap();
    let mut first_request = test_extension_spawn(
        "first",
        Some("review"),
        Some(&"f".repeat(64)),
        "first bounded task",
        "first-key",
    );
    first_request.policy.max_tokens = Some(64_000);
    first_request.policy.max_cost_microdollars = Some(500_000);
    let first = service.spawn("root-owner", first_request).unwrap();
    assert_eq!(
        first["effective_tool_policy"]["effect_policy"]["value"],
        "controlled"
    );
    assert_eq!(
        first["orchestration_provenance"]["sandbox"],
        "parent_inherited"
    );
    assert_eq!(
        first["orchestration_provenance"]["effect_policy"],
        "parent_inherited"
    );
    assert_eq!(
        first["orchestration_provenance"]["approval_authority"],
        "parent_inherited"
    );
    assert_eq!(
        first["orchestration_provenance"]["environment"],
        "parent_inherited"
    );
    assert_eq!(
        first["orchestration_provenance"]["working_directory"],
        "parent_inherited"
    );
    assert_eq!(
        first["orchestration_provenance"]["extension_trust"],
        "parent_inherited"
    );
    assert_eq!(
        first["orchestration_provenance"]["tool_scope"],
        "child_override"
    );
    assert_eq!(
        first["orchestration_provenance"]["execution_limits"],
        "child_override"
    );
    let mut second_request =
        test_extension_spawn("second", None, None, "second bounded task", "second-key");
    second_request.policy.max_tokens = Some(64_000);
    second_request.policy.max_cost_microdollars = Some(500_000);
    let second = service.spawn("root-owner", second_request).unwrap();
    let error = service
        .spawn(
            "root-owner",
            test_extension_spawn("third", None, None, "third bounded task", "third-key"),
        )
        .unwrap_err();
    assert!(error.contains("concurrency limit"), "{error}");

    let first_id = first["agent_id"].as_str().unwrap();
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(first_id).unwrap();
        assert!(extension_delegated_session_matches_owner(
            "extension-policy",
            "root-owner",
            &record.session_path,
        ));
        assert!(!extension_delegated_session_matches_owner(
            "another-extension",
            "root-owner",
            &record.session_path,
        ));
        assert!(!extension_delegated_session_matches_owner(
            "extension-policy",
            "another-owner",
            &record.session_path,
        ));
        record.turn_count = 2;
        record.tool_call_count = 1;
        record.active_tools.insert("call-3".into(), "search".into());
        record.usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            ..Usage::default()
        };
        record.cost_microdollars = Some(7);
    }
    // Exercise the real capture path outside the state lock: one finished
    // call with flattened arguments and one still in flight.
    manager.update_agent_tool_started(
        first_id,
        "call-9",
        "read".to_owned(),
        tool_args_summary(&json!({
            "path": "crates/octet-agent/src/delegation.rs",
            "limit": 120,
            "options": {"nested": true},
            "note": "line one\nline two"
        })),
    );
    manager.update_agent_tool_finished(first_id, "call-9", false);
    manager.update_agent_tool_started(
        first_id,
        "call-10",
        "search".to_owned(),
        tool_args_summary(&json!({"pattern": "spawn_agent"})),
    );
    let listed = service.list("root-owner").unwrap();
    let record = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["agent_id"] == first_id)
        .unwrap();
    assert_eq!(record["task_name"], "first");
    assert_eq!(record["policy"]["tools"], json!(["read", "search"]));
    assert_eq!(
        record["effective_tool_policy"]["effect_policy"]["value"],
        "controlled"
    );
    assert_eq!(
        record["orchestration_provenance"]["approval_authority"],
        "parent_inherited"
    );
    assert_eq!(
        record["orchestration_provenance"]["tool_scope"],
        "child_override"
    );
    assert_eq!(
        record["orchestration_provenance"]["execution_limits"],
        "child_override"
    );
    assert_eq!(record["turn_count"], 2);
    assert_eq!(record["tool_call_count"], 3);
    assert_eq!(record["phase"], "using_tool");
    assert_eq!(record["tool_name"], "search");
    let recent = record["recent_tools"].as_array().unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0]["name"], "read");
    assert_eq!(
            recent[0]["args"],
            "limit=120 note=line one line two options={\"nested\":true} path=crates/octet-agent/src/delegation.rs"
        );
    assert!(recent[0]["started_at_ms"].as_u64().is_some());
    assert!(recent[0]["finished_at_ms"].as_u64().is_some());
    assert_eq!(recent[0]["error"], false);
    assert_eq!(recent[1]["name"], "search");
    assert_eq!(recent[1]["args"], "pattern=spawn_agent");
    assert!(recent[1]["finished_at_ms"].is_null());
    assert_eq!(record["usage"]["total_tokens"], 15);
    assert_eq!(record["cost_microdollars"], 7);
    assert_eq!(record["profile"], "review");
    assert_eq!(record["idempotency_key"], "first-key");
    assert_eq!(record["fingerprint"], "f".repeat(64));
    assert!(record["created_at_ms"].as_u64().is_some());
    assert!(record["deadline_at_ms"].as_u64().is_some());
    assert_eq!(record["provenance"]["principal"], "extension-policy");
    assert_eq!(record["provenance"]["resource_owner"], "root-owner");
    let reference = record["session"].as_str().unwrap();
    let mut inspection = binding
        .open_session_reference("extension-policy", reference)
        .unwrap()
        .unwrap();
    assert!(inspection
        .append(crate::EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .is_err());
    assert!(binding
        .open_session_reference("another-extension", reference)
        .unwrap()
        .is_none());
    assert!(binding
        .open_session_reference(
            "extension-policy",
            &format!("agent-session:{}", "0".repeat(64)),
        )
        .unwrap()
        .is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let session_path = manager.state.lock().unwrap().records[first_id]
            .session_path
            .clone();
        let saved = session_path.with_extension("saved");
        std::fs::rename(&session_path, &saved).unwrap();
        symlink(&saved, &session_path).unwrap();
        assert!(
            binding
                .open_session_reference("extension-policy", reference)
                .is_err(),
            "an authorized opaque reference must not follow a replaced ledger symlink"
        );
        std::fs::remove_file(&session_path).unwrap();
        std::fs::rename(&saved, &session_path).unwrap();
        std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            binding
                .open_session_reference("extension-policy", reference)
                .is_err(),
            "an authorized reference must not disclose a non-private child ledger"
        );
        std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(binding
            .open_session_reference("extension-policy", reference)
            .unwrap()
            .is_some());
    }

    let journal = std::fs::read_to_string(manager.team_directory.join("provenance.jsonl")).unwrap();
    let persisted = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["event"] == "agent_spawned" && event["agent_id"] == first_id)
        .unwrap();
    assert_eq!(persisted["extension_parent_session_id"], "parent-session");
    assert_eq!(persisted["extension_principal"], "extension-policy");
    assert_eq!(persisted["extension_resource_owner"], "root-owner");
    assert_eq!(persisted["extension_profile"], "review");
    assert_eq!(persisted["extension_idempotency_key"], "first-key");
    assert_eq!(persisted["extension_fingerprint"], "f".repeat(64));
    assert_eq!(
        persisted["effective_tool_policy"]["effect_policy"]["value"],
        "controlled"
    );
    assert_eq!(
        persisted["orchestration_provenance"]["sandbox"],
        "parent_inherited"
    );
    assert_eq!(
        persisted["orchestration_provenance"]["extension_trust"],
        "parent_inherited"
    );
    assert_eq!(
        persisted["orchestration_provenance"]["tool_scope"],
        "child_override"
    );
    assert_eq!(
        persisted["orchestration_provenance"]["execution_limits"],
        "child_override"
    );
    assert!(persisted["session_reference"]
        .as_str()
        .is_some_and(|reference| reference.starts_with("agent-session:")));
    assert!(persisted.get("task").is_none());
    assert!(persisted.get("session").is_none());
    assert!(!journal.contains("first bounded task"));

    let nested_owner = AgentIdentity {
        id: first_id.into(),
        path: first["agent_path"].as_str().unwrap().into(),
        depth: 1,
    };
    let nested_error = manager
        .spawn(
            &nested_owner,
            SpawnRequest {
                task_name: "nested".into(),
                display_task_name: None,
                message: "must not create a session".into(),
                extension_policy: Some(test_extension_policy()),
                extension_provenance: Some(ExtensionSpawnProvenance {
                    parent_session_id: "parent-session".into(),
                    principal: "extension-policy".into(),
                    resource_owner: "child-owner".into(),
                    profile: None,
                    idempotency_key: "nested-key".into(),
                    fingerprint: None,
                }),
            },
        )
        .unwrap_err();
    assert!(nested_error.contains("depth limit"), "{nested_error}");
    assert_eq!(second["policy"]["max_concurrent_children"], 2);
    assert_eq!(first["policy"]["max_tokens"], 64_000);
    assert_eq!(second["policy"]["max_tokens"], 64_000);
    assert_eq!(first["policy"]["max_cost_microdollars"], 500_000);
    assert_eq!(second["policy"]["max_cost_microdollars"], 500_000);

    binding.request_shutdown();
    assert!(
        service.list("root-owner").is_ok(),
        "owner-scoped observation must remain available after root settlement"
    );
}

#[tokio::test]
async fn delegation_span_owns_the_child_run_and_nests_child_spans() {
    use crate::telemetry::spans::{InMemoryTelemetryContext, SpanStatus};

    let directory = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(
                        b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_test\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"ok\"}\n\ndata: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n",
                    ),
            )
            .mount(&server)
            .await;

    let mut manager = writable_manager(directory.path());
    {
        let manager_mut = Arc::get_mut(&mut manager).expect("new manager is uniquely owned");
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-6-astra".into()))
            .unwrap();
        let spec = Arc::make_mut(&mut model.spec);
        spec.id = octet_ai::ModelId("codex/gpt-6-astra".into());
        spec.capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
        spec.capabilities
            .reasoning
            .as_mut()
            .expect("Astra reasoning capability")
            .max_effort = octet_ai::ReasoningEffort::Ultra;
        let endpoint = Arc::make_mut(&mut model.endpoint);
        endpoint.base_url = url::Url::parse(&format!("{}/", server.uri())).unwrap();
        endpoint.auth = octet_ai::Auth::None;
        endpoint.transport = octet_ai::EndpointTransport::Http;
        manager_mut
            .template
            .runtime
            .get_mut()
            .unwrap()
            .max_output_tokens = model.spec.limits.max_output_tokens;
        // An Astra request without an explicit effort is a validated
        // `Reasoning` rejection, so this span boundary runs at the host's
        // Ultra tier like the wire-contract sibling test.
        *manager_mut.template.reasoning.get_mut().unwrap() =
            octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra);
        manager_mut.template.model = model;
    }

    let fixture = InMemoryTelemetryContext::default();
    manager.set_span_context(fixture.context());
    let (identity, mut commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let session = Session::create(directory.path().join("span-child.jsonl")).unwrap();
    let mut child = manager.build_child_agent(session, &identity, None).unwrap();
    let shutdown = crate::CancellationToken::default();
    let execution = manager
        .execute_child_run(
            &mut child,
            "report ok".into(),
            ChildRunContext {
                queued_delivery_ids: BTreeSet::new(),
                identity: &identity,
                commands: &mut commands,
                shutdown: &shutdown,
                extension_policy: None,
                deadline: None,
            },
        )
        .await;
    assert!(
        matches!(execution.outcome, WorkerOutcome::Completed(_)),
        "the scripted child run must complete: {:?}",
        execution.outcome
    );

    let spans = fixture.get_spans();
    let names: Vec<&str> = spans.iter().map(|span| span.name.as_str()).collect();
    assert_eq!(
        names[0], "octet.agent.delegation",
        "the child run is observed through one delegation boundary: {names:?}"
    );
    assert_eq!(spans[0].parent_id, None);
    let run = spans
        .iter()
        .position(|span| span.name == "octet.agent.run")
        .expect("the driven child run is spanned");
    assert_eq!(
        spans[run].parent_id,
        Some(spans[0].id),
        "the child's own run nests under the delegation boundary: {names:?}"
    );
    let turn = spans
        .iter()
        .position(|span| span.name == "octet.agent.turn")
        .expect("the child turn is spanned");
    assert_eq!(spans[turn].parent_id, Some(spans[run].id));
    let request = spans
        .iter()
        .position(|span| span.name == "octet.ai.request")
        .expect("the child provider request is spanned");
    assert_eq!(spans[request].parent_id, Some(spans[turn].id));
    assert!(
        spans
            .iter()
            .all(|span| span.settled && span.status == SpanStatus::Ok),
        "every delegated boundary settles with the child run: {spans:#?}"
    );
    assert_eq!(fixture.dropped_spans(), 0);
}

#[tokio::test]
async fn elapsed_extension_deadline_settles_before_provider_or_tool_execution() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (identity, mut commands) = insert_test_record(&manager, DelegatedAgentStatus::Pending);
    let session = Session::create(directory.path().join("deadline-child.jsonl")).unwrap();
    let mut agent = manager.build_child_agent(session, &identity, None).unwrap();
    let shutdown = crate::CancellationToken::default();
    let policy = test_extension_policy();
    let outcome = manager
        .execute_child_run(
            &mut agent,
            "must not execute".into(),
            ChildRunContext {
                queued_delivery_ids: BTreeSet::new(),
                identity: &identity,
                commands: &mut commands,
                shutdown: &shutdown,
                extension_policy: Some(&policy),
                deadline: Some(tokio::time::Instant::now()),
            },
        )
        .await;
    assert!(matches!(outcome.outcome, WorkerOutcome::TimedOut));
    assert!(agent.session().entries().is_empty());
}

#[tokio::test]
async fn follow_up_reanchors_an_elapsed_deadline_for_a_settled_worker() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let root = manager.root_binding().identity;
    let (identity, _commands) = insert_test_record(
        &manager,
        DelegatedAgentStatus::Completed {
            output: "first run settled".into(),
        },
    );
    {
        let mut state = manager.state.lock().unwrap();
        let record = state
            .records
            .get_mut(&identity.id)
            .expect("test record exists");
        record.extension_policy = Some(test_extension_policy());
        record.deadline_at_ms = Some(1);
        record.completed_at_ms = Some(2);
    }

    let resumed = manager
        .follow_up(
            &root,
            FollowUpRequest {
                target: identity.id.clone(),
                message: "second run".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(resumed["delivery"], "new_run");

    let state = manager.state.lock().unwrap();
    let record = &state.records[&identity.id];
    assert_eq!(record.status, DelegatedAgentStatus::Pending);
    assert!(record.completed_at_ms.is_none());
    let deadline = record
        .deadline_at_ms
        .expect("elapsed deadline was re-anchored");
    let now = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
    assert!(deadline > now + 290_000);
    assert!(deadline <= now + 310_000);
}

#[tokio::test]
async fn follow_up_preserves_a_future_deadline_for_a_settled_worker() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let root = manager.root_binding().identity;
    let (identity, _commands) = insert_test_record(
        &manager,
        DelegatedAgentStatus::Completed {
            output: "first run settled".into(),
        },
    );
    let preserved = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX) + 1_000_000;
    {
        let mut state = manager.state.lock().unwrap();
        let record = state
            .records
            .get_mut(&identity.id)
            .expect("test record exists");
        record.extension_policy = Some(test_extension_policy());
        record.deadline_at_ms = Some(preserved);
        record.completed_at_ms = Some(u64::try_from(timestamp_ms()).unwrap_or(u64::MAX));
    }

    manager
        .follow_up(
            &root,
            FollowUpRequest {
                target: identity.id.clone(),
                message: "second run".into(),
            },
        )
        .await
        .unwrap();

    let state = manager.state.lock().unwrap();
    assert_eq!(state.records[&identity.id].deadline_at_ms, Some(preserved));
}

#[test]
fn extension_output_bound_is_exact_and_utf8_safe() {
    let output = bounded_text_to(&"é".repeat(10_000), 513);
    assert!(output.len() <= 513);
    assert!(output.is_char_boundary(output.len()));
    assert!(output.ends_with("...[truncated]"));
}

#[test]
fn tool_args_summary_is_flat_bounded_and_single_line() {
    let summary = tool_args_summary(&json!({
        "path": "src/main.rs",
        "line": 42,
        "all": true,
        "missing": null,
        "options": {"deep": [1, 2]},
    }));
    // serde_json maps sort keys, so the summary is deterministic.
    assert_eq!(
        summary,
        "all=true line=42 options={\"deep\":[1,2]} path=src/main.rs"
    );
    let collapsed = tool_args_summary(&json!({"command": "make\ntest\n  here"}));
    assert_eq!(collapsed, "command=make test here");
    let oversized = tool_args_summary(&json!({ "blob": "x".repeat(4_000) }));
    assert!(oversized.len() <= MAX_TOOL_ARGS_SUMMARY_BYTES + "\n...[truncated]".len());
    assert_eq!(tool_args_summary(&serde_json::Value::Null), "");
    assert_eq!(
        tool_args_summary(&json!([1, 2, 3])),
        "",
        "non-object arguments summarize to nothing"
    );
}

#[tokio::test]
async fn extension_services_are_idempotent_and_isolated_by_principal_and_owner() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let binding = manager.root_binding();
    let service_a = binding
        .extension_service("extension-a", "parent-session", "root-owner")
        .unwrap();
    let service_b = binding
        .extension_service("extension-b", "parent-session", "root-owner")
        .unwrap();
    let (identity, mut commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    {
        let mut state = service_a.state.lock().unwrap();
        let owner = state.owners.entry("root-owner".into()).or_default();
        owner.owned_agents.insert(identity.id.clone());
        owner.idempotent_spawns.insert(
            "spawn-1".into(),
            IdempotentExtensionSpawn {
                task_name: "research".into(),
                profile: None,
                fingerprint: None,
                message_sha256: format!("{:x}", Sha256::digest(b"find it")),
                policy: test_extension_policy(),
                result: json!({"agent_id":identity.id.clone(),"status":"pending"}),
            },
        );
    }

    let cached = service_a
        .spawn(
            "root-owner",
            test_extension_spawn("research", None, None, "find it", "spawn-1"),
        )
        .unwrap();
    assert_eq!(cached["agent_id"], "agent-1");
    assert!(service_a
        .spawn(
            "root-owner",
            test_extension_spawn("research", None, None, "different", "spawn-1"),
        )
        .unwrap_err()
        .contains("different input"));

    assert!(service_b
        .send_message("root-owner", &identity.id, "cross-principal".into())
        .await
        .unwrap_err()
        .contains("no child sessions"));
    assert!(service_a
        .list("different-owner")
        .unwrap_err()
        .contains("not an active"));

    service_a
        .send_message("root-owner", &identity.id, "owned".into())
        .await
        .unwrap();
    let command = commands.recv().await.unwrap();
    assert!(matches!(command.kind, WorkerCommandKind::Message(_)));
    service_a.shutdown_owned();
    assert!(manager.state.lock().unwrap().records[&identity.id]
        .shutdown
        .is_cancelled());
}

#[tokio::test]
async fn extension_spawn_idempotency_survives_the_owning_run_without_a_duplicate_worker() {
    let directory = tempfile::tempdir().unwrap();
    let team = directory.path().join("team-idempotency-runs");
    std::fs::create_dir(&team).unwrap();
    let manager = writable_manager_with_core_tools(&team);
    let binding = manager.root_binding();
    let service = binding
        .extension_service("extension-a", "parent-session", "root-owner")
        .unwrap();
    let first = service
        .spawn(
            "root-owner",
            test_extension_spawn("research", None, None, "find it", "spawn-1"),
        )
        .unwrap();

    manager.prepare_owning_run(&root_identity()).unwrap();
    // Force the durable path: a new service process has no local cache.
    service.state.lock().unwrap().owners.clear();
    assert!(service
        .spawn(
            "root-owner",
            test_extension_spawn("research", None, None, "different task", "spawn-1")
        )
        .unwrap_err()
        .contains("different input"));
    // The session owns the worker, so the same idempotency key re-issues
    // the original result instead of spawning a duplicate worker.
    let second = service
        .spawn(
            "root-owner",
            test_extension_spawn("research", None, None, "find it", "spawn-1"),
        )
        .unwrap();
    assert_eq!(first["agent_id"], second["agent_id"]);
    assert_eq!(first["agent_path"], second["agent_path"]);
    assert_eq!(
        service.list("root-owner").unwrap()["agents"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // A different key still spawns its own worker.
    let other = service
        .spawn(
            "root-owner",
            test_extension_spawn("research", None, None, "find it", "spawn-2"),
        )
        .unwrap();
    assert_ne!(other["agent_id"], first["agent_id"]);
    assert_eq!(
        service.list("root-owner").unwrap()["agents"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn automatic_mailbox_eviction_is_oldest_first_and_stays_bounded() {
    let mut mailbox = VecDeque::new();
    for index in 0..(MAX_MAILBOX_MESSAGES + 5) {
        push_mailbox_bounded(
            &mut mailbox,
            MailboxMessage {
                kind: "task_status",
                from: ROOT_AGENT_ID.into(),
                task_name: None,
                message: index.to_string(),
                evictable: true,
                continued: false,
                leased: false,
            },
        );
    }

    assert_eq!(mailbox.len(), MAX_MAILBOX_MESSAGES);
    assert_eq!(mailbox.front().unwrap().message, "5");
    assert!(mailbox.iter().map(mailbox_message_bytes).sum::<usize>() <= MAX_MAILBOX_BYTES);
}

#[test]
fn automatic_mailbox_notifications_never_evict_direct_messages() {
    let mut mailbox = VecDeque::new();
    push_mailbox_bounded(
        &mut mailbox,
        MailboxMessage {
            kind: "message",
            from: "agent-a".into(),
            task_name: None,
            message: "durable".into(),
            evictable: false,
            continued: false,
            leased: false,
        },
    );
    for index in 0..MAX_MAILBOX_MESSAGES {
        push_mailbox_bounded(
            &mut mailbox,
            MailboxMessage {
                kind: "task_status",
                from: "agent-b".into(),
                task_name: None,
                message: index.to_string(),
                evictable: true,
                continued: false,
                leased: false,
            },
        );
    }

    assert_eq!(mailbox.len(), MAX_MAILBOX_MESSAGES);
    assert!(mailbox
        .iter()
        .any(|message| message.message == "durable" && !message.evictable));
    assert_eq!(
        mailbox.back().unwrap().message,
        (MAX_MAILBOX_MESSAGES - 1).to_string()
    );
}

#[test]
fn direct_mailbox_messages_displace_only_automatic_notifications() {
    let mut mailbox = VecDeque::new();
    for index in 0..MAX_MAILBOX_MESSAGES {
        push_mailbox_bounded(
            &mut mailbox,
            MailboxMessage {
                kind: "task_status",
                from: "agent-b".into(),
                task_name: None,
                message: index.to_string(),
                evictable: true,
                continued: false,
                leased: false,
            },
        );
    }
    let direct = MailboxMessage {
        kind: "message",
        from: "agent-a".into(),
        task_name: None,
        message: "durable".into(),
        evictable: false,
        continued: false,
        leased: false,
    };

    assert!(mailbox_can_accept_after_evicting_automatic(
        &mailbox, &direct
    ));
    push_mailbox_bounded(&mut mailbox, direct);

    assert_eq!(mailbox.len(), MAX_MAILBOX_MESSAGES);
    assert_eq!(mailbox.front().unwrap().message, "1");
    assert_eq!(mailbox.back().unwrap().message, "durable");
    assert!(!mailbox.back().unwrap().evictable);
}

#[test]
fn mailbox_pages_commit_only_after_acknowledgement_and_preserve_utf8() {
    const OUTPUT_LIMIT: usize = 512;
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let owner = root_identity();
    let original = "αβγ delegated evidence ".repeat(180);
    {
        let mut state = manager.state.lock().unwrap();
        state.root_mailbox.push_back(MailboxMessage {
            kind: "message",
            from: "agent-a".into(),
            task_name: None,
            message: original.clone(),
            evictable: false,
            continued: false,
            leased: false,
        });
    }

    let first = manager
        .take_wait_result(&owner, OUTPUT_LIMIT)
        .unwrap()
        .unwrap();
    let first_delivery = first.delivery_id.unwrap();
    assert!(serde_json::to_string(&first.value).unwrap().len() <= OUTPUT_LIMIT);
    {
        let state = manager.state.lock().unwrap();
        assert_eq!(state.root_mailbox.len(), 1);
        assert!(state.root_mailbox.front().unwrap().leased);
    }
    manager.resolve_mailbox_delivery(ROOT_AGENT_ID, first_delivery, false);
    {
        let state = manager.state.lock().unwrap();
        assert_eq!(state.root_mailbox.front().unwrap().message, original);
        assert!(!state.root_mailbox.front().unwrap().leased);
    }

    let mut reconstructed = String::new();
    let mut page_index = 0usize;
    loop {
        let page = manager
            .take_wait_result(&owner, OUTPUT_LIMIT)
            .unwrap()
            .unwrap();
        let Some(delivery_id) = page.delivery_id else {
            break;
        };
        let encoded = serde_json::to_string(&page.value).unwrap();
        assert!(encoded.len() <= OUTPUT_LIMIT, "{}", encoded.len());
        let messages = page.value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let chunk = messages[0]["message"].as_str().unwrap();
        assert!(!chunk.is_empty());
        if page_index == 0 {
            assert_ne!(messages[0]["continued"], true);
        } else {
            assert_eq!(messages[0]["continued"], true);
        }
        reconstructed.push_str(chunk);
        manager.resolve_mailbox_delivery(ROOT_AGENT_ID, delivery_id, true);
        page_index += 1;
        if !page.value["more"].as_bool().unwrap() {
            break;
        }
    }

    assert!(page_index > 1);
    assert_eq!(reconstructed, original);
    assert!(manager.state.lock().unwrap().root_mailbox.is_empty());
}

#[test]
fn mailbox_delivery_ids_are_bound_to_the_owning_agent() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _command_rx) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    {
        let mut state = manager.state.lock().unwrap();
        state.root_mailbox.push_back(MailboxMessage {
            kind: "message",
            from: "root-peer".into(),
            task_name: None,
            message: "root-only".into(),
            evictable: false,
            continued: false,
            leased: false,
        });
        state
            .records
            .get_mut(&child.id)
            .unwrap()
            .mailbox
            .push_back(MailboxMessage {
                kind: "message",
                from: ROOT_AGENT_ID.into(),
                task_name: None,
                message: "child-only".into(),
                evictable: false,
                continued: false,
                leased: false,
            });
    }

    let page = manager.take_wait_result(&child, 512).unwrap().unwrap();
    let delivery_id = page.delivery_id.unwrap();
    assert_eq!(page.value["messages"][0]["message"], "child-only");

    manager.resolve_mailbox_delivery(ROOT_AGENT_ID, delivery_id, true);
    {
        let state = manager.state.lock().unwrap();
        assert_eq!(state.root_mailbox.front().unwrap().message, "root-only");
        assert!(state.records[&child.id].mailbox.front().unwrap().leased);
    }

    manager.resolve_mailbox_delivery(&child.id, delivery_id, true);
    let state = manager.state.lock().unwrap();
    assert_eq!(state.root_mailbox.front().unwrap().message, "root-only");
    assert!(state.records[&child.id].mailbox.is_empty());
}

#[tokio::test]
async fn oversized_durable_tasks_messages_and_followups_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let owner = root_identity();
    let oversized = "é".repeat(MAX_PROVENANCE_TEXT_BYTES / 2 + 1);

    let error = manager
        .spawn(
            &owner,
            SpawnRequest {
                task_name: "oversized".into(),
                display_task_name: None,
                message: oversized.clone(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();
    assert!(error.contains("spawn task exceeds"), "{error}");
    assert!(manager.state.lock().unwrap().records.is_empty());

    let error = manager
        .spawn(
            &owner,
            SpawnRequest {
                task_name: oversized.clone(),
                display_task_name: None,
                message: "work".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();
    assert!(error.contains("task_name must contain"), "{error}");
    assert!(manager.state.lock().unwrap().records.is_empty());

    let (_identity, command_rx) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let error = manager
        .send_message(&owner, "/root/child", oversized.clone())
        .await
        .unwrap_err();
    assert!(error.contains("message exceeds"), "{error}");
    let error = manager
        .follow_up(
            &owner,
            FollowUpRequest {
                target: "/root/child".into(),
                message: oversized,
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("follow-up exceeds"), "{error}");
    assert_eq!(command_rx.len(), 0);
    let state = manager.state.lock().unwrap();
    assert_eq!(
        state.records["agent-1"].reserved_messages,
        QueueUsage::default()
    );
    assert_eq!(
        state.records["agent-1"].queued_follow_ups,
        QueueUsage::default()
    );
}

#[test]
fn undelivered_prompt_messages_are_restored_in_fifo_order() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _command_rx) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    {
        let mut state = manager.state.lock().unwrap();
        let pending = &mut state.records.get_mut(&child.id).unwrap().pending_messages;
        pending.push_back(DirectedMessage {
            delivery_id: "test-delivery-a".into(),
            from: "older-a".into(),
            message: "first".into(),
        });
        pending.push_back(DirectedMessage {
            delivery_id: "test-delivery-b".into(),
            from: "older-b".into(),
            message: "second".into(),
        });
    }
    let leased = manager.take_pending_messages(&child.id);
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&child.id).unwrap();
        assert_eq!(record.reserved_messages.messages, 2);
        record.pending_messages.push_back(DirectedMessage {
            delivery_id: "test-delivery-c".into(),
            from: "newer".into(),
            message: "third".into(),
        });
    }

    manager.restore_pending_messages(&child.id, leased);

    let state = manager.state.lock().unwrap();
    let record = &state.records[&child.id];
    let messages = record
        .pending_messages
        .iter()
        .map(|message| message.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(messages, vec!["first", "second", "third"]);
    assert_eq!(record.reserved_messages, QueueUsage::default());
}

#[test]
fn undelivered_initial_task_returns_to_the_fifo_head_once() {
    let mut queued_tasks = VecDeque::from([
        QueuedTask::FollowUp(QueuedFollowUp {
            delivery_id: "test-follow-up".into(),
            from: ROOT_AGENT_ID.into(),
            message: "older follow-up".into(),
            attempts: 0,
        }),
        QueuedTask::FollowUp(QueuedFollowUp {
            delivery_id: "test-follow-up".into(),
            from: ROOT_AGENT_ID.into(),
            message: "newer follow-up".into(),
            attempts: 0,
        }),
    ]);

    assert!(matches!(
        restore_undelivered_task(
            &mut queued_tasks,
            QueuedTask::initial("initial task".into()),
            false,
            &WorkerOutcome::Failed("session append failed".into()),
        ),
        TaskRestore::Restored { .. }
    ));
    let labels = queued_tasks
        .iter()
        .map(|task| match task {
            QueuedTask::Initial(task) => task.task.as_str(),
            QueuedTask::FollowUp(follow_up) => follow_up.message.as_str(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec!["initial task", "older follow-up", "newer follow-up"]
    );

    let delivered = queued_tasks.pop_front().unwrap();
    assert!(matches!(
        restore_undelivered_task(
            &mut queued_tasks,
            delivered,
            true,
            &WorkerOutcome::Completed(String::new()),
        ),
        TaskRestore::NotRestored
    ));
    let labels = queued_tasks
        .iter()
        .map(|task| match task {
            QueuedTask::Initial(task) => task.task.as_str(),
            QueuedTask::FollowUp(follow_up) => follow_up.message.as_str(),
        })
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["older follow-up", "newer follow-up"]);
}

async fn stop_fixture_worker(manager: Arc<DelegationManager>) {
    manager.request_shutdown_descendants(ROOT_AGENT_ID);
    tokio::time::timeout(Duration::from_secs(3), async {
        while Arc::strong_count(&manager) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker and supervisor must release the old manager");
}

async fn assert_restart_follow_up_order(first: &str, second: &str) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let session_path = root.join("child.jsonl");
    Session::create(&session_path).unwrap();
    let first_id = {
        let manager = writable_manager(root);
        // Simulate acceptance immediately before process loss: the attached
        // channel is never polled, but acceptance must persist the payload.
        let (child, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Pending);
        manager
            .follow_up(
                &root_identity(),
                FollowUpRequest {
                    target: child.id.clone(),
                    message: first.into(),
                },
            )
            .await
            .unwrap();
        let state = manager.state.lock().unwrap();
        state.records[&child.id].pending_follow_ups[0]
            .delivery_id
            .clone()
    };
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
            .expect(2).mount(&server).await;
    let mut manager = writable_manager(root);
    let template = &mut Arc::get_mut(&mut manager).unwrap().template;
    Arc::make_mut(&mut template.model.endpoint).base_url =
        url::Url::parse(&format!("{}/", server.uri())).unwrap();
    Arc::make_mut(&mut template.model.endpoint).auth = octet_ai::Auth::None;
    manager.restore_durable_fleet();
    // Exercise explicit resume, not automatic prepare_owning_run reattach.
    manager
        .follow_up(
            &root_identity(),
            FollowUpRequest {
                target: "agent-1".into(),
                message: second.into(),
            },
        )
        .await
        .unwrap();
    let second_id = manager.state.lock().unwrap().records["agent-1"].pending_follow_ups[1]
        .delivery_id
        .clone();
    assert_ne!(first_id, second_id, "equal text is still distinct work");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let done = {
                let state = manager.state.lock().unwrap();
                let record = &state.records["agent-1"];
                assert!(
                    !matches!(record.status, DelegatedAgentStatus::Failed { .. }),
                    "{:?}",
                    record.status
                );
                matches!(record.status, DelegatedAgentStatus::Completed { .. })
                    && record.pending_follow_ups.is_empty()
                    && record.queued_follow_ups.messages == 0
            };
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stop_fixture_worker(manager).await;
    let transcript = Session::open_read_only(&session_path).unwrap();
    let delivered = transcript
        .entries()
        .iter()
        .filter_map(|entry| match &entry.value {
            crate::EntryValue::Message(octet_ai::Message::User(message)) => {
                let text = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        octet_ai::UserPart::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(text)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        delivered.len(),
        2,
        "each accepted envelope gets one user turn"
    );
    assert!(delivered[0].contains(first));
    assert!(delivered[1].contains(second));
    assert_eq!(
        delivery_ids_in_envelopes(&delivered[0]),
        BTreeSet::from([first_id])
    );
    assert_eq!(
        delivery_ids_in_envelopes(&delivered[1]),
        BTreeSet::from([second_id])
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    let restored = writable_manager(root);
    restored.restore_durable_fleet();
    assert!(restored.restored_tasks("agent-1").is_empty());
}

#[tokio::test]
async fn explicit_resume_preserves_older_durable_follow_up_order() {
    assert_restart_follow_up_order("older A", "new B").await;
}

#[tokio::test]
async fn explicit_resume_preserves_identical_text_with_distinct_delivery_ids() {
    assert_restart_follow_up_order("identical text", "identical text").await;
}

#[cfg(unix)]
#[tokio::test]
async fn initial_startup_retry_count_and_dead_letter_survive_real_fleet_restarts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = root.join("missing-workspace");
    let mut manager = writable_manager_with_workspace(root, &workspace);
    manager
        .spawn(
            &root_identity(),
            SpawnRequest {
                task_name: "initial".into(),
                display_task_name: None,
                message: "durable initial payload".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap();
    // No await has yielded to the spawned task: acknowledgement itself is
    // evidence that the initial payload/identity and zero attempts are safe.
    let fleet: DurableFleet =
        serde_json::from_slice(&std::fs::read(fleet_roster_path(root, Path::new(""))).unwrap())
            .unwrap();
    let initial = fleet.records[0].pending_initial_task.as_ref().unwrap();
    assert_eq!(initial.task, "durable initial payload");
    assert_eq!(initial.attempts, 0);
    let delivery_id = initial.delivery_id.clone();
    assert_eq!(delivery_id.len(), 32);
    for attempts in 1..=MAX_UNDELIVERED_TASK_ATTEMPTS {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(
                    manager.state.lock().unwrap().records["agent-1"].status,
                    DelegatedAgentStatus::Failed { .. }
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        {
            let state = manager.state.lock().unwrap();
            let record = &state.records["agent-1"];
            if attempts < MAX_UNDELIVERED_TASK_ATTEMPTS {
                let pending = record.pending_initial_task.as_ref().unwrap();
                assert_eq!(pending.attempts, attempts);
                assert_eq!(pending.delivery_id, delivery_id);
            } else {
                assert!(record.pending_initial_task.is_none());
                assert!(record
                    .durable_diagnostic
                    .as_deref()
                    .unwrap()
                    .contains("dead-lettered after 3"));
            }
        }
        stop_fixture_worker(manager).await;
        manager = writable_manager_with_workspace(root, &workspace);
        manager.restore_durable_fleet();
        if attempts < MAX_UNDELIVERED_TASK_ATTEMPTS {
            assert_eq!(
                manager.state.lock().unwrap().records["agent-1"]
                    .pending_initial_task
                    .as_ref()
                    .unwrap()
                    .attempts,
                attempts
            );
            manager.prepare_owning_run(&root_identity()).unwrap();
            // A settled failure is never autonomously replayed on a new
            // owner. Explicit continuation retries the retained initial
            // input ahead of the new follow-up in the same child session.
            manager
                .follow_up(
                    &root_identity(),
                    FollowUpRequest {
                        target: "agent-1".into(),
                        message: format!("explicit retry {attempts}"),
                    },
                )
                .await
                .unwrap();
        }
    }
    assert!(manager
        .restored_tasks("agent-1")
        .iter()
        .all(|task| !matches!(task, QueuedTask::Initial(_))));
}

#[test]
fn restart_recovers_usage_and_uncertainty_from_child_ledger_not_stale_roster() {
    for authority in ["known", "uncertain", "missing"] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let session_path = root.join("child.jsonl");
        let mut child = Session::create(&session_path).unwrap();
        {
            let manager = writable_manager(root);
            let (identity, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Idle);
            let mut state = manager.state.lock().unwrap();
            let record = state.records.get_mut(&identity.id).unwrap();
            record.usage = Usage::default();
            record.cost = Some(Cost::default());
            record.cost_microdollars = Some(0);
            record.usage_uncertain = false;
            manager.persist_durable_fleet_locked(&mut state);
        }
        // This synced child commit deliberately occurs after the roster write.
        let model = test_template(root).model;
        let usage = Usage {
            input_tokens: 29,
            output_tokens: 4,
            total_tokens: 33,
            ..Default::default()
        };
        let cost = Cost {
            input: 2,
            output: 3,
            total: 5,
            total_picodollars_remainder: 44,
            ..Default::default()
        };
        child
            .record_compaction_usage(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                usage,
                Some(cost),
            )
            .unwrap();
        if authority == "uncertain" {
            child
                .record_usage_uncertainty(
                    model.endpoint.id.clone(),
                    model.spec.id.clone(),
                    "late_interruption",
                )
                .unwrap();
        }
        drop(child);
        if authority == "missing" {
            std::fs::remove_file(&session_path).unwrap();
        }
        let manager = writable_manager(root);
        manager.restore_durable_fleet();
        let state = manager.state.lock().unwrap();
        let record = &state.records["agent-1"];
        if authority == "missing" {
            assert!(record.usage_uncertain);
            assert!(record.usage_exposure.is_none());
            assert!(record.cost.is_none());
            assert!(record.cost_microdollars.is_none());
            assert_eq!(record.status, DelegatedAgentStatus::Detached);
        } else {
            assert_eq!(record.usage, usage);
            assert_eq!(record.cost, Some(cost));
            assert_eq!(record.usage_uncertain, authority == "uncertain");
            assert!(record.usage_exposure.is_none());
            assert_eq!(
                record.cost_microdollars,
                (authority == "known").then_some(cost.total)
            );
        }
    }
}

#[test]
fn restart_reconciles_initial_delivery_only_on_active_ancestry_by_identity() {
    for abandoned in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let task = QueuedTask::initial("same payload".into());
        let QueuedTask::Initial(initial) = &task else {
            unreachable!()
        };
        let session_path = root.join("child.jsonl");
        let mut session = Session::create(&session_path).unwrap();
        session
            .append(crate::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text(task.format(&[]))],
                },
            )))
            .unwrap();
        if abandoned {
            session.checkout_root().unwrap();
            // Same content on the active branch is not the same delivery.
            session
                .append(crate::EntryValue::Message(octet_ai::Message::User(
                    octet_ai::UserMessage {
                        content: vec![octet_ai::UserPart::Text(
                            QueuedTask::initial("same payload".into()).format(&[]),
                        )],
                    },
                )))
                .unwrap();
        }
        drop(session);
        {
            let manager = writable_manager(root);
            let (child, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Pending);
            let mut state = manager.state.lock().unwrap();
            state
                .records
                .get_mut(&child.id)
                .unwrap()
                .pending_initial_task = Some(initial.clone());
            manager.persist_durable_fleet_locked(&mut state);
        }
        let manager = writable_manager(root);
        manager.restore_durable_fleet();
        assert_eq!(
            manager.restored_tasks("agent-1").len(),
            usize::from(abandoned)
        );
        let fleet: DurableFleet =
            serde_json::from_slice(&std::fs::read(fleet_roster_path(root, Path::new(""))).unwrap())
                .unwrap();
        assert_eq!(fleet.records[0].pending_initial_task.is_some(), abandoned);
    }
}

#[tokio::test]
async fn reattached_delivered_task_settles_and_explicit_follow_up_never_replays_mutation() {
    for result_persisted in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let session_path = root.join("child.jsonl");
        let task = QueuedTask::initial("original task".into());
        let QueuedTask::Initial(initial) = &task else {
            unreachable!()
        };
        let mut session = Session::create(&session_path).unwrap();
        let prompt = session
            .append(crate::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text(task.format(&[]))],
                },
            )))
            .unwrap();
        let model = test_template(root).model;
        session
            .append(crate::EntryValue::Message(octet_ai::Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![AssistantPart::ToolCall(octet_ai::ToolCall {
                        id: octet_ai::ToolCallId("prior-write".into()),
                        name: "write".into(),
                        arguments_json:
                            json!({"path": "effect.txt", "content": "original mutation"})
                                .to_string(),
                        argument_error: None,
                        async_execution: false,
                    })],
                    model: model.spec.id.clone(),
                    protocol: model.spec.protocol,
                },
            )))
            .unwrap();
        if result_persisted {
            session
                .append(crate::EntryValue::Message(octet_ai::Message::User(
                    octet_ai::UserMessage {
                        content: vec![octet_ai::UserPart::ToolResult(octet_ai::ToolResult {
                            tool_call_id: octet_ai::ToolCallId("prior-write".into()),
                            content: vec![octet_ai::ToolResultPart::Text("written".into())],
                            is_error: false,
                            added_tool_names: None,
                        })],
                    },
                )))
                .unwrap();
            // A driven interruption also leaves a checkpoint: not success evidence.
            session.checkpoint(prompt).unwrap();
        }
        drop(session);
        // The owner removed the original output after the effect occurred.
        // Replaying write would recreate it (no overwrite guard can mask replay).
        std::fs::write(root.join("effect.txt"), "original mutation").unwrap();
        std::fs::remove_file(root.join("effect.txt")).unwrap();
        {
            let manager = writable_manager(root);
            let (child, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
            let mut state = manager.state.lock().unwrap();
            let record = state.records.get_mut(&child.id).unwrap();
            record.pending_initial_task = Some(initial.clone());
            record.pending_messages.push_back(DirectedMessage {
                delivery_id: new_delivery_id().unwrap(),
                from: ROOT_AGENT_ID.into(),
                message: "retained steering".into(),
            });
            manager.persist_durable_fleet_locked(&mut state);
        }
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                    .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
                .expect(1).mount(&server).await;
        let mut manager = writable_manager_with_core_tools(root);
        let template = &mut Arc::get_mut(&mut manager).unwrap().template;
        Arc::make_mut(&mut template.model.endpoint).base_url =
            url::Url::parse(&format!("{}/", server.uri())).unwrap();
        Arc::make_mut(&mut template.model.endpoint).auth = octet_ai::Auth::None;
        template.effect_broker = crate::EffectBroker::new(crate::EffectPolicy::UnsafeHost);
        manager.restore_durable_fleet();
        let before = std::fs::read(&session_path).unwrap();
        for _ in 0..2 {
            manager.prepare_owning_run(&root_identity()).unwrap();
            let listed = manager.list_value_for(&root_identity()).unwrap();
            assert_eq!(listed["agents"][0]["status"]["state"], "interrupted");
            assert!(listed["agents"][0]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("subagent_continue"));
            assert!(!manager.state.lock().unwrap().records["agent-1"].live_task);
            assert_eq!(manager.current_permits().available_permits(), 3);
        }
        assert!(server.received_requests().await.unwrap().is_empty());
        assert_eq!(std::fs::read(&session_path).unwrap(), before);
        let fleet: DurableFleet =
            serde_json::from_slice(&std::fs::read(fleet_roster_path(root, Path::new(""))).unwrap())
                .unwrap();
        assert_eq!(fleet.records[0].status, DelegatedAgentStatus::Interrupted);
        assert!(fleet.records[0].pending_initial_task.is_none());
        assert_eq!(fleet.records[0].pending_messages.len(), 1);
        let resumed = manager
            .follow_up(
                &root_identity(),
                FollowUpRequest {
                    target: "agent-1".into(),
                    message: "continue from retained history".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(resumed["delivery"], "new_run");
        assert!(manager.state.lock().unwrap().records["agent-1"]
            .durable_diagnostic
            .is_none());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = manager.state.lock().unwrap().records["agent-1"]
                    .status
                    .clone();
                assert!(
                    !matches!(status, DelegatedAgentStatus::Failed { .. }),
                    "{status:?}"
                );
                if matches!(status, DelegatedAgentStatus::Completed { .. }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        stop_fixture_worker(manager).await;
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(String::from_utf8_lossy(&requests[0].body).contains("retained steering"));
        assert!(
            !root.join("effect.txt").exists(),
            "prior mutation was replayed"
        );
        let session = Session::open_read_only(&session_path).unwrap();
        let delivered = session.entries().iter().filter(|entry| match &entry.value {
                crate::EntryValue::Message(octet_ai::Message::User(message)) => message.content.iter().any(|part| {
                    matches!(part, octet_ai::UserPart::Text(text) if delivery_ids_in_envelopes(text).contains(&initial.delivery_id))
                }),
                _ => false,
            }).count();
        assert_eq!(delivered, 1);
    }
}

#[tokio::test]
async fn unreadable_session_authority_retains_work_until_explicit_or_auto_repair() {
    for automatic in [false, true] {
        for already_delivered in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            let session_path = root.join("child.jsonl");
            let saved_path = root.join("child.saved");
            let QueuedTask::Initial(mut initial) = QueuedTask::initial("initial work".into())
            else {
                unreachable!()
            };
            initial.attempts = 2;
            let message = DirectedMessage {
                delivery_id: new_delivery_id().unwrap(),
                from: ROOT_AGENT_ID.into(),
                message: "accepted message".into(),
            };
            let follow_up = QueuedFollowUp {
                delivery_id: new_delivery_id().unwrap(),
                from: ROOT_AGENT_ID.into(),
                message: "accepted follow-up".into(),
                attempts: 1,
            };
            let mut session = Session::create(&session_path).unwrap();
            if already_delivered {
                for text in [
                    QueuedTask::Initial(initial.clone()).format(std::slice::from_ref(&message)),
                    format_follow_up(&follow_up, &[]),
                ] {
                    session
                        .append(crate::EntryValue::Message(octet_ai::Message::User(
                            octet_ai::UserMessage {
                                content: vec![octet_ai::UserPart::Text(text)],
                            },
                        )))
                        .unwrap();
                }
            }
            drop(session);
            std::fs::rename(&session_path, &saved_path).unwrap();
            std::fs::create_dir(&session_path).unwrap();
            {
                let manager = writable_manager(root);
                let (child, _commands) =
                    insert_test_record(&manager, DelegatedAgentStatus::Pending);
                let mut state = manager.state.lock().unwrap();
                let record = state.records.get_mut(&child.id).unwrap();
                record.pending_initial_task = Some(initial.clone());
                record.pending_messages.push_back(message.clone());
                record.pending_follow_ups.push_back(follow_up.clone());
                record.queued_follow_ups.add_usage(follow_up.usage());
                manager.persist_durable_fleet_locked(&mut state);
            }
            // Repeat reconstruction while authority is unreadable: neither
            // restore's rewrite nor failed reattachment may destroy work.
            {
                let manager = writable_manager(root);
                manager.restore_durable_fleet();
                let state = manager.state.lock().unwrap();
                let record = &state.records["agent-1"];
                assert!(!record.live_task);
                assert!(record
                    .durable_diagnostic
                    .as_deref()
                    .unwrap()
                    .contains("authority could not be read"));
                assert_eq!(record.pending_initial_task.as_ref().unwrap().attempts, 2);
                assert_eq!(record.pending_follow_ups[0].attempts, 1);
                assert_eq!(record.pending_messages[0].delivery_id, message.delivery_id);
            }
            let server = MockServer::start().await;
            let expected_runs = if already_delivered { 1 } else { 3 };
            Mock::given(method("POST")).and(path("/chat/completions"))
                    .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                        .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
                    .expect(expected_runs).mount(&server).await;
            let mut manager = writable_manager(root);
            let template = &mut Arc::get_mut(&mut manager).unwrap().template;
            Arc::make_mut(&mut template.model.endpoint).base_url =
                url::Url::parse(&format!("{}/", server.uri())).unwrap();
            Arc::make_mut(&mut template.model.endpoint).auth = octet_ai::Auth::None;
            manager.restore_durable_fleet();
            manager.prepare_owning_run(&root_identity()).unwrap();
            assert!(!manager.state.lock().unwrap().records["agent-1"].live_task);
            assert!(manager
                .follow_up(
                    &root_identity(),
                    FollowUpRequest {
                        target: "agent-1".into(),
                        message: "must refuse".into(),
                    }
                )
                .await
                .unwrap_err()
                .contains("could not be reopened"));
            assert!(server.received_requests().await.unwrap().is_empty());
            let fleet: DurableFleet = serde_json::from_slice(
                &std::fs::read(fleet_roster_path(root, Path::new(""))).unwrap(),
            )
            .unwrap();
            let retained = &fleet.records[0];
            assert_eq!(
                retained.pending_initial_task.as_ref().unwrap().delivery_id,
                initial.delivery_id
            );
            assert_eq!(
                retained.pending_initial_task.as_ref().unwrap().attempts,
                initial.attempts
            );
            assert_eq!(
                retained.pending_messages[0].delivery_id,
                message.delivery_id
            );
            assert_eq!(retained.queued_follow_ups[0], follow_up);
            // Repair the same authority. Automatic reattachment must run
            // the same identity reconciliation as explicit follow-up resume.
            std::fs::remove_dir(&session_path).unwrap();
            std::fs::rename(&saved_path, &session_path).unwrap();
            if automatic {
                manager.prepare_owning_run(&root_identity()).unwrap();
            }
            manager
                .follow_up(
                    &root_identity(),
                    FollowUpRequest {
                        target: "agent-1".into(),
                        message: "new B".into(),
                    },
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let done = {
                        let state = manager.state.lock().unwrap();
                        let record = &state.records["agent-1"];
                        assert!(
                            !matches!(record.status, DelegatedAgentStatus::Failed { .. }),
                            "{:?}",
                            record.status
                        );
                        matches!(record.status, DelegatedAgentStatus::Completed { .. })
                            && record.pending_initial_task.is_none()
                            && record.pending_messages.is_empty()
                            && record.pending_follow_ups.is_empty()
                    };
                    if done {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            stop_fixture_worker(manager).await;
            assert_eq!(
                server.received_requests().await.unwrap().len(),
                expected_runs as usize
            );
            let session = Session::open_read_only(&session_path).unwrap();
            let user_texts = session
                .entries()
                .iter()
                .filter_map(|entry| match &entry.value {
                    crate::EntryValue::Message(octet_ai::Message::User(message)) => {
                        Some(&message.content)
                    }
                    _ => None,
                })
                .flatten()
                .filter_map(|part| match part {
                    octet_ai::UserPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for id in [
                &initial.delivery_id,
                &message.delivery_id,
                &follow_up.delivery_id,
            ] {
                assert_eq!(
                        user_texts
                            .iter()
                            .filter(|text| delivery_ids_in_envelopes(text).contains(id))
                            .count(),
                        1,
                        "each accepted identity appears once: automatic={automatic}, previously_delivered={already_delivered}"
                    );
            }
        }
    }
}

#[tokio::test]
async fn unreadable_first_worker_does_not_strand_later_reattachment_plans() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"healthy completed\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
            .expect(1).mount(&server).await;
    let mut manager = writable_manager(root);
    let template = &mut Arc::get_mut(&mut manager).unwrap().template;
    Arc::make_mut(&mut template.model.endpoint).base_url =
        url::Url::parse(&format!("{}/", server.uri())).unwrap();
    Arc::make_mut(&mut template.model.endpoint).auth = octet_ai::Auth::None;
    let missing = root.join("missing.jsonl");
    let healthy = root.join("healthy.jsonl");
    Session::create(&healthy).unwrap();
    for (id, name, path) in [
        ("agent-1", "/root/missing", missing),
        ("agent-2", "/root/healthy", healthy),
    ] {
        insert_durable_detached_record(&manager, id, name, path, DelegatedAgentStatus::Detached);
        let QueuedTask::Initial(initial) = QueuedTask::initial(format!("work for {id}")) else {
            unreachable!()
        };
        manager
            .state
            .lock()
            .unwrap()
            .records
            .get_mut(id)
            .unwrap()
            .pending_initial_task = Some(initial);
    }
    manager.prepare_owning_run(&root_identity()).unwrap();
    {
        let state = manager.state.lock().unwrap();
        let unavailable = &state.records["agent-1"];
        assert_eq!(unavailable.status, DelegatedAgentStatus::Detached);
        assert!(!unavailable.live_task);
        assert!(unavailable.detached_commands.is_some());
        assert!(unavailable.pending_initial_task.is_some());
        assert!(state.records["agent-2"].live_task);
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = manager.state.lock().unwrap().records["agent-2"]
                .status
                .clone();
            assert!(
                !matches!(status, DelegatedAgentStatus::Failed { .. }),
                "{status:?}"
            );
            if matches!(status, DelegatedAgentStatus::Completed { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stop_fixture_worker(manager).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
fn stale_worker_liveness_drop_cannot_clear_replacement_liveness() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let old_liveness = WorkerLiveness::new(&manager, child.id.clone(), 1);
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&child.id).unwrap();
        // Old worker published its park, and the replacement was admitted
        // before the old future's liveness guard could finish dropping.
        record.worker_generation = 2;
        record.live_task = true;
    }
    drop(old_liveness);
    assert!(manager.state.lock().unwrap().records[&child.id].live_task);
    drop(WorkerLiveness::new(&manager, child.id.clone(), 2));
    assert!(!manager.state.lock().unwrap().records[&child.id].live_task);
}

struct PanickingModelResolver;

impl AgentModelResolver for PanickingModelResolver {
    fn resolve(
        &self,
        _selection: &AgentModelSelection,
        _parent: &octet_ai::Model,
        _reasoning: &octet_ai::ReasoningConfig,
    ) -> Result<ResolvedAgentModel, String> {
        panic!("real worker startup panic");
    }
    fn models(
        &self,
        _query: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<AgentModelDescriptor>, String> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn real_worker_panic_is_supervised_and_wakes_a_registered_waiter() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    manager
        .spawn(
            &root_identity(),
            SpawnRequest {
                task_name: "panic".into(),
                display_task_name: None,
                message: "panic in startup".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap();
    *manager.template.model_resolver.write().unwrap() = Some(Arc::new(PanickingModelResolver));
    let owner = root_identity();
    let cancellation = crate::CancellationToken::default();
    let wait = manager.wait(
        &owner,
        Duration::from_secs(30),
        &cancellation,
        MAX_PROVENANCE_TEXT_BYTES,
    );
    tokio::pin!(wait);
    assert!(
        futures_util::poll!(&mut wait).is_pending(),
        "waiter registers before the worker runs"
    );
    let result = tokio::time::timeout(Duration::from_secs(3), &mut wait)
        .await
        .unwrap()
        .unwrap();
    assert!(result.value.to_string().contains("worker task panicked"));
    let state = manager.state.lock().unwrap();
    assert!(!state.records["agent-1"].live_task);
    assert!(matches!(&state.records["agent-1"].status,
            DelegatedAgentStatus::Failed { error } if error.contains("settled by supervisor")));
}

#[test]
fn durable_queues_round_trip_in_order_with_delivery_identity() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Detached);
    let durable = {
        let mut state = manager.state.lock().unwrap();
        state.next_mailbox_delivery = 42;
        let record = state.records.get_mut(&child.id).unwrap();
        record.pending_messages.push_back(DirectedMessage {
            delivery_id: "test-delivery".into(),
            from: ROOT_AGENT_ID.into(),
            message: "first message".into(),
        });
        record.pending_messages.push_back(DirectedMessage {
            delivery_id: "test-delivery".into(),
            from: ROOT_AGENT_ID.into(),
            message: "second message".into(),
        });
        record.pending_follow_ups.push_back(QueuedFollowUp {
            delivery_id: "test-follow-up".into(),
            from: ROOT_AGENT_ID.into(),
            message: "retry me".into(),
            attempts: 2,
        });
        record.mailbox.push_back(MailboxMessage {
            kind: "message",
            from: ROOT_AGENT_ID.into(),
            task_name: None,
            message: "leased mail".into(),
            evictable: false,
            continued: true,
            leased: true,
        });
        record.mailbox_delivery = Some(MailboxDeliveryPlan {
            id: 41,
            complete_messages: 0,
            partial_bytes: 3,
            touched_messages: 1,
        });
        durable_fleet_record(record)
    };
    let (tx, rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let restored = DelegationManager::agent_record_from_durable(
        durable,
        test_effective_tool_policy(),
        None,
        tx,
        Some(rx),
    );
    assert_eq!(
        restored
            .pending_messages
            .iter()
            .map(|message| message.message.as_str())
            .collect::<Vec<_>>(),
        vec!["first message", "second message"]
    );
    assert_eq!(restored.pending_follow_ups[0].attempts, 2);
    assert_eq!(restored.mailbox_delivery.unwrap().id, 41);
    assert!(restored.mailbox[0].leased);
    assert!(restored.mailbox[0].continued);
}

#[tokio::test]
async fn queue_enqueue_is_not_acknowledged_when_roster_persistence_fails() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = writable_manager(directory.path());
    let blocked_roster = directory.path().join("blocked-roster");
    std::fs::create_dir(&blocked_roster).unwrap();
    Arc::get_mut(&mut manager)
        .expect("fixture manager is uniquely owned")
        .roster_path = Some(blocked_roster);
    let (child, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Detached);
    let root = manager.root_binding().identity;
    let error = manager
        .send_message(&root, &child.id, "must not be acknowledged".into())
        .await
        .expect_err("failed roster write must reject the enqueue");
    assert!(error.contains("could not persist queued message"));
    assert!(manager.state.lock().unwrap().persistence_error.is_some());
}

#[tokio::test]
async fn worker_abort_settles_once_and_wakes_waiters() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut(&child.id)
        .unwrap()
        .live_task = true;
    let notified = manager.changed.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    manager.mark_worker_aborted(&child.id, None, 0, "worker task panicked");
    tokio::time::timeout(Duration::from_secs(1), &mut notified)
        .await
        .expect("worker abort did not wake waiters");
    let state = manager.state.lock().unwrap();
    assert!(matches!(
        &state.records[&child.id].status,
        DelegatedAgentStatus::Failed { error } if error.contains("worker task panicked")
    ));
    drop(state);
    manager.mark_worker_aborted(&child.id, None, 0, "worker task panicked");
    assert!(matches!(
        &manager.state.lock().unwrap().records[&child.id].status,
        DelegatedAgentStatus::Failed { .. }
    ));
}

#[tokio::test]
async fn aborted_worker_releases_steering_attempts_without_losing_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut(&child.id)
        .unwrap()
        .live_task = true;
    manager
        .send_message(&root_identity(), &child.id, "retain after abort".into())
        .await
        .unwrap();
    assert_eq!(manager.pending_message_count(&child.id), 1);
    drop(commands);
    manager.mark_worker_aborted(&child.id, None, 0, "worker task panicked");
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records[&child.id];
        assert!(record.inflight_message_ids.is_empty());
        assert_eq!(record.reserved_messages, QueueUsage::default());
        assert_eq!(record.pending_messages.len(), 1);
    }
    // A replacement worker must be able to take the retained input rather
    // than treating an attempt owned by the dead receiver as still live.
    let retained = manager.take_pending_messages(&child.id);
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].message, "retain after abort");
}

#[tokio::test]
async fn stale_supervisor_cannot_settle_a_same_claim_replacement_worker() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    manager
        .spawn(
            &root_identity(),
            SpawnRequest {
                task_name: "replace".into(),
                display_task_name: None,
                message: "original".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap();
    *manager.template.model_resolver.write().unwrap() = Some(Arc::new(PanickingModelResolver));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(
                manager.state.lock().unwrap().records["agent-1"].status,
                DelegatedAgentStatus::Failed { .. }
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (claim, generation, session_path, initial) = {
        let state = manager.state.lock().unwrap();
        let record = &state.records["agent-1"];
        (
            record.claim.clone(),
            record.worker_generation,
            record.session_path.clone(),
            record.pending_initial_task.clone().unwrap(),
        )
    };
    // Simulate a session append preceding the abnormal exit's lost roster
    // acknowledgement. Same-process resume must reconcile it too.
    let mut session = Session::open(&session_path).unwrap();
    session
        .append(crate::EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text(
                    QueuedTask::Initial(initial).format(&[]),
                )],
            },
        )))
        .unwrap();
    drop(session);
    manager
        .follow_up(
            &root_identity(),
            FollowUpRequest {
                target: "agent-1".into(),
                message: "replacement".into(),
            },
        )
        .await
        .unwrap();
    // Do not yield to the replacement yet: generation fencing must already
    // hold at publication, not only after spawn_worker polls its future.
    let mailbox_len = manager.state.lock().unwrap().root_mailbox.len();
    manager.mark_worker_aborted(
        "agent-1",
        claim.as_ref(),
        generation,
        "old worker task panicked",
    );
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records["agent-1"];
        assert_eq!(record.claim, claim);
        assert_eq!(record.worker_generation, generation + 1);
        assert_eq!(record.status, DelegatedAgentStatus::Pending);
        assert!(record.live_task);
        assert!(record.pending_initial_task.is_none());
        assert_eq!(record.pending_follow_ups.len(), 1);
        assert_eq!(state.root_mailbox.len(), mailbox_len);
    }
    stop_fixture_worker(manager).await;
}

#[test]
fn undelivered_tasks_dead_letter_after_a_small_durable_cap() {
    let mut queued = VecDeque::new();
    let mut task = QueuedTask::initial("poison".into());
    for attempts in 1..MAX_UNDELIVERED_TASK_ATTEMPTS {
        assert!(matches!(
            restore_undelivered_task(
                &mut queued,
                task,
                false,
                &WorkerOutcome::Failed("append failed".into())
            ),
            TaskRestore::Restored { attempts: observed } if observed == attempts
        ));
        task = queued.pop_front().unwrap();
    }
    assert!(matches!(
        restore_undelivered_task(
            &mut queued,
            task,
            false,
            &WorkerOutcome::Failed("append failed".into())
        ),
        TaskRestore::DeadLettered { attempts } if attempts == MAX_UNDELIVERED_TASK_ATTEMPTS
    ));
    assert!(queued.is_empty());

    assert!(matches!(
        restore_undelivered_task(
            &mut queued,
            QueuedTask::initial("transient".into()),
            false,
            &WorkerOutcome::Failed("once".into())
        ),
        TaskRestore::Restored { attempts: 1 }
    ));
}

#[test]
fn prompt_message_reservations_hold_queue_capacity_until_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (child, _command_rx) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    {
        let mut state = manager.state.lock().unwrap();
        let pending = &mut state.records.get_mut(&child.id).unwrap().pending_messages;
        for index in 0..MAX_PENDING_MESSAGES {
            pending.push_back(DirectedMessage {
                delivery_id: format!("test-delivery-{index}"),
                from: ROOT_AGENT_ID.into(),
                message: format!("message-{index}"),
            });
        }
    }

    let leased = manager.take_pending_messages(&child.id);
    let candidate = DirectedMessage {
        delivery_id: "test-delivery".into(),
        from: ROOT_AGENT_ID.into(),
        message: "overflow".into(),
    };
    {
        let state = manager.state.lock().unwrap();
        let record = &state.records[&child.id];
        assert_eq!(record.pending_messages.len(), MAX_PENDING_MESSAGES);
        assert_eq!(record.reserved_messages.messages, MAX_PENDING_MESSAGES);
        assert!(!record_can_accept_pending_message(record, &candidate));
    }

    manager.release_prompt_message_reservations(&child.id, &leased);
    let state = manager.state.lock().unwrap();
    assert!(state.records[&child.id].pending_messages.is_empty());
    assert_eq!(
        state.records[&child.id].reserved_messages,
        QueueUsage::default()
    );
}

#[tokio::test]
async fn pending_messages_reject_overflow_without_evicting_durable_work() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (_identity, _command_rx) = insert_test_record(
        &manager,
        DelegatedAgentStatus::Completed {
            output: "done".into(),
        },
    );
    let owner = root_identity();

    for index in 0..MAX_PENDING_MESSAGES {
        manager
            .send_message(&owner, "/root/child", format!("pending-message-{index}"))
            .await
            .unwrap();
    }
    let error = manager
        .send_message(&owner, "/root/child", "overflow".into())
        .await
        .unwrap_err();
    assert!(error.contains("pending-message queue is full"), "{error}");

    let state = manager.state.lock().unwrap();
    let pending = &state.records["agent-1"].pending_messages;
    assert_eq!(pending.len(), MAX_PENDING_MESSAGES);
    assert_eq!(pending.front().unwrap().message, "pending-message-0");
    assert_eq!(
        pending.back().unwrap().message,
        format!("pending-message-{}", MAX_PENDING_MESSAGES - 1)
    );
}

#[tokio::test]
async fn follow_up_queue_is_bounded_and_interrupt_drain_preserves_reservations() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (_identity, mut command_rx) = insert_test_record(
        &manager,
        DelegatedAgentStatus::Completed {
            output: "done".into(),
        },
    );
    let owner = root_identity();

    for index in 0..MAX_QUEUED_FOLLOW_UPS {
        manager
            .follow_up(
                &owner,
                FollowUpRequest {
                    target: "/root/child".into(),
                    message: format!("follow-up-{index}"),
                },
            )
            .await
            .unwrap();
    }
    let error = manager
        .follow_up(
            &owner,
            FollowUpRequest {
                target: "/root/child".into(),
                message: "overflow".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("follow-up queue is full"), "{error}");
    assert_eq!(
        manager.state.lock().unwrap().records["agent-1"]
            .queued_follow_ups
            .messages,
        MAX_QUEUED_FOLLOW_UPS
    );

    let mut queued_tasks = VecDeque::new();
    assert!(!manager.drain_interrupted_commands("agent-1", &mut command_rx, &mut queued_tasks,));
    assert_eq!(queued_tasks.len(), MAX_QUEUED_FOLLOW_UPS);
    assert!(queued_tasks
        .iter()
        .all(|task| matches!(task, QueuedTask::FollowUp(_))));
    assert_eq!(
        manager.state.lock().unwrap().records["agent-1"]
            .queued_follow_ups
            .messages,
        MAX_QUEUED_FOLLOW_UPS
    );
}

#[test]
fn waiter_registration_is_bounded_and_released_by_raii() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let owner = root_identity();
    let limit = manager.config.limits.max_total_agents;
    let guards = (0..limit)
        .map(|_| manager.register_waiter(&owner).unwrap())
        .collect::<Vec<_>>();

    let error = manager
        .register_waiter(&owner)
        .err()
        .expect("waiter overflow must be rejected");
    assert!(error.contains("waiter limit reached"), "{error}");
    assert_eq!(manager.state.lock().unwrap().active_waiters, limit);
    drop(guards);
    assert_eq!(manager.state.lock().unwrap().active_waiters, 0);
}

#[tokio::test]
async fn child_actions_require_the_matching_running_owner() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (identity, _command_rx) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    assert!(manager.list_value_for(&identity).is_ok());

    let mut spoofed = identity.clone();
    spoofed.path = "/root/not-child".into();
    let error = manager.list_value_for(&spoofed).unwrap_err();
    assert!(error.contains("identity does not match"), "{error}");

    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut("agent-1")
        .unwrap()
        .status = DelegatedAgentStatus::Completed {
        output: "done".into(),
    };
    let error = manager
        .send_message(&identity, ROOT_AGENT_ID, "stale child".into())
        .await
        .unwrap_err();
    assert!(error.contains("owner is not running"), "{error}");

    let mut state = manager.state.lock().unwrap();
    let record = state.records.get_mut("agent-1").unwrap();
    record.status = DelegatedAgentStatus::Running;
    record.shutdown.cancel();
    drop(state);
    let error = manager.list_value_for(&identity).unwrap_err();
    assert!(error.contains("owner is not running"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_and_follow_up_publication_are_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (_identity, mut command_rx) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let owner = root_identity();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));

    let follow_manager = Arc::clone(&manager);
    let follow_owner = owner.clone();
    let follow_barrier = Arc::clone(&barrier);
    let follow = tokio::spawn(async move {
        follow_barrier.wait().await;
        follow_manager
            .follow_up(
                &follow_owner,
                FollowUpRequest {
                    target: "/root/child".into(),
                    message: "race".into(),
                },
            )
            .await
    });
    let interrupt_manager = Arc::clone(&manager);
    let interrupt_owner = owner.clone();
    let interrupt_barrier = Arc::clone(&barrier);
    let interrupt = tokio::spawn(async move {
        interrupt_barrier.wait().await;
        interrupt_manager
            .interrupt(&interrupt_owner, "/root/child")
            .await
    });
    barrier.wait().await;
    let follow_result = follow.await.unwrap();
    let interrupt_result = interrupt.await.unwrap().unwrap();

    assert_eq!(interrupt_result["interrupt_requested"], true);
    let accepted = match follow_result {
        Ok(_) => {
            assert_eq!(command_rx.len(), 1);
            true
        }
        Err(error) => {
            assert!(error.contains("being interrupted"), "{error}");
            assert_eq!(command_rx.len(), 0);
            false
        }
    };
    let mut queued_tasks = VecDeque::new();
    manager.drain_interrupted_commands("agent-1", &mut command_rx, &mut queued_tasks);
    assert_eq!(queued_tasks.len(), usize::from(accepted));
    assert_eq!(
        manager.state.lock().unwrap().records["agent-1"]
            .queued_follow_ups
            .messages,
        usize::from(accepted)
    );
}

#[test]
fn delegated_snapshot_reads_do_not_consume_uncertainty_only_accounting() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let manager = writable_manager(&root);
    let (identity, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&identity.id).unwrap();
        record.extension_principal = Some("extension".into());
        record.usage = Usage::default();
        record.turn_count = 0;
        record.tool_call_count = 0;
        record.cost = None;
        record.usage_uncertain = true;
    }
    for _ in 0..2 {
        let snapshots = manager.extension_usage_records(ROOT_AGENT_ID);
        assert_eq!(snapshots.len(), 1);
        assert!(snapshots[0].usage_uncertain);
        assert_eq!(snapshots[0].usage, Usage::default());
    }
    // Fleet persistence cannot acknowledge a root ledger append.
    {
        let mut state = manager.state.lock().unwrap();
        manager.persist_durable_fleet_locked(&mut state);
    }
    let reopened = writable_manager(&root);
    reopened.restore_durable_fleet();
    let snapshots = reopened.extension_usage_records(ROOT_AGENT_ID);
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].usage_uncertain);
}

#[test]
fn durable_spawn_idempotency_checks_original_message_owner_and_policy_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let manager = writable_manager_with_core_tools(&root);
    let (identity, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let mut requested_policy = test_extension_policy();
    requested_policy.max_turns = None; // The effective child policy is Some(4).
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&identity.id).unwrap();
        record.display_task_name = Some("research".into());
        record.extension_principal = Some("extension-a".into());
        record.extension_idempotency_key = Some("spawn-1".into());
        record.extension_resource_owner = Some("root-owner".into());
        record.extension_message_sha256 = Some(format!("{:x}", Sha256::digest(b"find it")));
        record.extension_requested_policy = Some(requested_policy.clone());
        record.extension_policy = Some(test_extension_policy());
        manager.persist_durable_fleet_locked(&mut state);
    }
    drop(manager);
    let manager = writable_manager_with_core_tools(&root);
    manager.restore_durable_fleet();
    let service = manager
        .root_binding()
        .extension_service("extension-a", "parent-session", "root-owner")
        .unwrap();
    assert_eq!(
        service.list("root-owner").unwrap()["agents"][0]["agent_id"],
        identity.id
    );
    assert_eq!(
        service
            .resolve_owned_target(&manager, "root-owner", &identity.id)
            .unwrap(),
        identity.id
    );
    let foreign = manager
        .root_binding()
        .extension_service("extension-b", "parent-session", "root-owner")
        .unwrap();
    assert!(foreign.list("root-owner").unwrap()["agents"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(service.list("foreign-owner").is_err());
    let request = |message: &str| {
        let mut request = test_extension_spawn("research", None, None, message, "spawn-1");
        request.policy = requested_policy.clone();
        request
    };
    assert_eq!(
        service.spawn("root-owner", request("find it")).unwrap()["agent_id"],
        identity.id
    );
    service.state.lock().unwrap().owners.clear(); // Force durable fallback, not the cache.
    assert!(service
        .spawn("root-owner", request("changed task"))
        .unwrap_err()
        .contains("different input"));
    assert!(manager
        .extension_owned_record("extension-a", "foreign-owner", "spawn-1")
        .is_none());
    assert!(manager
        .extension_owned_record("extension-b", "root-owner", "spawn-1")
        .is_none());
    assert_eq!(manager.state.lock().unwrap().records.len(), 1);
    // Old rosters without verifiable ownership/hash must refuse, not silently replay.
    manager
        .state
        .lock()
        .unwrap()
        .records
        .get_mut(&identity.id)
        .unwrap()
        .extension_resource_owner = None;
    assert!(service
        .spawn("root-owner", request("find it"))
        .unwrap_err()
        .contains("different input"));
    assert_eq!(manager.state.lock().unwrap().records.len(), 1);
}

#[test]
fn streamed_output_progress_is_throttled_and_separate_from_reported_usage() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (identity, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    let mut telemetry = manager.attach_telemetry();
    let usage = Usage {
        input_tokens: 10,
        cache_read_tokens: 20,
        cache_write_tokens: 5,
        output_tokens: 4,
        total_tokens: 39,
        ..Usage::default()
    };
    manager.update_agent_usage(&identity.id, usage, Some(7), true);
    telemetry.borrow_and_update();

    let mut last_update = None;
    manager.update_agent_streamed_output(&identity.id, 40, &mut last_update);
    {
        let snapshot = telemetry.borrow_and_update();
        let live = &snapshot.as_ref().unwrap().children[0];
        assert_eq!(live.output_tokens, 4);
        assert_eq!(live.estimated_output_tokens, Some(14));
        assert_eq!(live.total_tokens, 39);
        assert_eq!(live.cost_microdollars, Some(7));
    }
    manager.update_agent_streamed_output(&identity.id, 40, &mut last_update);
    assert!(
        !telemetry.has_changed().unwrap(),
        "stream updates are coalesced"
    );
    last_update = Some(Instant::now() - STREAMED_OUTPUT_UPDATE_INTERVAL);
    manager.update_agent_streamed_output(&identity.id, 4, &mut last_update);
    assert_eq!(
        telemetry.borrow_and_update().as_ref().unwrap().children[0].estimated_output_tokens,
        Some(25)
    );

    manager.clear_agent_streamed_output(&identity.id);
    assert_eq!(
        telemetry.borrow_and_update().as_ref().unwrap().children[0].estimated_output_tokens,
        None,
        "a retry must discard provisional generation"
    );
    manager.update_agent_streamed_output(&identity.id, 20, &mut None);
    manager.update_agent_usage(&identity.id, usage, Some(7), true);
    let snapshot = telemetry.borrow();
    let settled = &snapshot.as_ref().unwrap().children[0];
    assert_eq!(settled.estimated_output_tokens, None);
    assert_eq!(settled.output_tokens, 4);
    assert_eq!(
        manager.state.lock().unwrap().records[&identity.id].usage,
        usage
    );
}

#[test]
fn child_unknown_usage_is_sticky_and_preserves_the_known_subtotal_for_root_mirroring() {
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
        .extension_principal = Some("test-extension".into());
    let usage = Usage {
        input_tokens: 7,
        output_tokens: 4,
        total_tokens: 11,
        ..Usage::default()
    };
    manager.update_agent_usage(&identity.id, usage, Some(7), true);
    manager.mark_agent_usage_uncertain(&identity.id);
    manager.update_agent_usage(&identity.id, usage, Some(7), true);
    {
        let state = manager.state.lock().unwrap();
        let value = agent_record_value(state.records.get(&identity.id).unwrap());
        assert_eq!(value["usage_uncertain"], true);
        assert!(value["cost_microdollars"].is_null());
    }
    let mut session = Session::create(directory.path().join("accounting-child.jsonl")).unwrap();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("codex".into()),
            octet_ai::ModelId("model".into()),
            "inference",
        )
        .unwrap();
    session
        .record_compaction_usage(
            octet_ai::EndpointId("codex".into()),
            octet_ai::ModelId("model".into()),
            usage,
            Some(Cost {
                total: 7,
                ..Cost::default()
            }),
        )
        .unwrap();
    manager.update_agent_session_accounting(&identity.id, &session, true);
    let records = manager.extension_usage_records(ROOT_AGENT_ID);
    assert_eq!(records.len(), 1);
    assert!(records[0].usage_uncertain);
    assert_eq!(records[0].usage, usage);
    assert_eq!(records[0].cost.unwrap().total, 7);
    let state = manager.state.lock().unwrap();
    let value = agent_record_value(state.records.get(&identity.id).unwrap());
    assert_eq!(value["usage_uncertain"], true);
    assert!(value["cost_microdollars"].is_null());
}

#[test]
fn reasoning_changes_update_new_workers_but_preserve_existing_worker_pins() {
    use octet_ai::{ReasoningConfig, ReasoningEffort};
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let binding = manager.root_binding();
    let mut pinned = test_extension_policy();
    let initial = manager.template.resolve_model(None).unwrap();
    pinned.resolved_model = Some(initial.metadata);
    pinned.resolved_reasoning = Some(initial.reasoning.clone());
    for reasoning in [
        ReasoningConfig::Effort(ReasoningEffort::Max),
        ReasoningConfig::Effort(ReasoningEffort::Ultra),
        ReasoningConfig::Off,
        ReasoningConfig::Effort(ReasoningEffort::Low),
    ] {
        binding.update_reasoning(reasoning.clone());
        assert_eq!(
            manager.template.resolve_model(None).unwrap().reasoning,
            reasoning
        );
        assert_eq!(
            manager
                .template
                .resolve_model(Some(&pinned))
                .unwrap()
                .reasoning,
            initial.reasoning
        );
    }
}

#[test]
fn root_outage_limit_updates_bound_child_template() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let mut root = Agent::new(AgentConfig {
        client: manager.template.client.clone(),
        model: manager.template.model.clone(),
        session: Session::create(directory.path().join("root-limit.jsonl")).unwrap(),
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
    for (index, limit) in [
        Some(Duration::from_secs(13)),
        Some(Duration::from_secs(17)),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        root.set_max_network_wait(limit);
        assert_eq!(
            manager.template.runtime.read().unwrap().max_network_wait,
            limit
        );
        let identity = AgentIdentity {
            id: format!("child-{index}"),
            path: format!("/root/child-{index}"),
            depth: 1,
        };
        let child = manager
            .build_child_agent(
                Session::create(directory.path().join(format!("limit-child-{index}.jsonl")))
                    .unwrap(),
                &identity,
                None,
            )
            .unwrap();
        assert_eq!(child.max_network_wait(), limit);
    }
}

#[test]
fn child_uses_runtime_settings_updated_after_delegation_activation() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let binding = manager.root_binding();
    let compaction_model = manager.template.model.clone();
    let audio = octet_ai::OutputModalities::TextAndAudio(octet_ai::AudioOutputOptions {
        format: octet_ai::AudioFormat::Wav,
        voice: octet_ai::AudioVoice::Named("alloy".into()),
    });
    let mut settings = manager.template.runtime.read().unwrap().clone();
    settings.compaction_model = Some(compaction_model.clone());
    settings.auto_compaction_mode = AgentCompactionMode::Disabled;
    settings.auto_compaction_threshold = 0.7;
    settings.compaction_keep_recent_tokens = 777;
    settings.completion_policy = CompletionPolicy::TerminalGate;
    settings.output_modalities = audio.clone();
    settings.max_output_tokens = 777;
    settings.max_session_tokens = Some(84_000);
    settings.max_session_cost_microdollars = Some(42);
    settings.provider_retries_enabled = false;
    settings.max_network_wait = Some(Duration::from_secs(17));
    binding.update_runtime_settings(settings);

    let session = Session::create(directory.path().join("child.jsonl")).unwrap();
    let identity = AgentIdentity {
        id: "agent-1".into(),
        path: "/root/child".into(),
        depth: 1,
    };
    let child = manager.build_child_agent(session, &identity, None).unwrap();

    assert_eq!(
        child.compaction_model().unwrap().spec.id,
        compaction_model.spec.id
    );
    assert_eq!(child.compaction_mode(), AgentCompactionMode::Disabled);
    assert_eq!(child.compaction_token_policy(), (false, 0.7, 777));
    assert_eq!(child.completion_policy(), CompletionPolicy::TerminalGate);
    assert_eq!(child.output_modalities(), &audio);
    assert_eq!(child.max_output_tokens(), 777);
    assert_eq!(child.max_session_tokens(), Some(84_000));
    assert_eq!(child.max_network_wait(), Some(Duration::from_secs(17)));
    let settings = manager.template.runtime.read().unwrap();
    assert_eq!(settings.max_session_tokens, Some(84_000));
    assert_eq!(settings.max_session_cost_microdollars, Some(42));
    assert!(!settings.provider_retries_enabled);
}

#[tokio::test]
async fn worker_start_failure_retains_accepted_work_for_explicit_retry() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let (identity, _command_rx) = insert_test_record(&manager, DelegatedAgentStatus::Pending);
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&identity.id).unwrap();
        record.pending_messages.push_back(DirectedMessage {
            delivery_id: "test-delivery".into(),
            from: ROOT_AGENT_ID.into(),
            message: "queued".into(),
        });
        record.reserved_messages = QueueUsage {
            messages: 1,
            bytes: 6,
        };
        record.queued_follow_ups = QueueUsage {
            messages: 1,
            bytes: 8,
        };
    }

    manager.fail_worker_start(&identity.id, "could not build child".into());

    {
        let state = manager.state.lock().unwrap();
        let record = &state.records[&identity.id];
        assert!(matches!(record.status, DelegatedAgentStatus::Failed { .. }));
        assert!(!record.shutdown.is_cancelled());
        assert_eq!(record.pending_messages.len(), 1);
        assert_eq!(record.reserved_messages.messages, 1);
        assert_eq!(record.queued_follow_ups.messages, 1);
    }
    let delivered = manager
        .send_message(&root_identity(), &identity.id, "too late".into())
        .await
        .unwrap();
    assert_eq!(delivered["delivery"], "queued");
    assert_eq!(
        manager.state.lock().unwrap().records[&identity.id]
            .pending_messages
            .len(),
        2
    );
}

#[test]
fn provenance_failure_rolls_back_spawn_and_fails_the_team_closed() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager_with_journal(read_only_journal(directory.path()), directory.path());
    let owner = AgentIdentity {
        id: ROOT_AGENT_ID.into(),
        path: ROOT_AGENT_PATH.into(),
        depth: 0,
    };

    let error = manager
        .spawn(
            &owner,
            SpawnRequest {
                task_name: "child".into(),
                display_task_name: None,
                message: "must not launch".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();
    assert!(error.contains("persist delegation provenance"), "{error}");
    let state = manager.state.lock().unwrap();
    assert!(state.records.is_empty());
    assert_eq!(state.total_agents, 1);
    assert!(state.persistence_error.is_some());
    assert!(state.shutting_down);
    drop(state);
    assert!(!directory.path().join("0001-child.jsonl").exists());

    let second_error = manager
        .spawn(
            &owner,
            SpawnRequest {
                task_name: "second".into(),
                display_task_name: None,
                message: "still closed".into(),
                extension_policy: None,
                extension_provenance: None,
            },
        )
        .unwrap_err();
    assert!(second_error.contains("persistence is unavailable"));
}

#[tokio::test]
async fn message_is_not_delivered_when_provenance_cannot_be_persisted() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager_with_journal(read_only_journal(directory.path()), directory.path());
    let (command_tx, _command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    {
        let record = fixture_record(
            DurableFleetRecord {
                agent_id: "agent-1".into(),
                agent_path: "/root/child".into(),
                parent_id: ROOT_AGENT_ID.into(),
                depth: 1,
                task_name: "child".into(),
                session_path: directory.path().join("child.jsonl"),
                status: DelegatedAgentStatus::Completed {
                    output: "done".into(),
                },
                created_at_ms: 1,
                started_at_ms: Some(1),
                ..DurableFleetRecord::default()
            },
            false,
            false,
            command_tx,
            None,
        );
        let mut state = manager.state.lock().unwrap();
        state.records.insert("agent-1".into(), record);
    }
    let owner = AgentIdentity {
        id: ROOT_AGENT_ID.into(),
        path: ROOT_AGENT_PATH.into(),
        depth: 0,
    };

    let error = manager
        .send_message(&owner, "/root/child", "not durable".into())
        .await
        .unwrap_err();
    assert!(error.contains("persist message provenance"), "{error}");
    let state = manager.state.lock().unwrap();
    assert!(state.persistence_error.is_some());
    assert!(state.records["agent-1"].pending_messages.is_empty());
}

#[tokio::test]
async fn roster_output_prefixes_preserve_full_child_sessions_after_reload() {
    for (count, bytes, extension) in [(16, 16 * 1024, true), (2, 128 * 1024, false)] {
        let directory = tempfile::tempdir().unwrap();
        let output = "x".repeat(bytes);
        {
            let manager = writable_manager(directory.path());
            for index in 0..count {
                let session_path = directory.path().join(format!("child-{index}.jsonl"));
                let mut session = Session::create(&session_path).unwrap();
                // The production TurnFinished boundary has already appended
                // this complete message before execute_child_run collects it.
                session
                    .append(crate::session::EntryValue::Message(
                        octet_ai::Message::Assistant(octet_ai::AssistantMessage {
                            content: vec![AssistantPart::Text(output.clone())],
                            model: manager.template.model.spec.id.clone(),
                            protocol: manager.template.model.spec.protocol,
                        }),
                    ))
                    .unwrap();
                drop(session);
                let id = format!("agent-{}", index + 1);
                let policy = extension.then(|| {
                    let mut policy = test_extension_policy();
                    policy.max_output_bytes = bytes;
                    policy
                });
                insert_fixture_record(
                    &manager,
                    DurableFleetRecord {
                        agent_id: id.clone(),
                        agent_path: format!("/root/worker-{index}"),
                        parent_id: ROOT_AGENT_ID.into(),
                        depth: 1,
                        session_path,
                        status: DelegatedAgentStatus::Running,
                        extension_policy: policy,
                        ..DurableFleetRecord::default()
                    },
                    false,
                    false,
                    true,
                );
                let status = if index % 2 == 0 {
                    DelegatedAgentStatus::Completed {
                        output: output.clone(),
                    }
                } else {
                    DelegatedAgentStatus::LimitReached {
                        output: output.clone(),
                        turn_count: 1,
                        turn_limit: 1,
                    }
                };
                assert!(manager.set_status(&id, status, false));
            }
            assert!(manager.state.lock().unwrap().persistence_error.is_none());
        }
        let manager = writable_manager(directory.path());
        manager.restore_durable_fleet();
        let mut state = manager.state.lock().unwrap();
        assert_eq!(state.records.len(), count);
        for record in state.records.values_mut() {
            let prefix = roster_status_text(&mut record.status).unwrap();
            assert!(prefix.ends_with(ROSTER_OUTPUT_SUFFIX));
            let session = Session::open_read_only(&record.session_path).unwrap();
            let complete: Vec<&str> = session
                .entries()
                .iter()
                .filter_map(|entry| {
                    if let crate::session::EntryValue::Message(octet_ai::Message::Assistant(
                        message,
                    )) = &entry.value
                    {
                        message.content.iter().find_map(|part| match part {
                            AssistantPart::Text(text) => Some(text.as_str()),
                            _ => None,
                        })
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(complete, vec![output.as_str()]);
        }
    }
}

#[test]
fn allowed_worker_outputs_compose_with_durable_roster_budget() {
    for (count, bytes, character) in [
        (16, 16 * 1024, 'x'),
        (2, 128 * 1024, 'é'),
        (16, 16 * 1024, '\0'),
    ] {
        let output = character.to_string().repeat(bytes / character.len_utf8());
        let fleet = DurableFleet {
            version: FLEET_ROSTER_VERSION,
            root_session: PathBuf::from("root.jsonl"),
            records: (0..count)
                .map(|index| DurableFleetRecord {
                    agent_id: format!("agent-{index}"),
                    session_path: PathBuf::from(format!("child-{index}.jsonl")),
                    status: DelegatedAgentStatus::Completed {
                        output: output.clone(),
                    },
                    ..DurableFleetRecord::default()
                })
                .collect(),
            next_mailbox_delivery: 1,
            root_mailbox: VecDeque::new(),
            root_mailbox_delivery: None,
        };
        assert!(serde_json::to_vec(&fleet).unwrap().len() > ROSTER_PROJECTION_BYTES);
        let encoded = encode_durable_fleet(fleet).unwrap();
        assert!(encoded.len() <= MAX_FLEET_ROSTER_BYTES);
        let restored: DurableFleet = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.records.len(), count);
        for (index, record) in restored.records.iter().enumerate() {
            assert_eq!(
                record.session_path,
                PathBuf::from(format!("child-{index}.jsonl"))
            );
            let DelegatedAgentStatus::Completed { output: retained } = &record.status else {
                panic!("lost status")
            };
            assert!(retained.ends_with(ROSTER_OUTPUT_SUFFIX));
            assert!(output.starts_with(retained.strip_suffix(ROSTER_OUTPUT_SUFFIX).unwrap()));
        }
    }
    let small = DurableFleet {
        version: FLEET_ROSTER_VERSION,
        root_session: PathBuf::from("root.jsonl"),
        records: vec![DurableFleetRecord {
            status: DelegatedAgentStatus::Completed {
                output: "small answer".into(),
            },
            ..DurableFleetRecord::default()
        }],
        next_mailbox_delivery: 1,
        root_mailbox: VecDeque::new(),
        root_mailbox_delivery: None,
    };
    assert_eq!(
        encode_durable_fleet(small.clone()).unwrap(),
        serde_json::to_vec(&small).unwrap()
    );
    for status in [
        DelegatedAgentStatus::Failed {
            error: "e".repeat(128 * 1024),
        },
        DelegatedAgentStatus::AwaitingApproval {
            reason: "a".repeat(128 * 1024),
        },
    ] {
        let fleet = DurableFleet {
            version: FLEET_ROSTER_VERSION,
            root_session: PathBuf::from("root.jsonl"),
            records: vec![
                DurableFleetRecord {
                    status: status.clone(),
                    ..DurableFleetRecord::default()
                },
                DurableFleetRecord {
                    status: DelegatedAgentStatus::Completed {
                        output: "x".repeat(128 * 1024),
                    },
                    ..DurableFleetRecord::default()
                },
            ],
            next_mailbox_delivery: 1,
            root_mailbox: VecDeque::new(),
            root_mailbox_delivery: None,
        };
        let restored: DurableFleet =
            serde_json::from_slice(&encode_durable_fleet(fleet).unwrap()).unwrap();
        assert_eq!(restored.records[0].status, status);
    }
    let oversized_metadata = DurableFleet {
        version: FLEET_ROSTER_VERSION,
        root_session: PathBuf::from("x".repeat(MAX_FLEET_ROSTER_BYTES)),
        records: vec![],
        next_mailbox_delivery: 1,
        root_mailbox: VecDeque::new(),
        root_mailbox_delivery: None,
    };
    assert!(encode_durable_fleet(oversized_metadata).is_err());
}

#[tokio::test]
async fn child_event_service_is_owner_fenced_non_consuming_and_cancellable() {
    let directory = tempfile::tempdir().unwrap();
    let manager = writable_manager(directory.path());
    let binding = manager.root_binding();
    let service = binding.extension_service("child-observer", "parent-session", "root-owner").unwrap();
    let foreign = binding.extension_service("foreign-observer", "parent-session", "root-owner").unwrap();
    let (identity, _commands) = insert_test_record(&manager, DelegatedAgentStatus::Running);
    {
        let mut state = manager.state.lock().unwrap();
        let record = state.records.get_mut(&identity.id).unwrap();
        record.extension_policy = Some(test_extension_policy());
        record.extension_principal = Some("child-observer".into());
        record.usage.input_tokens = 17;
    }
    service.state.lock().unwrap().owners.entry("root-owner".into()).or_default().owned_agents.insert(identity.id.clone());
    manager.record_child_event(&identity.id, json!({"kind": "run_started", "message": "real accepted input"}));
    let cancellation = crate::CancellationToken::default();
    let first = service.events("root-owner", &identity.id, 0, Duration::ZERO, &cancellation).await.unwrap();
    let again = service.events("root-owner", &identity.id, 0, Duration::ZERO, &cancellation).await.unwrap();
    assert_eq!(first, again, "reading is not an acknowledgement or accounting mutation");
    assert_eq!(first["events"][0]["sequence"], 1);
    assert_eq!(manager.extension_usage_records(ROOT_AGENT_ID)[0].usage.input_tokens, 17);
    assert!(foreign.events("root-owner", &identity.id, 0, Duration::ZERO, &cancellation).await.is_err());
    assert!(service.events("foreign-owner", &identity.id, 0, Duration::ZERO, &cancellation).await.is_err());
    cancellation.cancel();
    assert!(service.events("root-owner", &identity.id, 1, Duration::from_secs(1), &cancellation).await.unwrap_err().contains("cancelled"));
    assert!(foreign.stop("root-owner", &identity.id).is_err());
    service.stop("root-owner", &identity.id).unwrap();
    service.stop("root-owner", &identity.id).unwrap();
    assert!(manager.state.lock().unwrap().records[&identity.id].shutdown.is_cancelled());
    assert_eq!(manager.extension_usage_records(ROOT_AGENT_ID)[0].usage.input_tokens, 17);
}

#[test]
fn bounded_text_preserves_utf8_boundaries() {
    let input = "é".repeat(MAX_PROVENANCE_TEXT_BYTES);
    let output = bounded_text(&input);
    assert!(output.ends_with("...[truncated]"));
    assert!(output.is_char_boundary(output.len()));
}
