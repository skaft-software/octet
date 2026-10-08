//! Scripted-provider regressions for overlapping process calls of one response.
use super::*;
use crate::effect::EffectPolicy;
use std::sync::atomic::AtomicUsize;
use tokio::sync::Notify;

struct Script {
    turns: Mutex<VecDeque<Vec<AssistantPart>>>,
}
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for Script {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        _: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        let content = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .expect("no extra model calls");
        let stop_reason = if content
            .iter()
            .any(|part| matches!(part, AssistantPart::ToolCall(_)))
        {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content,
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason,
                usage: Usage::default(),
                cost: Some(Cost::default()),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

#[derive(Default)]
struct Gates {
    fast_finished: Notify,
    release_held: Notify,
    running: AtomicUsize,
    max_running: AtomicUsize,
    finished: Mutex<Vec<String>>,
}

/// A process-backed tool shaped like `bash`: one self-contained child per call.
struct Process {
    name: &'static str,
    gates: Arc<Gates>,
}
#[async_trait::async_trait]
impl Tool for Process {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: self.name.into(),
            description: "Run one command".into(),
            parameters: serde_json::json!({"type":"object","properties":{"command":{"type":"string"},"label":{"type":"string"}},"required":["command","label"],"additionalProperties":false}),
            constrained_sampling: None,
            async_execution: false,
        }
    }
    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::HostProcess)
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelProcess
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let label = args["label"].as_str().unwrap().to_owned();
        let running = self.gates.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.gates.max_running.fetch_max(running, Ordering::SeqCst);
        ctx.progress
            .output(OutputStream::Stdout, format!("live:{label}\n").into_bytes());
        match label.as_str() {
            "slow" => self.gates.fast_finished.notified().await,
            "held" => self.gates.release_held.notified().await,
            "fast" => {}
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
        self.gates.finished.lock().unwrap().push(label.clone());
        if label == "fast" {
            self.gates.fast_finished.notify_one();
        }
        self.gates.running.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::new(format!("result:{label}")))
    }
}

fn call(tool: &str, label: &str) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: octet_ai::ToolCallId(format!("call-{label}")),
        name: tool.into(),
        arguments_json: serde_json::json!({"command":"pwd","label":label}).to_string(),
        argument_error: None,
        async_execution: false,
    })
}

struct Fixture {
    agent: Agent,
    gates: Arc<Gates>,
    _dir: tempfile::TempDir,
}

fn fixture(tool: &'static str, labels: &[&str], policy: EffectPolicy) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let gates = Arc::new(Gates::default());
    let script = Arc::new(Script {
        turns: Mutex::new(
            [
                labels.iter().map(|label| call(tool, label)).collect(),
                vec![AssistantPart::Text("done".into())],
            ]
            .into(),
        ),
    });
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let client = AiClient::new();
    client.register_host_stream_transport(model.endpoint.id.clone(), script);
    let mut host = ExtensionHost::new();
    host.tool(Process {
        name: tool,
        gates: gates.clone(),
    });
    let mut sandbox = SandboxConfig::new(dir.path());
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        extensions: host,
        session: Session::create(dir.path().join("session.jsonl")).unwrap(),
        system: "test".into(),
        sandbox,
        effect_broker: EffectBroker::new(policy),
        max_turns: Some(5),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    agent
        .set_compaction_token_mode(AgentCompactionMode::Disabled, 0.9, 2)
        .unwrap();
    agent.set_parallel_read_wave_width(4);
    Fixture {
        agent,
        gates,
        _dir: dir,
    }
}

