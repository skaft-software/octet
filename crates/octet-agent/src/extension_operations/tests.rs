use super::*;
use crate::extension::{ExtensionHost, ToolCallHook};
use crate::extension_process::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
};
use crate::{Agent, AgentConfig, EffectBroker, EffectPolicy, SandboxConfig, Session};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "acceptance.rs"]
mod acceptance;

async fn fixture() -> (tempfile::TempDir, ExtensionProcess, ExtensionHost) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("fixture.py"),
        include_str!("fixture.py"),
    )
    .unwrap();
    let mut manifest = ExtensionManifest::parse(
        r#"
name = "operation-discovery-test"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = ["fixture.py"]
[contributes]
tools = []
"#,
    )
    .unwrap();
    manifest.entrypoint.env.insert(
        "HOME".into(),
        directory.path().to_string_lossy().into_owned(),
    );
    let descriptor = DiscoveredExtension {
        manifest,
        manifest_path: directory.path().join("extension.toml"),
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let mut config = ExtensionRuntimeConfig::new(directory.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(5);
    let process = ExtensionProcess::start(descriptor, config).await.unwrap();
    let mut host = ExtensionHost::new();
    host.load(&process);
    host.finalize_tool_surface();
    (directory, process, host)
}

async fn create(process: &ExtensionProcess, owner: &str) -> ResourceRef {
    let output = process
        .call_tool(
            "create",
            json!({}),
            process.current_context_for_resource_owner(owner.to_owned()),
        )
        .await
        .unwrap();
    serde_json::from_value(output.structured_content.unwrap()["circuit"].clone()).unwrap()
}

fn lookup(
    resource: &ResourceRef,
    limit: Option<usize>,
    cursor: Option<String>,
) -> ApplicableOperationsRequest {
    ApplicableOperationsRequest {
        resource: resource.clone(),
        limit,
        cursor,
    }
}

fn calls(directory: &tempfile::TempDir) -> Vec<Value> {
    std::fs::read_to_string(directory.path().join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn d01_d02_d03_d08_d09_d10_d12_real_process_lookup() {
    let (directory, process, mut host) = fixture().await;
    let resource = create(&process, "owner").await;
    let initial = host.model_tool_definitions("owner");
    assert_eq!(
        initial.len(),
        4,
        "only creation/plain/replace plus host discovery"
    );
    assert_eq!(
        host.tool_definitions().len(),
        104,
        "registry remains complete"
    );
    let first = host
        .applicable_operations("owner", lookup(&resource, None, None))
        .unwrap();
    assert_eq!(first.operations.len(), 8);
    assert!(first.next_cursor.is_some());
    assert_eq!(
        (&*first.operations[0].id, &*first.operations[0].path),
        ("circuit.op000", "/circuit")
    );
    assert!(first.operations[0].primary_receiver);
    assert_eq!(first.operations[1].path, "/secondary");
    assert!(!first.operations[1].primary_receiver);
    assert!(
        !first.operations[2].primary_receiver,
        "receiver is optional metadata"
    );
    assert!(first.operations.iter().all(|op| op.catalog_revision == 0));
    let second = host
        .applicable_operations("owner", lookup(&resource, None, first.next_cursor.clone()))
        .unwrap();
    assert_eq!(second.operations[0].id, "circuit.op007");
    let all = host
        .applicable_operations("owner", lookup(&resource, Some(32), None))
        .unwrap();
    assert_eq!(
        all.operations.len(),
        21,
        "20 exact operations, one has two matching slots"
    );
    assert!(all.next_cursor.is_none());
    assert!(
        all.operations
            .iter()
            .all(|op| op.id.as_str() < "circuit.op020"),
        "no Circuit2 prefix matching"
    );
    let again = host
        .applicable_operations("owner", lookup(&resource, None, None))
        .unwrap();
    assert_eq!(
        again.operations, first.operations,
        "baseline ordering is deterministic"
    );
    let continued = host
        .applicable_operations("owner", lookup(&resource, None, first.next_cursor.clone()))
        .unwrap();
    assert_eq!(
        continued.operations, second.operations,
        "other projections do not change the cursor's catalog/policy epoch"
    );
    host.applicable_operations("owner", lookup(&resource, None, None))
        .unwrap();
    assert_eq!(
        host.model_tool_definitions("owner").len(),
        11,
        "8 cards select only 7 unique exact schemas"
    );
    assert_eq!(
        host.model_tool_definitions("foreign").len(),
        4,
        "no projection across owners"
    );
    assert!(host
        .applicable_operations("foreign", lookup(&resource, None, None))
        .is_err());
    let fabricated = ResourceRef {
        resource: "unknown".into(),
        resource_type: "Circuit".into(),
    };
    assert!(host
        .applicable_operations("owner", lookup(&fabricated, None, None))
        .is_err());
    for invalid_limit in [0, 33] {
        assert!(host
            .applicable_operations("owner", lookup(&resource, Some(invalid_limit), None))
            .is_err());
    }
    assert_eq!(
        calls(&directory).len(),
        1,
        "lookup/projection invoke zero domain handlers"
    );

    // Discovery/receiver matching does not satisfy or authorize another slot.
    assert!(process
        .call_tool(
            "op_000",
            json!({"circuit":resource,"secondary":fabricated}),
            process.current_context_for_resource_owner("owner")
        )
        .await
        .is_err());
    assert_eq!(
        calls(&directory).len(),
        1,
        "invalid secondary reference never enters target"
    );
    let valid = process
        .call_tool(
            "op_000",
            json!({"circuit":resource,"secondary":resource}),
            process.current_context_for_resource_owner("owner"),
        )
        .await
        .unwrap();
    assert!(
        valid.content.contains("count 1"),
        "duplicate slots use one exclusive pin"
    );

    // Active-tool narrowing remains ordinary registry policy, not lazy loading.
    let active = host
        .policed_tool_names()
        .into_iter()
        .filter(|name| name != "op_000")
        .collect();
    host.set_active_tools(Some(&active)).unwrap();
    let error = host
        .applicable_operations("owner", lookup(&resource, None, again.next_cursor))
        .unwrap_err();
    assert!(error.to_string().contains("catalog_changed"));
    let filtered = host
        .applicable_operations("owner", lookup(&resource, None, None))
        .unwrap();
    assert_eq!(filtered.operations[0].id, "circuit.op001");
    host.set_active_tools(None).unwrap();
    let old = host
        .applicable_operations("owner", lookup(&resource, None, None))
        .unwrap();
    process
        .call_tool(
            "replace_catalog",
            json!({}),
            process.current_context_for_resource_owner("owner"),
        )
        .await
        .unwrap();
    assert!(host
        .applicable_operations("owner", lookup(&resource, None, old.next_cursor))
        .unwrap_err()
        .to_string()
        .contains("catalog_changed"));
    let updated = host
        .applicable_operations("owner", lookup(&resource, None, None))
        .unwrap();
    assert!(updated.operations.iter().all(|op| op.catalog_revision == 1));
    host.set_tool_policy(|name| !name.starts_with("op_"));
    assert!(host
        .applicable_operations("owner", lookup(&resource, None, updated.next_cursor))
        .unwrap_err()
        .to_string()
        .contains("catalog_changed"));
    assert_eq!(host.model_tool_definitions("owner").len(), 4);
    assert!(
        host.applicable_operations("owner", lookup(&resource, None, None))
            .unwrap()
            .operations
            .is_empty(),
        "valid resource with hidden catalog is not unavailable"
    );
    process.release_resource("owner", &resource).unwrap();
    assert!(host
        .applicable_operations("owner", lookup(&resource, None, None))
        .is_err());
    assert!(process.shutdown().await);
    println!("D lookup child log: {}", json!(calls(&directory)));
}

// This hook is a deterministic barrier AFTER the provider captured rev N and
// BEFORE ordinary dispatch. No sleeps, paid inference or synthetic RPC host.
struct ChangeBeforeDispatch {
    process: ExtensionProcess,
    host: ExtensionHost,
    revoke: bool,
    changed: AtomicBool,
}
#[async_trait::async_trait]
impl ToolCallHook for ChangeBeforeDispatch {
    async fn before_tool_call(
        &self,
        name: &str,
        _: &Value,
        ctx: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        if name == "op_000" && !self.changed.swap(true, Ordering::AcqRel) {
            if self.revoke {
                let mut host = self.host.clone();
                host.set_tool_policy(|name| name != "op_000");
            } else {
                self.process
                    .call_tool(
                        "replace_catalog",
                        json!({}),
                        self.process
                            .current_context_for_resource_owner(ctx.resource_owner.to_owned()),
                    )
                    .await
                    .map_err(|error| ToolError::new(error.to_string()))?;
            }
        }
        Ok(())
    }
    async fn after_tool_call(&self, _: &str, _: &Value, _: &str, _: bool, _: &ToolContext<'_>) {}
}

fn frame(event: &str, value: Value) -> String {
    format!("event: {event}\ndata: {value}\n\n")
}
fn scripted_turn(call: Option<(&str, Value)>) -> ResponseTemplate {
    static NEXT_CALL_ID: AtomicUsize = AtomicUsize::new(0);
    let call_id = NEXT_CALL_ID.fetch_add(1, Ordering::Relaxed);
    let mut body = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"local","usage":{"input_tokens":5,"output_tokens":0}}}),
    );
    let stop = if let Some((name, args)) = call {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":format!("call-{name}-{call_id}"),"name":name}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":args.to_string()}}),
        );
        "tool_use"
    } else {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}),
        );
        "end_turn"
    };
    body += &frame(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    );
    body += &frame(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":3}}),
    );
    body += &frame("message_stop", json!({"type":"message_stop"}));
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

