//! Additional D host acceptance. These wire fixtures are not SDK-parity evidence.
use super::*;
use std::collections::BTreeSet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn evidence(name: &str, value: Value) {
    println!("D {name}: {value}");
    if let Ok(root) = std::env::var("OCTET_DISCOVERY_EVIDENCE_DIR") {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            std::path::Path::new(&root).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

fn agent(
    directory: &tempfile::TempDir,
    host: ExtensionHost,
    session: Session,
    server: &MockServer,
) -> Agent {
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::AnthropicMessages;
    Arc::make_mut(&mut model.endpoint).base_url =
        url::Url::parse(&format!("{}/", server.uri())).unwrap();
    Arc::make_mut(&mut model.endpoint).auth = octet_ai::Auth::bearer("local-scripted-no-inference");
    Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
    Agent::new(AgentConfig {
        client: octet_ai::AiClient::new(),
        model,
        session,
        system: "Local D acceptance probe".into(),
        sandbox: SandboxConfig::new(directory.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions: host,
        max_turns: Some(8),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap()
}

async fn requests(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.body_json::<Value>().unwrap())
        .collect()
}

// Read the real tool-result text from the provider request, not an independently
// computed cursor or a second host lookup that would itself change selection.
fn last_page(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .filter(|page| page["operations"].is_array()),
        Value::Array(values) => values.iter().rev().find_map(last_page),
        Value::Object(fields) => fields.values().find_map(last_page),
        _ => None,
    }
}

async fn barrier(directory: &tempfile::TempDir) -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::fs::write(
        directory.path().join("barrier.json"),
        listener.local_addr().unwrap().port().to_string(),
    )
    .unwrap();
    listener
}

async fn entered(listener: &TcpListener) -> TcpStream {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 7];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"entered");
        stream
    })
    .await
    .expect("real child did not reach execution barrier")
}

#[tokio::test]
async fn d07_secondary_foreign_and_busy_refuse_without_partial_pins() {
    let (directory, process, host) = fixture().await;
    let receiver = create(&process, "owner").await;
    let foreign = create(&process, "foreign").await;
    let busy = create(&process, "owner").await;
    host.applicable_operations("owner", lookup(&receiver, None, None))
        .unwrap();
    let before = calls(&directory).len();
    let refusal = process
        .call_tool(
            "op_000",
            json!({"circuit":receiver,"secondary":foreign}),
            process.current_context_for_resource_owner("owner"),
        )
        .await
        .unwrap_err();
    assert!(refusal.to_string().contains("resource_unavailable"));
    assert_eq!(
        calls(&directory).len(),
        before,
        "foreign secondary never dispatched"
    );
    process
        .call_tool(
            "op_001",
            json!({"circuit":receiver}),
            process.current_context_for_resource_owner("owner"),
        )
        .await
        .expect("foreign-secondary refusal left a partial receiver pin");

    let listener = barrier(&directory).await;
    let child = process.clone();
    let held = busy.clone();
    let running = tokio::spawn(async move {
        child
            .call_tool(
                "op_019",
                json!({"circuit":held}),
                child.current_context_for_resource_owner("owner"),
            )
            .await
    });
    let mut permit = entered(&listener).await;
    let before = calls(&directory)
        .iter()
        .filter(|entry| entry["kind"] == "call")
        .count();
    let refusal = process
        .call_tool(
            "op_000",
            json!({"circuit":receiver,"secondary":busy}),
            process.current_context_for_resource_owner("owner"),
        )
        .await
        .unwrap_err();
    assert!(refusal.to_string().contains("resource_busy"));
    assert_eq!(
        calls(&directory)
            .iter()
            .filter(|entry| entry["kind"] == "call")
            .count(),
        before
    );
    process
        .release_resource("owner", &receiver)
        .expect("failed joint admission left a partial pin");
    assert!(process
        .release_resource("owner", &busy)
        .unwrap_err()
        .to_string()
        .contains("resource_busy"));
    std::fs::remove_file(directory.path().join("barrier.json")).unwrap();
    permit.write_all(b"x").await.unwrap();
    assert!(running.await.unwrap().unwrap().content.contains("count 1"));

    // The opposite ordering: once B has settled, both distinct slots succeed.
    let fresh = create(&process, "owner").await;
    process
        .call_tool(
            "op_000",
            json!({"circuit":fresh,"secondary":busy}),
            process.current_context_for_resource_owner("owner"),
        )
        .await
        .unwrap();
    process.release_resource("owner", &fresh).unwrap();
    process.release_resource("owner", &busy).unwrap();
    assert!(process.shutdown().await);
    let log = calls(&directory);
    assert_eq!(
        log.iter().filter(|entry| entry["name"] == "op_000").count(),
        1
    );
    evidence("d07-secondary-admission", json!({"child_log":log}));
}

