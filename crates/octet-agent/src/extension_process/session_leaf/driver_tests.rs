//! Real native Session/Agent/composition integration, with a protocol peer only.
use super::super::tests::{trusted_descriptor, write_executable_script};
use super::driver::*;
use super::*;
use crate::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig, Tool};
use octet_ai::{
    AssistantMessage, AssistantPart, CacheRetention, ModelCatalog, ModelId, ReasoningConfig,
    ReasoningMode, Request, Response, ToolCall, ToolCallId,
};
use serde_json::json;

// The routed peer reads both descriptors through parent-bound chunk handles.
const SCRIPT: &str = include_str!("driver_peer.py");

fn peer_error(temp: &tempfile::TempDir) -> String {
    std::fs::read_to_string(temp.path().join("peer-error.txt")).unwrap_or_default()
}

async fn process(temp: &tempfile::TempDir) -> ExtensionProcess {
    write_executable_script(&temp.path().join("extension.py"), SCRIPT);
    let manifest = ExtensionManifest::parse(
        r#"name = "driver-process"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
tools = ["compose"]
hooks = ["before_tool_call", "after_tool_call"]
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.tool_composition = true;
    config.supervise = false;
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .unwrap();
    // No incomplete wire is offered. Exercise native routed implementation by
    // installing the test-selected gates after the ordinary complete handshake.
    let connection = read_std_lock(&process.inner.connection).clone();
    let mut protocol = write_std_lock(&connection.protocol);
    protocol.features.insert("session_owner_routes_v1".into());
    protocol
        .features
        .insert("session_snapshot_transport_v1".into());
    drop(protocol);
    process
}

struct Tiny;
#[async_trait::async_trait]
impl Tool for Tiny {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "tiny".into(),
            description: "Tiny".into(),
            parameters: json!({"type":"object"}),
            constrained_sampling: None,
            async_execution: false,
        }
    }
    fn composition_is_unmetered(&self) -> bool {
        true
    }
    fn effect(
        &self,
        _: &serde_json::Value,
        _: &crate::ToolContext<'_>,
    ) -> Result<crate::ToolEffect, crate::ToolError> {
        Ok(crate::ToolEffect::Pure)
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: &crate::ToolContext<'_>,
    ) -> Result<crate::ToolOutput, crate::ToolError> {
        Ok(crate::ToolOutput::new("tiny success"))
    }
}

struct Scripted(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for Scripted {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        _: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, octet_ai::AiError> {
        let first = self.0.fetch_add(1, Ordering::AcqRel) == 0;
        let content = if first {
            vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId("outer".into()),
                name: "compose".into(),
                arguments_json: "{}".into(),
                async_execution: false,
                argument_error: None,
            })]
        } else {
            vec![AssistantPart::Text("done".into())]
        };
        let response = Response {
            message: AssistantMessage {
                content,
                model: model.id,
                protocol: model.protocol,
            },
            usage: Default::default(),
            cost: Some(Default::default()),
            stop_reason: if first {
                octet_ai::StopReason::ToolUse
            } else {
                octet_ai::StopReason::EndTurn
            },
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: None,
            diagnostics: Vec::new(),
        };
        use octet_ai::StreamEvent;
        let mut events = vec![Ok(StreamEvent::Started { response_id: None })];
        if first {
            events.extend([
                Ok(StreamEvent::ToolCallStart {
                    index: 0,
                    id: ToolCallId("outer".into()),
                    name: "compose".into(),
                    async_execution: false,
                }),
                Ok(StreamEvent::ToolCallArgsDelta {
                    index: 0,
                    delta: "{}".into(),
                }),
                Ok(StreamEvent::ToolCallEnd {
                    index: 0,
                    argument_error: None,
                }),
            ]);
        } else {
            events.extend([
                Ok(StreamEvent::TextStart { index: 0 }),
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    delta: "done".into(),
                }),
                Ok(StreamEvent::TextEnd { index: 0 }),
            ]);
        }
        events.push(Ok(StreamEvent::Finished(response)));
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
}

