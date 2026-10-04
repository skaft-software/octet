//! Resource-specific nested admission through a real Agent-owned composition
//! scope and real Python/Rust ExtensionProcess targets. The small host composing
//! tool is a test script, NOT a fake ToolCompositionService or SDK parity peer.
use super::*;
use crate::extension::ToolCallHook;
use crate::extension_operations::ApplicableOperationsRequest;
use crate::tool_composition::{ToolCompositionConfig, ToolCompositionMode};
use crate::{Agent, AgentConfig, EffectBroker, EffectPolicy, SandboxConfig, Session};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Clone)]
struct Refusal {
    label: String,
    arguments: Value,
    // Wrong nominal types can fail the composition schema validator before the
    // process registry. R23 requires zero entry, not an identical error layer.
    code: Option<&'static str>,
}

fn target_calls(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap()
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|entry| entry["kind"] == "call" && entry["name"] == "use")
        .count()
}

fn assert_refusal(error: &str, case: &Refusal) {
    assert!(!error.is_empty(), "{}: missing refusal", case.label);
    if let Some(code) = case.code {
        assert!(error.contains(code), "{}: {error}", case.label);
    }
}

// Runs after ordinary nested schema/effect admission, before ProcessTool::execute.
// Revocation here is deliberately later than the frozen composition snapshot.
struct NestedHook {
    owner: String,
    seen: Arc<AtomicUsize>,
    revoke: Option<ExtensionHost>,
}

#[async_trait::async_trait]
impl ToolCallHook for NestedHook {
    async fn before_tool_call(
        &self,
        name: &str,
        _: &Value,
        context: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        if name == "use" {
            assert!(context.progress.is_programmatic());
            assert!(context.progress.composition_service().is_none());
            assert_eq!(context.resource_owner, self.owner);
            self.seen.fetch_add(1, Ordering::AcqRel);
            if let Some(host) = &self.revoke {
                let mut host = host.clone();
                host.set_tool_policy(|name| name != "use");
            }
        }
        Ok(())
    }

    async fn after_tool_call(&self, _: &str, _: &Value, _: &str, _: bool, _: &ToolContext<'_>) {}
}

struct ComposeProbe {
    owner: String,
    primary: ResourceRef,
    expected_schema: Value,
    cases: Vec<Refusal>,
    log: PathBuf,
    reports: Arc<StdMutex<Vec<Value>>>,
    // Refusal matrices make a healthy nested call after each rejection to prove
    // the primary was not partially pinned. The policy test keeps policy denied.
    healthy_followup: bool,
}

#[async_trait::async_trait]
impl Tool for ComposeProbe {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "r23_compose".into(),
            description: "Run bounded resource admission checks".into(),
            parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
            async_execution: false,
            constrained_sampling: None,
        }
    }

    fn composition_config(&self) -> Option<ToolCompositionConfig> {
        Some(ToolCompositionConfig {
            mode: ToolCompositionMode::Only,
            inline_budget: 0,
        })
    }

    fn effect(&self, _: &Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Extension)
    }

    async fn execute(&self, _: Value, context: &ToolContext<'_>) -> Result<ToolOutput, ToolError> {
        assert_eq!(context.resource_owner, self.owner);
        let service = context
            .progress
            .composition_service()
            .expect("actual Agent scope required");
        let frozen = service.context().await?;
        let definition = frozen["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "use")
            .expect("selected resource operation in frozen scope");
        assert_eq!(definition["parameters"], self.expected_schema);
        for (index, case) in self.cases.iter().enumerate() {
            let before = target_calls(&self.log);
            let error = service
                .call(
                    "use".into(),
                    case.arguments.clone(),
                    context.cancellation.clone(),
                )
                .await
                .unwrap_err()
                .to_string();
            assert_refusal(&error, case);
            assert_eq!(
                target_calls(&self.log),
                before,
                "{}: nested target entered",
                case.label
            );
            // Current policy must not rewrite this admitted scope's schema.
            assert_eq!(service.context().await?["tools"], frozen["tools"]);
            lock_std_mutex(&self.reports).push(json!({
                "entrypoint":"nested", "case":case.label, "error":error,
                "before":before, "after":target_calls(&self.log),
            }));
            if self.healthy_followup {
                let result = service
                    .call(
                        "use".into(),
                        json!({"resource":self.primary}),
                        context.cancellation.clone(),
                    )
                    .await?;
                assert_eq!(
                    result["count"],
                    index + 1,
                    "{}: native primary state",
                    case.label
                );
                assert_eq!(
                    target_calls(&self.log),
                    before + 1,
                    "{}: primary pin leaked",
                    case.label
                );
            }
        }
        Ok(ToolOutput::new("R23 nested checks complete"))
    }
}