#[tokio::test]
async fn d10_receiver_marker_changes_only_presentation_not_schema_effect_or_pins() {
    let (directory, process, host) = fixture().await;
    let (_, tools) = host.tool_snapshot();
    let unmarked = tools
        .iter()
        .find(|tool| tool.definition().name == "op_001")
        .unwrap();
    let marked = tools
        .iter()
        .find(|tool| tool.definition().name == "op_002")
        .unwrap();
    assert_eq!(
        unmarked.definition().parameters,
        marked.definition().parameters
    );
    let sandbox = SandboxConfig::new(directory.path());
    let ctx = ToolContext {
        workspace: directory.path(),
        sandbox: &sandbox,
        execution_scope: "owner",
        resource_owner: "owner",
        active_skills: &[],
        registered_tools: &[],
        progress: crate::tool::ToolProgressSink::null(),
        cancellation: Default::default(),
    };
    let mut outputs = Vec::new();
    let resource = create(&process, "owner").await;
    for name in ["op_001", "op_002"] {
        let args = json!({"circuit":resource});
        assert_eq!(
            unmarked.effect(&args, &ctx).unwrap(),
            marked.effect(&args, &ctx).unwrap()
        );
        let page = host
            .applicable_operations("owner", lookup(&resource, None, None))
            .unwrap();
        let card = page
            .operations
            .iter()
            .find(|card| card.tool == name)
            .unwrap();
        assert_eq!(card.primary_receiver, name == "op_002");
        let listener = barrier(&directory).await;
        let child = process.clone();
        let running = tokio::spawn(async move {
            child
                .call_tool(
                    name,
                    args,
                    child.current_context_for_resource_owner("owner"),
                )
                .await
        });
        let mut permit = entered(&listener).await;
        assert!(process
            .release_resource("owner", &resource)
            .unwrap_err()
            .to_string()
            .contains("resource_busy"));
        permit.write_all(b"x").await.unwrap();
        outputs.push(running.await.unwrap().unwrap().content);
    }
    process.release_resource("owner", &resource).unwrap();
    assert_eq!(outputs[0], outputs[1]);
    assert!(process.shutdown().await);
    evidence(
        "d10-receiver-equivalence",
        json!({"outputs":outputs,"child_log":calls(&directory)}),
    );
}