#[tokio::test]
async fn real_agent_legal_nested_composition_hook_appends_through_active_sole_driver() {
    let temp = tempfile::tempdir().unwrap();
    let process = process(&temp).await;
    let mut host = ExtensionHost::new();
    host.tool(Tiny);
    crate::extension::Extension::register(&process, &mut host);
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let client = octet_ai::AiClient::new();
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(Scripted(AtomicUsize::new(0))),
    );
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session: crate::Session::create(temp.path().join("session.jsonl")).unwrap(),
        extensions: host,
        system: "system".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::new(crate::effect::EffectPolicy::UnsafeHost),
        max_turns: Some(3),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("driver-test".into()),
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), agent.complete("run"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.text, "done");
    let persisted = std::fs::read_to_string(agent.session().path()).unwrap();
    assert_eq!(persisted.matches("driver-checkpoint").count(), 4);
    assert!(persisted.contains("before_tool_call"));
    assert!(persisted.contains("after_tool_call"));
    assert!(persisted.contains("nested complete"));
    let connection = read_std_lock(&process.inner.connection).clone();
    assert!(lock_std_mutex(&connection.session_leaf.bound).is_empty());
    assert!(lock_std_mutex(&connection.session_leaf.execution_slots).is_empty());
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn native_driver_route_wrong_owner_refuses_before_activation() {
    let temp = tempfile::tempdir().unwrap();
    let process = process(&temp).await;
    let (_, route) = SessionDriver::new("real-owner".into());
    assert!(route.prepare(&process, "foreign-owner").await.is_err());
    let connection = read_std_lock(&process.inner.connection).clone();
    assert!(lock_std_mutex(&connection.session_leaf.bound).is_empty());
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn cancelled_noncooperative_callbacks_keep_capacity_until_late_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let process = process(&temp).await;
    let connection = read_std_lock(&process.inner.connection).clone();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let connection = Arc::clone(&connection);
        handles.push(tokio::spawn(async move {
            connection
                .request("probe", json!({}), Duration::from_secs(5))
                .await
        }));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let written = {
                let pending = lock_std_mutex(&connection.pending);
                pending.len() == 4
                    && pending
                        .values()
                        .all(|request| request.frame_state.load(Ordering::Acquire) == FRAME_WRITTEN)
            };
            if written {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for handle in handles {
        handle.abort();
        let _ = handle.await;
    }
    assert_eq!(
        lock_std_mutex(&connection.session_leaf.execution_slots).len(),
        4
    );
    assert_eq!(read_std_lock(&connection.slots).available_permits(), 0);
    let refusal = connection
        .request("probe", json!({}), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(refusal
        .to_string()
        .contains("session_request_quota_exceeded"));
    let owner = ExtensionResourceOwner {
        session_id: "owner".into(),
        extension_instance_id: process.inner.instance_id.clone(),
        process_generation: connection.generation,
    };
    let refusal = connection
        .request_lifecycle("hook/run", json!({}), Duration::from_secs(5), owner)
        .await
        .unwrap_err();
    assert!(refusal
        .to_string()
        .contains("session_request_quota_exceeded"));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if lock_std_mutex(&connection.session_leaf.execution_slots).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(read_std_lock(&connection.slots).available_permits(), 4);
    assert!(!connection.closed.load(Ordering::Acquire));
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn two_native_owner_drivers_share_process_without_foreground_promotion() {
    let temp = tempfile::tempdir().unwrap();
    let process = process(&temp).await;
    let mut root = crate::Session::create(temp.path().join("root.jsonl")).unwrap();
    let mut child = crate::Session::create(temp.path().join("child.jsonl")).unwrap();
    let root_owner = root.resource_owner_key();
    let child_owner = child.resource_owner_key();
    process
        .set_host_state_with_session(ExtensionHostState::default(), &root)
        .unwrap();
    let (mut root_driver, root_route) = SessionDriver::new(root_owner.clone());
    let (mut child_driver, child_route) = SessionDriver::new(child_owner.clone());
    let root_work = async {
        let invocation = root_route.prepare(&process, &root_owner).await.unwrap();
        invocation
            .run_hook(
                ExtensionHook::BeforeToolCall,
                json!({"name":"root","arguments":{}}),
                process.current_context_for_resource_owner(root_owner.clone()),
            )
            .await
            .unwrap_or_else(|error| panic!("root hook: {error}\npeer: {}", peer_error(&temp)))
    };
    let child_work = async {
        let invocation = child_route.prepare(&process, &child_owner).await.unwrap();
        invocation
            .run_hook(
                ExtensionHook::BeforeToolCall,
                json!({"name":"child","arguments":{}}),
                process.current_context_for_resource_owner(child_owner.clone()),
            )
            .await
            .unwrap_or_else(|error| panic!("child hook: {error}\npeer: {}", peer_error(&temp)))
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        let cancellation = CancellationToken::default();
        let (root_result, child_result) = tokio::join!(
            root_driver.drive(root_work, &mut root, &cancellation),
            child_driver.drive(child_work, &mut child, &cancellation)
        );
        root_result.unwrap();
        child_result.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(root.entries().len(), 1);
    assert_eq!(child.entries().len(), 1);
    let root_text = std::fs::read_to_string(root.path()).unwrap();
    let child_text = std::fs::read_to_string(child.path()).unwrap();
    assert!(root_text.contains("\"name\":\"root\""));
    assert!(!root_text.contains("\"name\":\"child\""));
    assert!(child_text.contains("\"name\":\"child\""));
    assert!(!child_text.contains("\"name\":\"root\""));
    let connection = read_std_lock(&process.inner.connection).clone();
    // The routed profile keeps its current view in the transport, not the
    // legacy mirror, and only for the owner it was published for.
    let owner = process
        .current_context_for_resource_owner(root_owner.clone())
        .resource_owner
        .unwrap();
    {
        let store = lock_std_mutex(&connection.session_leaf.transport);
        // The current view is the immutable publication (taken before the hook
        // append); the append advanced the activation view and its session.
        assert_eq!(
            store.current.len(),
            1,
            "only the published owner has a current view"
        );
        let view = store.current.get(&owner).and_then(Option::as_ref).unwrap();
        assert_eq!(view.descriptor.owner, owner);
        assert!(view.descriptor.head.is_none());
        assert_eq!(view.descriptor.entry_count, 0);
    }
    assert_eq!(lock_std_mutex(&connection.session_leaf.bound).len(), 2);
    drop(root_driver);
    drop(child_driver);
    assert!(lock_std_mutex(&connection.session_leaf.bound).is_empty());
    assert!(process.shutdown().await);
}