#[tokio::test]
async fn d04_d05_d06_d08_actual_agent_frozen_projection_and_revocation() {
    for revoke in [false, true] {
        let (directory, process, mut host) = fixture().await;
        let session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let resource = create(&process, &session.resource_owner_key()).await;
        let originals = process.tool_definitions();
        let original = &originals
            .iter()
            .find(|tool| tool.name == "op_000")
            .unwrap()
            .parameters;
        host.tool_call_hook(ChangeBeforeDispatch {
            process: process.clone(),
            host: host.clone(),
            revoke,
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
                    _ => panic!("unexpected provider turn"),
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
        Arc::make_mut(&mut model.endpoint).auth =
            octet_ai::Auth::bearer("local-scripted-no-inference");
        Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
        let mut agent = Agent::new(AgentConfig {
            client: octet_ai::AiClient::new(),
            model,
            session,
            system: "Local conformance probe".into(),
            sandbox: SandboxConfig::new(directory.path()),
            effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
            extensions: host,
            max_turns: Some(4),
            reasoning: octet_ai::ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
            cache_retention: octet_ai::CacheRetention::Short,
            session_id: None,
        })
        .unwrap();
        assert_eq!(agent.registered_tool_names().len(), 104);
        let output = agent
            .complete("discover and use the circuit")
            .await
            .unwrap();
        assert_eq!(output.text, "done");
        assert_eq!(turn.load(Ordering::SeqCst), 3);
        let requests = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.body_json::<Value>().unwrap())
            .collect::<Vec<_>>();
        let tools = |index: usize| requests[index]["tools"].as_array().unwrap();
        assert_eq!(tools(0).len(), 4);
        assert!(tools(0)
            .iter()
            .all(|tool| !tool["name"].as_str().unwrap().starts_with("op_")));
        assert_eq!(
            tools(1).len(),
            11,
            "bounded lazy selection, not 100 operation schemas"
        );
        assert_eq!(
            tools(1)
                .iter()
                .find(|tool| tool["name"] == "op_000")
                .unwrap()["input_schema"],
            *original
        );
        for projected in tools(1) {
            if projected["name"] == DISCOVERY_TOOL_NAME {
                continue;
            }
            let exact = originals
                .iter()
                .find(|tool| projected["name"] == tool.name)
                .unwrap();
            assert_eq!(projected["input_schema"], exact.parameters);
        }
        assert!(tools(0).iter().any(|tool| tool["name"] == "plain"));
        let log = calls(&directory);
        let invoked = log
            .iter()
            .filter(|entry| entry["name"] == "op_000")
            .collect::<Vec<_>>();
        if revoke {
            assert!(
                invoked.is_empty(),
                "lookup/old schema does not authorize execution after policy revocation"
            );
            assert!(!tools(2).iter().any(|tool| tool["name"] == "op_000"));
        } else {
            assert_eq!(invoked.len(), 1);
            assert_eq!(
                invoked[0]["revision"], 0,
                "old definition/handler retained for in-flight turn"
            );
            let next = &tools(2)
                .iter()
                .find(|tool| tool["name"] == "op_000")
                .unwrap()["input_schema"];
            assert_eq!(
                next["properties"]["revision_marker"]["const"], 1,
                "next turn sees replacement schema"
            );
            assert!(requests[2].to_string().contains("handler revision 0"));
        }
        assert!(process.shutdown().await);
        println!(
            "D Agent revoke={revoke}; tool counts={:?}; child log={}",
            [tools(0).len(), tools(1).len(), tools(2).len()],
            json!(log)
        );
        if let Ok(root) = std::env::var("OCTET_DISCOVERY_EVIDENCE_DIR") {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                std::path::Path::new(&root).join(format!("agent-revoke-{revoke}.json")),
                serde_json::to_vec_pretty(&json!({"requests":requests,"child_log":log})).unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn cursor_binds_owner_reference_generation_and_revision() {
    let owner = ExtensionResourceOwner {
        session_id: "owner".into(),
        extension_instance_id: "instance".into(),
        process_generation: 1,
    };
    let resource = ResourceRef {
        resource: "token".into(),
        resource_type: "Circuit".into(),
    };
    let binding = cursor_binding(&owner, &resource, 4);
    let cursor = format!("{binding}:8");
    assert_eq!(cursor_offset(Some(&cursor), &binding).unwrap(), 8);
    for changed in [
        cursor_binding(&owner, &resource, 5),
        cursor_binding(
            &ExtensionResourceOwner {
                process_generation: 2,
                ..owner.clone()
            },
            &resource,
            4,
        ),
        cursor_binding(
            &ExtensionResourceOwner {
                session_id: "other".into(),
                ..owner
            },
            &resource,
            4,
        ),
        cursor_binding(
            &ExtensionResourceOwner {
                session_id: "owner".into(),
                extension_instance_id: "instance".into(),
                process_generation: 1,
            },
            &ResourceRef {
                resource: "other".into(),
                ..resource
            },
            4,
        ),
    ] {
        assert!(cursor_offset(Some(&cursor), &changed).is_err());
    }
}