fn result_ids(entries: &[crate::session::Entry]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|entry| match &entry.value {
            EntryValue::Message(Message::User(user)) => user
                .content
                .iter()
                .filter_map(|part| match part {
                    UserPart::ToolResult(result) => Some(result.tool_call_id.0.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

#[tokio::test]
async fn process_calls_of_one_response_overlap_and_commit_in_emitted_order() {
    // `slow` can only finish after `fast`: one-at-a-time execution would hang.
    let mut f = fixture("proc", &["slow", "fast"], EffectPolicy::UnsafeHost);
    let mut run = f.agent.prompt("run both").await.unwrap();
    let mut started = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::ToolStarted { id, .. } => started.push(id.0),
                AgentEvent::RunFinished { reason, .. } => {
                    assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
                }
                _ => {}
            }
        }
    })
    .await
    .expect("independent process calls of one response overlap");
    drop(run);
    assert_eq!(started, ["call-slow", "call-fast"]);
    assert_eq!(*f.gates.finished.lock().unwrap(), ["fast", "slow"]);
    assert_eq!(f.gates.max_running.load(Ordering::SeqCst), 2);
    assert_eq!(
        result_ids(f.agent.session.entries()),
        ["call-slow", "call-fast"],
        "results are committed in emitted order, not completion order"
    );
}

#[tokio::test]
async fn overlapped_process_output_streams_while_the_call_runs() {
    // `held` finishes only once the test has seen its live output, so output
    // drained after the wave instead of streamed during it would hang.
    let mut f = fixture("proc", &["held", "fast"], EffectPolicy::UnsafeHost);
    let mut run = f.agent.prompt("run both").await.unwrap();
    let gates = f.gates.clone();
    let mut live_before_finish = false;
    let mut finished = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::ToolProgress {
                    id,
                    progress: ToolProgress::Output { bytes, .. },
                } if id.0 == "call-held" => {
                    assert_eq!(&bytes[..], b"live:held\n");
                    live_before_finish = finished.is_empty();
                    gates.release_held.notify_one();
                }
                AgentEvent::ToolFinished { id, .. } => finished.push(id.0),
                _ => {}
            }
        }
    })
    .await
    .expect("overlapped output reaches observers while the call runs");
    drop(run);
    assert!(live_before_finish);
    assert_eq!(finished, ["call-held", "call-fast"]);
}

#[tokio::test]
async fn process_calls_stay_one_at_a_time_when_the_broker_can_prompt() {
    // Controlled mode admits a known-safe `bash` command without a prompt, but
    // any process call there could need approval, so calls stay ordered.
    let mut f = fixture(
        "bash",
        &["first", "second", "third"],
        EffectPolicy::Controlled,
    );
    let mut run = f.agent.prompt("run all").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            if let AgentEvent::RunFinished { reason, .. } = event {
                assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
            }
        }
    })
    .await
    .unwrap();
    drop(run);
    assert_eq!(f.gates.max_running.load(Ordering::SeqCst), 1);
    assert_eq!(
        *f.gates.finished.lock().unwrap(),
        ["first", "second", "third"]
    );
}

async fn run_to_completion(f: &mut Fixture) {
    let mut run = f.agent.prompt("run all").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            if let AgentEvent::RunFinished { reason, .. } = event {
                assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn process_calls_are_bound_by_the_turn_limit_not_the_read_width() {
    // Processes wait on their own children, so like Pi a wide batch runs at
    // once; the read width still bounds read observations.
    let labels = ["a", "b", "c", "d", "e", "f"];
    let mut f = fixture("proc", &labels, EffectPolicy::UnsafeHost);
    f.agent.set_parallel_read_wave_width(3);
    run_to_completion(&mut f).await;
    assert_eq!(f.gates.max_running.load(Ordering::SeqCst), labels.len());
    assert_eq!(
        result_ids(f.agent.session.entries()),
        ["call-a", "call-b", "call-c", "call-d", "call-e", "call-f"]
    );
}

#[tokio::test]
async fn a_wave_width_of_one_runs_process_calls_in_turn() {
    let mut f = fixture("proc", &["a", "b", "c"], EffectPolicy::UnsafeHost);
    f.agent.set_parallel_read_wave_width(1);
    run_to_completion(&mut f).await;
    assert_eq!(f.gates.max_running.load(Ordering::SeqCst), 1);
    assert_eq!(*f.gates.finished.lock().unwrap(), ["a", "b", "c"]);
}

#[test]
fn waves_bound_observations_by_the_read_width_and_stop_at_barriers() {
    use CallOverlap::{Observation as R, Process as P};
    let end = |members: &[Option<CallOverlap>], start: usize, width: usize| {
        parallel_wave_end(start, width, |index| members[index], members.len())
    };
    let reads = [Some(R); 6];
    assert_eq!(end(&reads, 0, 4), 4);
    assert_eq!(end(&reads, 4, 4), 6);
    let processes = [Some(P); 6];
    assert_eq!(end(&processes, 0, 2), 6);
    assert_eq!(end(&processes, 0, 1), 1);
    // Processes do not use up the read width; a barrier ends the wave.
    let mixed = [Some(P), Some(R), Some(P), Some(R), Some(R), None, Some(P)];
    assert_eq!(end(&mixed, 0, 2), 4);
    assert_eq!(end(&mixed, 0, 3), 5);
    assert_eq!(end(&mixed, 6, 3), 7);
    assert_eq!(end(&[Some(R), Some(P)], 0, 1), 1);
}
