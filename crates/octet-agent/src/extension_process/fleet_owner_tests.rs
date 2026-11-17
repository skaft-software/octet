//! Real native fleet + shared Pi adapter, using an offline scripted provider.
use super::*;
use crate::{
    Agent, AgentCompactionMode, AgentConfig, CoreTools, EffectBroker, ExtensionHost, SandboxConfig,
    Session,
};
use octet_ai::{
    AssistantMessage, AssistantPart, CacheRetention, Cost, HostStreamModel, Message, ModelCatalog,
    ModelId, ReasoningConfig, ReasoningMode, Request, Response, StopReason, StreamEvent, ToolCall,
    ToolCallId, UserPart,
};
use serde_json::{json, Value};

struct FleetProvider {
    turns: StdMutex<BTreeMap<String, usize>>,
    requests: Arc<StdMutex<Vec<Request>>>,
}
fn call(id: &str, name: &str, args: Value) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: ToolCallId(id.into()),
        name: name.into(),
        arguments_json: args.to_string(),
        async_execution: false,
        argument_error: None,
    })
}
fn ordinary(prefix: &str) -> Vec<AssistantPart> {
    vec![
        call(
            &format!("{prefix}-read"),
            "read",
            json!({"path":"proof.txt"}),
        ),
        call(
            &format!("{prefix}-bash"),
            "bash",
            json!({"command":"printf fleet-proof"}),
        ),
    ]
}
fn lead_tools(prefix: &str) -> Vec<AssistantPart> {
    let mut calls = ordinary(prefix);
    calls.push(call(
        &format!("{prefix}-status"),
        "subagent_status",
        json!({}),
    ));
    for i in 0..3 {
        calls.push(call(
            &format!("{prefix}-wait-{i}"),
            "subagent_wait",
            json!({"target":format!("reader-{i}"), "timeout_seconds":10}),
        ));
    }
    calls
}
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for FleetProvider {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, octet_ai::AiError> {
        let root = request
            .tools
            .iter()
            .any(|tool| tool.name == "subagent_spawn");
        let compacting = request.tools.is_empty();
        let key = request.session_id.clone().unwrap();
        let content = if compacting {
            vec![AssistantPart::Text(
                "Fleet reports remain available after compaction.".into(),
            )]
        } else {
            let mut turns = self.turns.lock().unwrap();
            let turn = turns.entry(key.clone()).or_default();
            let content = match (root, *turn) {
                (true, 0) => (0..3).map(|i| call(&format!("spawn-{i}"), "subagent_spawn", json!({
                    "name":format!("reader-{i}"), "task":format!("Read proof.txt and run printf; report worker-{i}"),
                    "tools":["read","bash"], "idempotency_key":format!("worker-{i}"),
                }))).collect(),
                (true, 1) => lead_tools("lead"),
                (true, 3) => lead_tools("resumed"),
                (false, 0) => ordinary(&key),
                (true, _) => vec![AssistantPart::Text("lead done".into())],
                (false, _) => vec![AssistantPart::Text(format!("worker report {key}"))],
            };
            *turn += 1;
            content
        };
        let stop_reason = if content
            .iter()
            .any(|part| matches!(part, AssistantPart::ToolCall(_)))
        {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };
        self.requests.lock().unwrap().push(request);
        let mut events = vec![Ok(StreamEvent::Started { response_id: None })];
        for (index, part) in content.iter().enumerate() {
            match part {
                AssistantPart::Text(text) => events.extend([
                    Ok(StreamEvent::TextStart { index }),
                    Ok(StreamEvent::TextDelta {
                        index,
                        delta: text.clone(),
                    }),
                    Ok(StreamEvent::TextEnd { index }),
                ]),
                AssistantPart::ToolCall(call) => events.extend([
                    Ok(StreamEvent::ToolCallStart {
                        index,
                        id: call.id.clone(),
                        name: call.name.clone(),
                        async_execution: false,
                    }),
                    Ok(StreamEvent::ToolCallArgsDelta {
                        index,
                        delta: call.arguments_json.clone(),
                    }),
                    Ok(StreamEvent::ToolCallEnd {
                        index,
                        argument_error: None,
                    }),
                ]),
                _ => unreachable!(),
            }
        }
        events.push(Ok(StreamEvent::Finished(Response {
            message: AssistantMessage {
                model: model.id,
                protocol: model.protocol,
                content,
            },
            stop_reason,
            usage: Default::default(),
            cost: Some(Cost::default()),
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: None,
            diagnostics: Vec::new(),
        })));
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_pi_permission_hooks_preserve_child_and_lead_owners_across_compaction_and_reload() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::write(root.join("proof.txt"), "fleet-proof").unwrap();
    let factory = root.join("permission.mjs");
    std::fs::write(&factory, r#"import fs from 'node:fs';
export default pi => {
  pi.on('session_start', () => {});
  pi.on('tool_call', (event, ctx) => {
    try {
      const session = ctx.sessionManager.getSessionId();
      const entries = ctx.sessionManager.getEntries();
      pi.getThinkingLevel();
      if (!session || !Array.isArray(entries)) throw new Error('missing session');
      fs.appendFileSync('permission-calls.jsonl', JSON.stringify({session, call:event.toolCallId, tool:event.toolName}) + '\n');
    } catch (error) {
      return {block:true, reason:`Permission gate failed and blocked the tool call (fail-closed): ${error.message}`};
    }
  });
};"#).unwrap();
    let manifest_path = super::pi_fixture::capture(root, &[factory]).await;
    let mut config = ExtensionRuntimeConfig::new(root);
    config.supervise = false;
    config.host_state.has_ui = Some(false);
    config.host_state.reasoning = Some("off".into());
    let pi = ExtensionProcess::start(
        super::tests::trusted_descriptor(
            manifest_path.parent().unwrap(),
            ExtensionManifest::load(&manifest_path).unwrap(),
        ),
        config,
    )
    .await
    .unwrap();
    let subagent_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-subagents/extension.toml")
        .canonicalize()
        .unwrap();
    let mut manifest = ExtensionManifest::load(&subagent_manifest).unwrap();
    manifest.entrypoint.command = subagent_manifest
        .parent()
        .unwrap()
        .join("octet-subagents")
        .to_string_lossy()
        .into_owned();
    let mut config = ExtensionRuntimeConfig::new(root);
    config.supervise = false;
    config.agent_sessions = true;
    let subagents = ExtensionProcess::start(
        super::tests::trusted_descriptor(subagent_manifest.parent().unwrap(), manifest),
        config,
    )
    .await
    .unwrap();
    let mut host = ExtensionHost::new();
    host.load(&CoreTools);
    pi.register(&mut host);
    subagents.register(&mut host);
    let mut model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).limits.context_window = 20_000;
    Arc::make_mut(&mut model.spec).limits.max_output_tokens = 1024;
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let client = octet_ai::AiClient::new();
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(FleetProvider {
            turns: StdMutex::new(BTreeMap::new()),
            requests: requests.clone(),
        }),
    );
    let mut sandbox = SandboxConfig::new(root);
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(root.join("lead.jsonl")).unwrap(),
        extensions: host,
        system: "fleet test".into(),
        sandbox,
        effect_broker: EffectBroker::new(crate::EffectPolicy::UnsafeHost),
        max_turns: Some(5),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("lead".into()),
    })
    .unwrap();
    agent
        .set_compaction_token_mode(AgentCompactionMode::Disabled, 0.8, 1)
        .unwrap();
    let owner = agent.session().resource_owner_key();
    pi.set_host_state_with_session(pi.current_context().host, agent.session())
        .unwrap();
    pi.start_session_hook_binding(owner.clone()).await.unwrap();
    agent
        .enable_v2_delegation_extension_only(crate::DelegationConfig::new(root.join("children")))
        .unwrap();
    agent.bind_extension_agent_sessions(&subagents).unwrap();
    let first = tokio::time::timeout(Duration::from_secs(30), agent.complete("launch fleet"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.text, "lead done");
    assert_successful_tools(&requests.lock().unwrap());
    // Compaction retains the same durable owner. Restarting only the adapter
    // retains the native child service and reports, but needs a fresh binding.
    agent
        .session_mut()
        .append(crate::session::EntryValue::Message(Message::User(
            octet_ai::UserMessage {
                content: vec![UserPart::Text("older fleet context ".repeat(2500))],
            },
        )))
        .unwrap();
    agent
        .session_mut()
        .append(crate::session::EntryValue::Message(Message::User(
            octet_ai::UserMessage {
                content: vec![UserPart::Text("recent tail".into())],
            },
        )))
        .unwrap();
    agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.5, 1)
        .unwrap();
    pi.reload().await.unwrap();
    let second = tokio::time::timeout(
        Duration::from_secs(30),
        agent.complete("retrieve fleet reports"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(second.text, "lead done");
    assert_eq!(agent.session().resource_owner_key(), owner);
    assert!(agent
        .session()
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, crate::session::EntryValue::Compaction { .. })));
    let service = read_std_lock(&subagents.inner.delegation_service)
        .clone()
        .unwrap();
    let waited = service
        .wait(
            &owner,
            Duration::from_secs(1),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(waited["timed_out"], false);
    for child in waited["snapshot"]["agents"].as_array().unwrap() {
        assert_eq!(child["status"]["state"], "completed", "{child}");
        assert!(child.to_string().contains("worker report"), "{child}");
    }
    {
        let requests = requests.lock().unwrap();
        assert_successful_tools(&requests);
        for wait_id in [
            "lead-wait-0",
            "lead-wait-1",
            "lead-wait-2",
            "resumed-wait-0",
            "resumed-wait-1",
            "resumed-wait-2",
        ] {
            let result = requests
                .iter()
                .flat_map(|r| &r.messages)
                .filter_map(|m| match m {
                    Message::User(u) => Some(&u.content),
                    _ => None,
                })
                .flatten()
                .find_map(|p| match p {
                    UserPart::ToolResult(r) if r.tool_call_id.0 == wait_id => Some(r),
                    _ => None,
                })
                .unwrap();
            let text = serde_json::to_string(&result.content).unwrap();
            assert!(text.contains("worker report"), "{wait_id}: {text}");
        }
    }
    let audit = std::fs::read_to_string(root.join("permission-calls.jsonl")).unwrap();
    let rows: Vec<Value> = audit
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 21, "{audit}"); // 3 spawns, 6 child tools, 12 lead tools.
    let mut by_session = BTreeMap::<String, usize>::new();
    let mut ids = BTreeSet::new();
    for row in rows {
        assert!(
            ids.insert(row["call"].as_str().unwrap().to_owned()),
            "duplicate hook {row}"
        );
        *by_session
            .entry(row["session"].as_str().unwrap().to_owned())
            .or_default() += 1;
    }
    assert_eq!(by_session.len(), 4);
    assert_eq!(by_session.values().filter(|count| **count == 2).count(), 3);
    assert_eq!(by_session.values().filter(|count| **count == 15).count(), 1);
    assert!(pi.shutdown().await);
    assert!(subagents.shutdown().await);
}

fn assert_successful_tools(requests: &[Request]) {
    for request in requests {
        for message in &request.messages {
            if let Message::User(user) = message {
                for part in &user.content {
                    if let UserPart::ToolResult(result) = part {
                        assert!(!result.is_error, "tool failed: {result:?}");
                    }
                }
            }
        }
    }
}
