//! Channel-gated waits prove receipts commit before the held work is released.
use super::*;
use crate::extension_process::{
    DiscoveredExtension, ExtensionActivation, ExtensionEvent, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSessionEntryOperation, ExtensionSource, ExtensionTrust,
};
use futures_util::FutureExt;
use std::os::unix::fs::PermissionsExt;
use tokio::sync::Semaphore;

fn write_executable_script(path: &std::path::Path, source: &str) {
    std::fs::write(path, source).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn trusted_descriptor(
    directory: &std::path::Path,
    manifest: ExtensionManifest,
) -> DiscoveredExtension {
    DiscoveredExtension {
        manifest,
        manifest_path: directory.join("extension.toml"),
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    }
}

const APPENDER: &str = r#"#!/usr/bin/env python3
import json,sys
def send(v): print(json.dumps(v),flush=True)
def reply(m,r): send({'jsonrpc':'2.0','id':m['id'],'result':r})
init=json.loads(sys.stdin.readline()); offer=init['params']['protocol']
features=offer['required_features']+['session_entries','session_owner_routes_v1','session_snapshot_transport_v1','session_append_receipts_v1']
reply(init,{'api_version':'0.4','tools':[],'commands':[{'name':'append','description':'Append'}],
 'protocol':{'version':'0.4','features':features,'limits':{'max_concurrent_requests':4,'max_message_bytes':offer['limits']['max_message_bytes']},'session_snapshot_transport_v1':offer['session_snapshot_transport_v1']}})
command=None
for line in sys.stdin:
 m=json.loads(line); method=m.get('method'); p=m.get('params',{})
 if method=='session/snapshot/prepare':
  d=p['snapshot']; reply(m,{'accepted':True,'transfer_id':d['transfer_id'],'view_revision':d['view_revision'],'head':d['head'],'sha256':d['sha256']})
 elif method=='command/execute':
  command=m
  send({'jsonrpc':'2.0','id':'append','method':'session/append_entry','params':{'parent_request_id':m['id'],'entry_type':'held-wait','data':{}}})
 elif m.get('id')=='append': reply(command,{'text':json.dumps(m)})
 elif method=='shutdown': reply(m,{}); break
"#;

async fn appender(dir: &std::path::Path, session: &Session) -> ExtensionProcess {
    write_executable_script(&dir.join("extension.py"), APPENDER);
    let manifest = ExtensionManifest::parse(
        r#"name = "wait-appender"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
commands = ["append"]
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(dir);
    config.supervise = false;
    let process = ExtensionProcess::start(trusted_descriptor(dir, manifest), config)
        .await
        .unwrap();
    process
        .set_host_state_with_session(Default::default(), session)
        .unwrap();
    let owner = process
        .current_context_for_resource_owner(session.resource_owner_key())
        .resource_owner
        .unwrap();
    process
        .wait_for_session_snapshot_prepared(
            &owner,
            Duration::from_secs(5),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    process
}

struct HeldRead {
    started: mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
}

#[async_trait::async_trait]
impl Tool for HeldRead {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "held_read".into(),
            description: "Held read".into(),
            parameters: serde_json::json!({"type":"object"}),
            async_execution: false,
            constrained_sampling: None,
        }
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }
    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::WorkspaceRead)
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.started.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
        Ok(ToolOutput::new("read complete"))
    }
}

struct WaitTransport {
    background: bool,
    video_read: bool,
    initial_video: bool,
    second: mpsc::UnboundedSender<()>,
}

