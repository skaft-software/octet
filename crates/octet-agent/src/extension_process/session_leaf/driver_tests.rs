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

const SCRIPT: &str = r#"#!/usr/bin/env python3
import json,sys,os,time,threading
pending={}
count=0
def send(v): print(json.dumps(v,separators=(',',':')),flush=True)
def reply(m,r): send({'jsonrpc':'2.0','id':m['id'],'result':r})
for line in sys.stdin:
 m=json.loads(line); method=m.get('method'); p=m.get('params',{})
 if method=='initialize':
  reply(m,{'api_version':'0.4','tools':[{'name':'compose','description':'Compose','parameters':{'type':'object'},'composition':{'mode':'on','inline_budget':3000}}],'commands':[],
   'protocol':{'version':'0.4','features':p['protocol']['required_features']+['session_entries','tool_composition_v1'],'limits':{'max_concurrent_requests':4}}})
 elif method=='hook/run':
  assert p['session_leaf']['expected_head']==p['context']['host']['session_leaf_id']
  assert len(p['context']['host']['session_entries'])==len(p['context']['host']['session_branch'])
  count+=1; cid='append:'+str(count); pending[cid]=('hook',m)
  send({'jsonrpc':'2.0','id':cid,'method':'session/append_entry','params':{'parent_request_id':m['id'],'entry_type':'driver-checkpoint','data':{'hook':p['hook'],'name':p['payload']['name'],'count':count},'session_leaf':{k:p['session_leaf'][k] for k in ['grant_id','activation_epoch','operation_id']}}})
 elif method=='tool/call':
  cid='nested:'+str(m['id']); pending[cid]=('tool',m)
  send({'jsonrpc':'2.0','id':cid,'method':'composition/call','params':{'parent_request_id':m['id'],'name':'tiny','arguments':{}}})
 elif method=='probe':
  def delayed(original=m):
   time.sleep(0.75); reply(original,{})
  threading.Thread(target=delayed,daemon=True).start()
 elif method=='shutdown': reply(m,{}); break
 elif method in ['context/updated','$/cancelRequest']: pass
 elif method is None:
  kind,original=pending.pop(m['id'])
  assert 'result' in m,m
  if kind=='hook': reply(original,{'disposition':{'action':'continue'}})
  else: reply(original,{'content':[{'type':'text','text':'nested complete'}],'is_error':False})
 else: raise AssertionError(m)
"#;

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
            .unwrap()
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
            .unwrap()
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
    assert_eq!(
        lock_std_mutex(&connection.session_leaf.mirror)
            .as_ref()
            .unwrap()
            .owner
            .session_id,
        root_owner
    );
    assert_eq!(lock_std_mutex(&connection.session_leaf.bound).len(), 2);
    drop(root_driver);
    drop(child_driver);
    assert!(lock_std_mutex(&connection.session_leaf.bound).is_empty());
    assert!(process.shutdown().await);
}