fn response(compose: bool) -> ResponseTemplate {
    let frame = |event: &str, value: Value| format!("event: {event}\ndata: {value}\n\n");
    let mut body = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"local-r23","usage":{"input_tokens":5,"output_tokens":0}}}),
    );
    if compose {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"r23-call","name":"r23_compose"}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
        );
    } else {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}),
        );
    }
    body += &frame(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    );
    body += &frame(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":if compose {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":3}}),
    );
    body += &frame("message_stop", json!({"type":"message_stop"}));
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

async fn run_agent(fixture: &Fixture, session: Session, host: ExtensionHost) -> Value {
    let server = MockServer::start().await;
    let turns = Arc::new(AtomicUsize::new(0));
    let counter = turns.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            move |_: &wiremock::Request| match counter.fetch_add(1, Ordering::SeqCst) {
                0 => response(true),
                1 => response(false),
                _ => panic!("unexpected R23 provider request"),
            },
        )
        .mount(&server)
        .await;
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::AnthropicMessages;
    Arc::make_mut(&mut model.endpoint).base_url =
        url::Url::parse(&format!("{}/", server.uri())).unwrap();
    Arc::make_mut(&mut model.endpoint).auth = octet_ai::Auth::bearer("local-scripted-no-inference");
    Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
    let mut agent = Agent::new(AgentConfig {
        client: octet_ai::AiClient::new(),
        model,
        session,
        system: "Local R23 nested resource probe".into(),
        sandbox: SandboxConfig::new(fixture.temp.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions: host,
        max_turns: Some(3),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    assert_eq!(
        agent
            .complete("run resource admission checks")
            .await
            .unwrap()
            .text,
        "done"
    );
    assert_eq!(turns.load(Ordering::SeqCst), 2);
    let requests = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.body_json::<Value>().unwrap())
        .collect::<Vec<_>>();
    for request in &requests {
        let tools = request["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0]["name"], "r23_compose",
            "resource target is nested-only"
        );
    }
    assert!(requests[1]
        .to_string()
        .contains("R23 nested checks complete"));
    let receipts = agent
        .session()
        .entries()
        .iter()
        .filter_map(|entry| entry.metadata.as_ref()?.tool_composition.as_ref())
        .map(|receipt| serde_json::to_value(receipt).unwrap())
        .collect::<Vec<_>>();
    json!({"requests":requests,"receipts":receipts})
}

fn select(host: &ExtensionHost, owner: &str, resource: &ResourceRef) {
    let page = host
        .applicable_operations(
            owner,
            ApplicableOperationsRequest {
                resource: resource.clone(),
                limit: None,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(
        page.operations.len(),
        2,
        "receiver and secondary slot cards"
    );
    assert!(page
        .operations
        .iter()
        .all(|operation| operation.id == "demo.use"));
}

fn evidence(fixture: &Fixture, name: &str, reports: &[Value], agent: Value) {
    let value = json!({"case":name,"host_pid":std::process::id(),"rust":fixture.rust,
        "reports":reports,"agent":agent,"child_log":fixture.log()});
    println!(
        "R23 {name} rust={}: {} refusal observations",
        fixture.rust,
        reports.len()
    );
    if let Some(root) = std::env::var_os("OCTET_RESOURCE_EVIDENCE_DIR") {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            PathBuf::from(root).join(format!(
                "{}-{}-r23-{name}-{}.json",
                std::process::id(),
                fixture.serial,
                fixture.rust
            )),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn r23_all_entrypoints_receiver_and_secondary_refusals_real_peers() {
    for rust in [false, true] {
        let mut fixture = Fixture::start(rust, true, 4).await;
        let other = Fixture::start(rust, true, 4).await;
        let session = Session::create(fixture.temp.path().join("session.jsonl")).unwrap();
        let owner = session.resource_owner_key();
        let stale = reference(&fixture.call(&owner, "create", json!({})).await.unwrap());
        let generation = fixture.process.health_snapshot().generation;
        fixture.process.reload().await.unwrap();
        assert_ne!(fixture.process.health_snapshot().generation, generation);
        let primary = reference(&fixture.call(&owner, "create", json!({})).await.unwrap());
        let busy = reference(&fixture.call(&owner, "create", json!({})).await.unwrap());
        let foreign_owner = reference(
            &fixture
                .call("foreign-owner", "create", json!({}))
                .await
                .unwrap(),
        );
        let foreign_extension = reference(&other.call(&owner, "create", json!({})).await.unwrap());
        let wrong_type = ResourceRef {
            resource_type: "demo.Other".into(),
            ..primary.clone()
        };
        let fabricated = ResourceRef {
            resource: "r23-never-issued".into(),
            resource_type: "demo.Circuit".into(),
        };
        assert_ne!(primary.resource, stale.resource);
        let running = tokio::spawn(call(
            fixture.process.clone(),
            true,
            owner.clone(),
            "use".into(),
            json!({"resource":busy,"block":true}),
            CancellationToken::default(),
        ));
        let entered = loop {
            let event = fixture.event("entered").await;
            if event["name"] == "use" {
                break event;
            }
        };
        let mut cases = Vec::new();
        for (label, resource, code) in [
            ("fabricated", fabricated, Some("resource_unavailable")),
            ("stale-generation", stale, Some("resource_unavailable")),
            ("foreign-owner", foreign_owner, Some("resource_unavailable")),
            (
                "foreign-extension",
                foreign_extension,
                Some("resource_unavailable"),
            ),
            ("busy", busy.clone(), Some("resource_busy")),
            ("wrong-type", wrong_type, None),
        ] {
            for secondary in [false, true] {
                cases.push(Refusal {
                    label: format!(
                        "{label}-{}",
                        if secondary { "secondary" } else { "receiver" }
                    ),
                    arguments: if secondary {
                        json!({"resource":primary,"second":resource})
                    } else {
                        json!({"resource":resource})
                    },
                    code,
                });
            }
        }
        let reports = Arc::new(StdMutex::new(Vec::new()));
        let hooks = Arc::new(AtomicUsize::new(0));
        let mut host = ExtensionHost::new();
        fixture.process.register_dynamic_tool_catalog(&mut host);
        let schema = fixture
            .process
            .tool_definitions()
            .into_iter()
            .find(|tool| tool.name == "use")
            .unwrap()
            .parameters;
        host.tool(ComposeProbe {
            owner: owner.clone(),
            primary: primary.clone(),
            expected_schema: schema,
            cases: cases.clone(),
            log: fixture.temp.path().join("calls.jsonl"),
            reports: reports.clone(),
            healthy_followup: true,
        });
        host.tool_call_hook(NestedHook {
            owner: owner.clone(),
            seen: hooks.clone(),
            revoke: None,
        });
        host.finalize_tool_surface();
        select(&host, &owner, &primary);
        let (_, tools) = host.tool_snapshot();
        let registered = tools
            .iter()
            .find(|tool| tool.definition().name == "use")
            .unwrap();
        let sandbox = SandboxConfig::new(fixture.temp.path());
        let context = ToolContext {
            workspace: fixture.temp.path(),
            sandbox: &sandbox,
            execution_scope: "r23-baseline",
            resource_owner: &owner,
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: CancellationToken::default(),
        };
        for case in &cases {
            for direct in [true, false] {
                let before = target_calls(&fixture.temp.path().join("calls.jsonl"));
                let error = if direct {
                    fixture
                        .process
                        .call_tool(
                            "use",
                            case.arguments.clone(),
                            fixture
                                .process
                                .current_context_for_resource_owner(owner.clone()),
                        )
                        .await
                        .unwrap_err()
                        .to_string()
                } else {
                    registered
                        .execute(case.arguments.clone(), &context)
                        .await
                        .unwrap_err()
                        .to_string()
                };
                assert_refusal(&error, case);
                assert_eq!(
                    target_calls(&fixture.temp.path().join("calls.jsonl")),
                    before,
                    "{}: baseline target entered",
                    case.label
                );
                lock_std_mutex(&reports).push(json!({"entrypoint":if direct {"direct"} else {"registered"},"case":case.label,"error":error,"before":before,"after":before}));
            }
        }
        // Process registration holds a Weak policy registry. Keep its owning
        // host alive after run_agent drops the Agent, through the positive
        // control and native resource release below.
        let agent = run_agent(&fixture, session, host.clone()).await;
        assert_eq!(lock_std_mutex(&reports).len(), 36);
        // Two wrong-type calls are rejected before hooks/journaling; all other
        // refusals and twelve healthy calls traverse the real nested dispatcher.
        assert_eq!(hooks.load(Ordering::Acquire), 22);
        let receipts = agent["receipts"].as_array().unwrap();
        for kind in ["call_started", "call_finished"] {
            assert_eq!(
                receipts
                    .iter()
                    .filter(|receipt| receipt["kind"] == kind)
                    .count(),
                22
            );
        }
        assert!(
            !running.is_finished(),
            "busy secondary must stay blocked through the matrix"
        );
        assert!(fixture
            .process
            .release_resource(&owner, &busy)
            .unwrap_err()
            .to_string()
            .contains("resource_busy"));
        fixture.allow(entered["request"].as_u64().unwrap());
        running.await.unwrap().unwrap();
        let valid = fixture
            .call(&owner, "use", json!({"resource":primary,"second":busy}))
            .await
            .unwrap();
        assert_eq!(valid.structured_content.unwrap()["count"], 13);
        fixture.process.release_resource(&owner, &primary).unwrap();
        fixture.process.release_resource(&owner, &busy).unwrap();
        evidence(
            &fixture,
            "all-entrypoints",
            &lock_std_mutex(&reports),
            agent,
        );
        other.process.shutdown().await;
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r23_nested_frozen_schema_does_not_authorize_after_policy_revocation() {
    for rust in [false, true] {
        let fixture = Fixture::start(rust, true, 4).await;
        let session = Session::create(fixture.temp.path().join("session.jsonl")).unwrap();
        let owner = session.resource_owner_key();
        let primary = reference(&fixture.call(&owner, "create", json!({})).await.unwrap());
        let mut host = ExtensionHost::new();
        fixture.process.register_dynamic_tool_catalog(&mut host);
        let reports = Arc::new(StdMutex::new(Vec::new()));
        let hooks = Arc::new(AtomicUsize::new(0));
        let schema = fixture
            .process
            .tool_definitions()
            .into_iter()
            .find(|tool| tool.name == "use")
            .unwrap()
            .parameters;
        host.tool(ComposeProbe {
            owner: owner.clone(),
            primary: primary.clone(),
            expected_schema: schema,
            cases: vec![Refusal {
                label: "late-policy-revocation".into(),
                arguments: json!({"resource":primary,"second":primary}),
                code: Some("operation execution policy denied"),
            }],
            log: fixture.temp.path().join("calls.jsonl"),
            reports: reports.clone(),
            healthy_followup: false,
        });
        host.tool_call_hook(NestedHook {
            owner: owner.clone(),
            seen: hooks.clone(),
            revoke: Some(host.clone()),
        });
        host.finalize_tool_surface();
        select(&host, &owner, &primary);
        let before = fixture.calls();
        let agent = run_agent(&fixture, session, host.clone()).await;
        assert_eq!(hooks.load(Ordering::Acquire), 1);
        assert_eq!(fixture.calls(), before);
        assert_eq!(lock_std_mutex(&reports).len(), 1);
        assert!(!host
            .tool_snapshot()
            .1
            .iter()
            .any(|tool| tool.definition().name == "use"));
        assert_eq!(
            agent["receipts"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|receipt| receipt["kind"] == "call_finished" && receipt["ok"] == false)
                .count(),
            1
        );
        fixture.process.release_resource(&owner, &primary).unwrap();
        evidence(&fixture, "late-policy", &lock_std_mutex(&reports), agent);
        fixture.process.shutdown().await;
    }
}