#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for WaitTransport {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        let continuing = self.initial_video
            || request
                .messages
                .iter()
                .any(|message| matches!(message, Message::Assistant(_)));
        let content = if continuing {
            self.second.send(()).unwrap();
            vec![AssistantPart::Text("done".into())]
        } else {
            (0..if self.video_read { 1 } else { 2 })
                .map(|i| {
                    AssistantPart::ToolCall(ToolCall {
                        id: octet_ai::ToolCallId(format!("read-{i}")),
                        name: if self.video_read { "read" } else { "held_read" }.into(),
                        arguments_json: if self.video_read {
                            r#"{"path":"clip.mp4"}"#
                        } else {
                            "{}"
                        }
                        .into(),
                        argument_error: None,
                        async_execution: self.background,
                    })
                })
                .collect()
        };
        // Native Responses output must retain the same calls as the canonical message.
        let output = content.iter().map(|part| match part {
            AssistantPart::ToolCall(call) => serde_json::json!({"type":"function_call","call_id":call.id.0,"name":call.name,"arguments":call.arguments_json}),
            _ => serde_json::json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}),
        }).map(|v| serde_json::from_value(v).unwrap()).collect();
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content,
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: if continuing {
                    StopReason::EndTurn
                } else {
                    StopReason::ToolUse
                },
                usage: Usage::default(),
                cost: None,
                response_id: None,
                responses_output: (model.protocol == Protocol::OpenAiResponses)
                    .then(|| octet_ai::ResponsesOutput::new(output)),
                deferred: None,
                inference: None,
                diagnostics: vec![],
            })),
        ])))
    }
}

fn agent(
    dir: &std::path::Path,
    session: Session,
    host: ExtensionHost,
    background: bool,
    video_read: bool,
    initial_video: bool,
    second: mpsc::UnboundedSender<()>,
) -> Agent {
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId(
            if background {
                "gpt-6-astra"
            } else {
                "gpt-4o-mini"
            }
            .into(),
        ))
        .unwrap();
    Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
    let client = AiClient::new();
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(WaitTransport {
            background,
            video_read,
            initial_video,
            second,
        }),
    );
    Agent::new(AgentConfig {
        client,
        model,
        session,
        system: "test".into(),
        sandbox: SandboxConfig::new(dir),
        effect_broker: EffectBroker::default(),
        extensions: host,
        max_turns: Some(if background { 2 } else { 1 }),
        reasoning: if background {
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low)
        } else {
            ReasoningConfig::Off
        },
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap()
}

async fn commit_while_held(
    run: &mut Run<'_>,
    process: &ExtensionProcess,
    owner: String,
    path: &std::path::Path,
) {
    let mut events = process.subscribe();
    let command = tokio::spawn({
        let process = process.clone();
        async move {
            process
                .execute_command(
                    "append",
                    vec![],
                    process.current_context_for_resource_owner(owner),
                )
                .await
        }
    });
    let (id, generation, entry_type, data) = loop {
        if let ExtensionEvent::SessionEntryRequested {
            request_id,
            generation,
            operation: ExtensionSessionEntryOperation::Append { entry_type, data },
            ..
        } = events.recv().await.unwrap()
        {
            break (request_id, generation, entry_type, data);
        }
    };
    run.queue_extension_append(process.clone(), id, generation, entry_type, data)
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::pin!(command);
        loop {
            tokio::select! {
                output = &mut command => break output.unwrap().unwrap(),
                event = run.next() => {
                    assert!(event.is_some(), "run ended before held work was released");
                    assert!(!matches!(event, Some(AgentEvent::ToolFinished { .. })), "held tool completed before release");
                }
            }
        }
    }).await.expect("append receipt must commit before the held wait finishes");
    let receipt: serde_json::Value = serde_json::from_str(&output.text).unwrap();
    assert!(receipt.get("result").is_some(), "{receipt}");
    let reopened = Session::open_read_only(path).unwrap();
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter_map(|e| reopened.extension_entry(&e.id, "wait-appender"))
            .filter(|e| e.entry_type == "held-wait")
            .count(),
        1
    );
}