#[tokio::test]
async fn d08_d09_actual_agent_pagination_replaces_selection_without_eager_schemas() {
    let (directory, process, host) = fixture().await;
    let session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let resource = create(&process, &session.resource_owner_key()).await;
    let originals = process.tool_definitions();
    let server = MockServer::start().await;
    let turn = Arc::new(AtomicUsize::new(0));
    let turns = turn.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |request: &wiremock::Request| {
            match turns.fetch_add(1, Ordering::SeqCst) {
                0 => scripted_turn(Some((DISCOVERY_TOOL_NAME, json!({"resource":resource})))),
                1 | 2 => {
                    let page =
                        last_page(&request.body_json::<Value>().unwrap()["messages"]).unwrap();
                    assert_eq!(page["operations"].as_array().unwrap().len(), 8);
                    assert!(page["next_cursor"].is_string());
                    scripted_turn(Some((
                        DISCOVERY_TOOL_NAME,
                        json!({"resource":resource,"cursor":page["next_cursor"]}),
                    )))
                }
                3 => scripted_turn(None),
                _ => panic!("unexpected provider request"),
            }
        })
        .mount(&server)
        .await;
    let mut agent = agent(&directory, host, session, &server);
    assert_eq!(
        agent
            .complete("page over operations without invoking any")
            .await
            .unwrap()
            .text,
        "done"
    );
    assert_eq!(turn.load(Ordering::SeqCst), 4);
    let requests = requests(&server).await;
    assert_eq!(requests[0]["tools"].as_array().unwrap().len(), 4);
    for request in &requests[1..4] {
        let page = last_page(&request["messages"]).unwrap();
        let cards = page["operations"].as_array().unwrap();
        let selected: BTreeSet<_> = cards
            .iter()
            .map(|card| card["tool"].as_str().unwrap())
            .collect();
        let projected: BTreeSet<_> = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str().filter(|name| name.starts_with("op_")))
            .collect();
        assert_eq!(
            projected, selected,
            "page replaces selection; no accumulated schemas"
        );
        for tool in request["tools"].as_array().unwrap() {
            if tool["name"] != DISCOVERY_TOOL_NAME {
                let exact = originals
                    .iter()
                    .find(|exact| tool["name"] == exact.name)
                    .unwrap();
                assert_eq!(tool["input_schema"], exact.parameters);
            }
        }
    }
    let final_page = last_page(&requests[3]["messages"]).unwrap();
    assert_eq!(final_page["operations"].as_array().unwrap().len(), 5);
    assert!(final_page["next_cursor"].is_null());
    assert_eq!(
        calls(&directory).len(),
        1,
        "discovery never auto-invokes operations"
    );
    assert_eq!(agent.registered_tool_names().len(), 104);
    assert!(process.shutdown().await);
    evidence(
        "agent-pagination",
        json!({"requests":requests,"child_log":calls(&directory)}),
    );
}

struct RetireBeforeDispatch {
    process: ExtensionProcess,
    remove: bool,
    changed: AtomicBool,
}
#[async_trait::async_trait]
impl ToolCallHook for RetireBeforeDispatch {
    async fn before_tool_call(
        &self,
        name: &str,
        args: &Value,
        ctx: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        if name == "op_000" && !self.changed.swap(true, Ordering::AcqRel) {
            if self.remove {
                self.process
                    .call_tool(
                        "replace_catalog",
                        json!({}),
                        self.process
                            .current_context_for_resource_owner(ctx.resource_owner),
                    )
                    .await
                    .map_err(|error| ToolError::new(error.to_string()))?;
            } else {
                let resource: ResourceRef =
                    serde_json::from_value(args["circuit"].clone()).unwrap();
                self.process
                    .release_resource(ctx.resource_owner, &resource)
                    .map_err(|error| ToolError::new(error.to_string()))?;
            }
        }
        Ok(())
    }
    async fn after_tool_call(&self, _: &str, _: &Value, _: &str, _: bool, _: &ToolContext<'_>) {}
}

#[tokio::test]
async fn d04_d06_actual_agent_frozen_removal_and_retirement_fail_closed() {
    for remove in [false, true] {
        let (directory, process, mut host) = fixture().await;
        let session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let resource = create(&process, &session.resource_owner_key()).await;
        if remove {
            std::fs::write(directory.path().join("remove-on-replace"), b"").unwrap();
        }
        host.tool_call_hook(RetireBeforeDispatch {
            process: process.clone(),
            remove,
            changed: AtomicBool::new(false),
        });
        let server = MockServer::start().await;
        let turn = Arc::new(AtomicUsize::new(0));
        let turns = turn.clone();
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                move |_: &wiremock::Request| match turns.fetch_add(1, Ordering::SeqCst) {
                    0 => scripted_turn(Some((DISCOVERY_TOOL_NAME, json!({"resource":resource})))),
                    1 => scripted_turn(Some((
                        "op_000",
                        json!({"circuit":resource,"secondary":resource}),
                    ))),
                    2 => scripted_turn(None),
                    _ => panic!("unexpected provider request"),
                },
            )
            .mount(&server)
            .await;
        let mut agent = agent(&directory, host, session, &server);
        assert_eq!(
            agent
                .complete("discover then use captured schema")
                .await
                .unwrap()
                .text,
            "done"
        );
        assert_eq!(turn.load(Ordering::SeqCst), 3);
        let requests = requests(&server).await;
        assert_eq!(requests[0]["tools"].as_array().unwrap().len(), 4);
        assert_eq!(requests[1]["tools"].as_array().unwrap().len(), 11);
        assert_eq!(
            requests[2]["tools"].as_array().unwrap().len(),
            if remove { 10 } else { 4 }
        );
        assert!(
            calls(&directory)
                .iter()
                .all(|entry| entry["name"] != "op_000"),
            "stale captured operation never enters child"
        );
        assert!(process.shutdown().await);
        evidence(
            &format!("agent-remove-{remove}"),
            json!({"requests":requests,"child_log":calls(&directory)}),
        );
    }
}