async fn held_tools(background: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let session = Session::create(&path).unwrap();
    let owner = session.resource_owner_key();
    let (started_tx, mut started) = mpsc::unbounded_channel();
    let (second_tx, mut second) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let mut host = ExtensionHost::new();
    host.tool(HeldRead {
        started: started_tx,
        release: release.clone(),
    });
    let mut agent = agent(
        dir.path(),
        session,
        host,
        background,
        false,
        false,
        second_tx,
    );
    // Publish after the native reasoning baseline, which precedes Run admission.
    if background {
        persist_reasoning_selection(&mut agent.session, &agent.model, &agent.reasoning).unwrap();
    }
    let process = appender(dir.path(), agent.session()).await;
    let mut run = agent.prompt("read").await.unwrap();
    for _ in 0..2 {
        loop {
            tokio::select! {
                signal = started.recv() => { signal.unwrap(); break; },
                event = run.next() => assert!(event.is_some()),
            }
        }
    }
    if background {
        loop {
            tokio::select! {
                signal = second.recv() => { signal.unwrap(); break; },
                event = run.next() => assert!(event.is_some()),
            }
        }
    }
    // Poll through remaining ready events into the actual held wave/settlement.
    while let Some(event) = run.next().now_or_never() {
        assert!(event.is_some());
    }
    commit_while_held(&mut run, &process, owner, &path).await;
    release.add_permits(2);
    while let Some(event) = run.next().await {
        assert!(
            !matches!(
                event,
                AgentEvent::RunFinished {
                    reason: FinishReason::Failed(_),
                    ..
                }
            ),
            "{event:?}"
        );
    }
    drop(run);
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn admitted_append_commits_during_parallel_wave() {
    held_tools(false).await;
}

#[tokio::test]
async fn admitted_append_commits_during_background_settlement() {
    held_tools(true).await;
}

// Decoder PATH is private to a child test process. A socket handshake, rather
// than elapsed time, proves the real video preparation future is suspended.
fn video_wait(post_tool: bool) {
    const CHILD: &str = "OCTET_APPEND_VIDEO_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        write_executable_script(&dir.path().join("ffprobe"), "#!/usr/bin/python3\nimport os,socket\ns=socket.create_connection(('127.0.0.1',int(os.environ['OCTET_APPEND_VIDEO_PORT'])))\ns.sendall(b'R')\ns.recv(1)\nprint('{\"streams\":[{\"width\":16,\"height\":16}],\"format\":{\"duration\":\"2\"}}')\n");
        write_executable_script(&dir.path().join("ffmpeg"), "#!/bin/sh\nexit 1\n");
        let name = if post_tool {
            "admitted_append_commits_during_post_tool_video"
        } else {
            "admitted_append_commits_during_initial_video"
        };
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("agent::turn_loop::append_wait_tests::{name}"),
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    dir.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            std::env::set_var(
                "OCTET_APPEND_VIDEO_PORT",
                listener.local_addr().unwrap().port().to_string(),
            );
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("session.jsonl");
            let mut session = Session::create(&path).unwrap();
            let bytes =
                bytes::Bytes::from_static(include_bytes!("../../../octet-ai/src/video/tiny.mp4"));
            std::fs::write(dir.path().join("clip.mp4"), &bytes).unwrap();
            if !post_tool {
                session
                    .append(EntryValue::Message(Message::User(UserMessage {
                        content: vec![UserPart::Media(Media::video_bytes(
                            bytes,
                            "video/mp4".parse().unwrap(),
                        ))],
                    })))
                    .unwrap();
            }
            let owner = session.resource_owner_key();
            let process = appender(dir.path(), &session).await;
            let (second, _rx) = mpsc::unbounded_channel();
            let mut host = ExtensionHost::new();
            host.load(&crate::tools::CoreTools);
            let mut agent = agent(dir.path(), session, host, false, true, !post_tool, second);
            let mut run = agent.prompt("describe video").await.unwrap();
            let mut socket = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        connection = listener.accept() => break connection.unwrap().0,
                        event = run.next() => assert!(event.is_some(), "{event:?}"),
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(socket.read_u8().await.unwrap(), b'R');
            commit_while_held(&mut run, &process, owner, &path).await;
            socket.write_all(b"G").await.unwrap();
            while let Some(event) = run.next().await {
                assert!(
                    !matches!(
                        event,
                        AgentEvent::RunFinished {
                            reason: FinishReason::Failed(_),
                            ..
                        }
                    ),
                    "{event:?}"
                );
            }
            drop(run);
            assert!(process.shutdown().await);
        });
}

#[test]
fn admitted_append_commits_during_initial_video() {
    video_wait(false);
}

#[test]
fn admitted_append_commits_during_post_tool_video() {
    video_wait(true);
}