async fn pi_fixture(directory: &tempfile::TempDir) -> ExtensionProcess {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    let factory = root.join("test/fixtures/core.ts");
    // Load this existing checked-in Pi factory unchanged. The second ordinary Pi
    // factory only observes hook completion and records the real adapter PID.
    std::fs::write(
        directory.path().join("pi-observer.mjs"),
        include_str!("pi-observer.mjs"),
    )
    .unwrap();
    // Capture the real factory catalog, including exact schemas, optional
    // prompt metadata, flags and hooks. This raw Agent has no App resource
    // consumer: do not use an App-generated bridge or enable its feature merely
    // to pass registration. No captured factory registration is removed.
    let manifest_path = crate::extension_process::pi_fixture::capture(
        directory.path(),
        &[factory, directory.path().join("pi-observer.mjs")],
    )
    .await;
    let bundle = directory.path().join("octet-pi-compat");
    let reviewed: Value =
        serde_json::from_slice(&std::fs::read(bundle.join("bridge.json")).unwrap()).unwrap();
    let expected_tools: Vec<crate::extension_process::ToolDefinition> =
        serde_json::from_value(reviewed["registrations"]["tools"].clone()).unwrap();
    let mut manifest = ExtensionManifest::load(&manifest_path).unwrap();
    evidence(
        "pi-reviewed-registration",
        json!({
            "config":reviewed,
            "manifest":std::fs::read_to_string(&manifest_path).unwrap(),
            "boundary":"raw factory capture; not generated-App-bridge or resource-consumer acceptance",
        }),
    );
    manifest.entrypoint.env.insert(
        "HOME".into(),
        directory.path().to_string_lossy().into_owned(),
    );
    let descriptor = DiscoveredExtension {
        manifest,
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let mut config = ExtensionRuntimeConfig::new(directory.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(10);
    // Missing Node/dependencies is a test failure, never an ignored success.
    let process = ExtensionProcess::start(descriptor, config)
        .await
        .expect("D11 requires real local Node/Pi adapter");
    assert!(!process.supports_feature(crate::extension_process::EXTENSION_FEATURE_RESOURCE_PATHS));
    assert_eq!(
        process.tool_definitions(),
        expected_tools,
        "host ToolDefinitions must match the actual reviewed factory metadata"
    );
    process
}

#[tokio::test]
async fn d11_actual_agent_unchanged_pi_factory_coexists_with_lazy_operations() {
    let (directory, process, mut host) = fixture().await;
    let pi = pi_fixture(&directory).await;
    let ordinary = pi
        .tool_definitions()
        .into_iter()
        .find(|tool| tool.name == "core")
        .unwrap();
    assert!(
        ordinary.operation.is_none(),
        "Pi factory needs no resource annotations"
    );
    host.load(&pi);
    host.finalize_tool_surface();
    let session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let resource = create(&process, &session.resource_owner_key()).await;
    let server = MockServer::start().await;
    let turn = Arc::new(AtomicUsize::new(0));
    let turns = turn.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            move |_: &wiremock::Request| match turns.fetch_add(1, Ordering::SeqCst) {
                0 | 2 => scripted_turn(Some(("core", json!({"mode":"normal"})))),
                1 => scripted_turn(Some((DISCOVERY_TOOL_NAME, json!({"resource":resource})))),
                3 => scripted_turn(None),
                _ => panic!("unexpected provider request"),
            },
        )
        .mount(&server)
        .await;
    let mut agent = agent(&directory, host, session, &server);
    // The unchanged factory returns ctx.model in JSON details. Supply the actual
    // selected model, as the frontend does, rather than an absent model fixture.
    let spec = &agent.model().spec;
    pi.set_host_state(crate::extension_process::ExtensionHostState {
        model: Some(spec.id.0.clone()),
        model_view: Some(crate::extension_process::ExtensionModelView {
            id: spec.api_name.clone(),
            name: spec.display_name.clone(),
            base_url: None,
            api: "anthropic-messages".into(),
            provider: agent.model().endpoint.id.0.clone(),
            reasoning: spec.capabilities.reasoning.is_some(),
            input: if spec
                .capabilities
                .input_modalities
                .contains(octet_ai::Modality::Image)
            {
                vec!["text".into(), "image".into()]
            } else {
                vec!["text".into()]
            },
            cost: spec.pricing.as_ref().map(|pricing| {
                crate::extension_process::ExtensionModelCost {
                    input: pricing.input.0,
                    output: pricing.output.0,
                    cache_read: pricing.cache_read.0,
                    cache_write: pricing.cache_write_5m.0,
                }
            }),
            context_window: spec.limits.context_window,
            max_tokens: spec.limits.max_output_tokens,
        }),
        ..Default::default()
    });
    assert_eq!(agent.registered_tool_names().len(), 105);
    agent
        .set_active_tool_names(Some(BTreeSet::from(["core".to_owned()])))
        .unwrap();
    assert_eq!(agent.registered_tool_definitions().len(), 1);
    assert_eq!(agent.registered_tool_definitions()[0].name, "core");
    assert_eq!(
        agent.registered_tool_names().len(),
        105,
        "inactive lazy operations remain registered"
    );
    agent.set_active_tool_names(None).unwrap();
    assert_eq!(
        agent
            .complete("use ordinary Pi tool before and after resource discovery")
            .await
            .unwrap()
            .text,
        "done"
    );
    assert_eq!(turn.load(Ordering::SeqCst), 4);
    let requests = requests(&server).await;
    for (index, expected) in [5, 5, 12, 12].into_iter().enumerate() {
        let tools = requests[index]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), expected);
        let core = tools.iter().find(|tool| tool["name"] == "core").unwrap();
        assert_eq!(core["input_schema"], ordinary.parameters);
        assert_eq!(core["description"], ordinary.description);
    }
    let expected = format!("{}|false|default", directory.path().display());
    assert!(
        requests[1]["messages"].to_string().contains(&expected),
        "expected {expected:?}: {}",
        requests[1]["messages"]
    );
    assert!(
        requests[3]["messages"].to_string().contains(&expected),
        "expected {expected:?}: {}",
        requests[3]["messages"]
    );
    assert_eq!(agent.registered_tool_names().len(), 105);
    assert_eq!(agent.registered_tool_definitions().len(), 105);
    assert_eq!(
        calls(&directory).len(),
        1,
        "ordinary Pi calls do not invoke resource handlers"
    );
    assert!(pi.shutdown().await);
    assert!(process.shutdown().await);
    let pi_log: Vec<Value> = std::fs::read_to_string(directory.path().join("pi-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        pi_log
            .iter()
            .filter(|entry| entry["kind"] == "tool_result" && entry["tool"] == "core")
            .count(),
        2
    );
    assert!(pi_log
        .iter()
        .all(|entry| entry["pid"].as_u64().is_some_and(|pid| pid > 0)));
    evidence(
        "agent-pi-coexistence",
        json!({"requests":requests,"child_log":calls(&directory),"pi_log":pi_log}),
    );
}
