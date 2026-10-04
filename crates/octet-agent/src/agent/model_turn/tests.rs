//! Source-only scripted-provider regressions: no network, no provider credentials.
use super::*;
use crate::compaction::{
    SessionOperationDecision, SessionOperationFuture, SessionOperationInvocation,
};
use crate::extension_process::ExtensionResourceOwner;
use crate::session::Entry;
use crate::session_leaf::{SessionLeafBinding, SessionLeafConsumer};
use std::sync::atomic::AtomicUsize;
use tokio::sync::Notify;

type Trace = Arc<Mutex<Vec<String>>>;

#[derive(Clone)]
enum Behavior {
    Continue,
    VetoStart,
    VetoEnd,
    FailEnd,
    HoldEnd(Arc<Notify>),
}

struct Observe {
    seen: Arc<Mutex<Vec<SessionOperation>>>,
    trace: Trace,
    behavior: Behavior,
    append: bool,
}
struct Invocation {
    future: Option<SessionOperationFuture>,
    leaf: Option<(SessionLeafConsumer, SessionLeafBinding)>,
}
impl SessionOperationInvocation for Invocation {
    fn take_future(&mut self) -> SessionOperationFuture {
        self.future.take().unwrap()
    }
    fn ready(&self) -> Pin<Box<dyn std::future::Future<Output = bool> + Send + 'static>> {
        match &self.leaf {
            Some((consumer, _)) => Box::pin(consumer.ready()),
            None => Box::pin(std::future::pending()),
        }
    }
    fn consume_next(&mut self, session: &mut Session) -> Result<(), String> {
        let (consumer, binding) = self.leaf.as_mut().unwrap();
        assert!(consumer
            .consume_next(session, binding)
            .map_err(|e| e.to_string())?);
        Ok(())
    }
}
impl SessionOperationHook for Observe {
    fn begin(
        &self,
        session: &Session,
        operation: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        let (is_end, index) = match operation {
            SessionOperation::ModelTurnStart { turn_index, .. } => (false, *turn_index),
            SessionOperation::ModelTurnEnd {
                turn_index,
                assistant_entry,
                tool_result_entries,
                ..
            } => {
                // The event contains exact durable records, not provisional
                // stream messages or recomputed projections.
                for entry in std::iter::once(assistant_entry).chain(tool_result_entries.iter()) {
                    assert_eq!(
                        serde_json::to_value(entry).unwrap(),
                        serde_json::to_value(session.entry(&entry.id).unwrap()).unwrap()
                    );
                }
                (true, *turn_index)
            }
            _ => return Ok(None),
        };
        self.seen.lock().unwrap().push(operation.clone());
        let name = format!("{}:{index}", if is_end { "end" } else { "start" });
        let binding = SessionLeafBinding {
            activation_epoch: index * 2 + u64::from(is_end) + 1,
            owner: ExtensionResourceOwner {
                session_id: session.resource_owner_key(),
                extension_instance_id: "model-turn-test".into(),
                process_generation: 1,
            },
            namespace: "model.turn.test".into(),
            operation_id: name.clone(),
        };
        let (leaf, producer) = if self.append {
            let (consumer, producer, grant) =
                SessionLeafConsumer::new(session, binding.clone()).unwrap();
            (Some((consumer, binding)), Some((producer, grant)))
        } else {
            (None, None)
        };
        let behavior = self.behavior.clone();
        let trace = self.trace.clone();
        let future = Box::pin(async move {
            if let Some((producer, grant)) = producer {
                let receipt = producer
                    .try_append(
                        grant.id(),
                        grant.binding(),
                        "observation".into(),
                        serde_json::json!({"boundary":name}),
                    )
                    .unwrap();
                // A reply here requires the Agent to service the private lane
                // on its existing mutable writer while this callback waits.
                let committed = receipt.wait().await.unwrap();
                assert_eq!(committed.entry_id, committed.head);
            }
            trace.lock().unwrap().push(name);
            match behavior {
                Behavior::VetoStart if !is_end => Ok(SessionOperationDecision::Cancel),
                Behavior::VetoEnd if is_end => Ok(SessionOperationDecision::Cancel),
                Behavior::FailEnd if is_end => Err("private payload must not escape".into()),
                Behavior::HoldEnd(entered) if is_end => {
                    entered.notify_one();
                    std::future::pending().await
                }
                _ => Ok(SessionOperationDecision::Continue),
            }
        });
        Ok(Some(Box::new(Invocation {
            future: Some(future),
            leaf,
        })))
    }
}

struct Script {
    turns: Mutex<VecDeque<Vec<AssistantPart>>>,
    calls: AtomicUsize,
    trace: Trace,
    release_on_second: Option<Arc<Notify>>,
}
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for Script {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.trace.lock().unwrap().push(format!("provider:{index}"));
        if index == 1 {
            if let Some(release) = &self.release_on_second {
                assert!(!request.messages.iter().any(|message| matches!(message,
                    Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(_)))
                )), "pending async result must not enter the concurrent request");
                release.notify_one();
            }
        }
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

struct Probe {
    trace: Trace,
    fast_finished: Arc<Notify>,
    held: Arc<Notify>,
}
#[async_trait::async_trait]
impl Tool for Probe {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "probe".into(),
            description: "Local observation".into(),
            parameters: serde_json::json!({"type":"object","properties":{"label":{"type":"string"}},"required":["label"],"additionalProperties":false}),
            constrained_sampling: None,
            async_execution: false,
        }
    }
    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        _: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let label = args["label"].as_str().unwrap();
        if label == "slow" {
            self.fast_finished.notified().await;
        }
        if label == "held" {
            self.held.notified().await;
        }
        self.trace.lock().unwrap().push(format!("tool:{label}"));
        if label == "fast" {
            self.fast_finished.notify_one();
        }
        Ok(ToolOutput::new(format!("result:{label}")))
    }
}
fn call(label: &str, async_execution: bool) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: octet_ai::ToolCallId(format!("call-{label}")),
        name: "probe".into(),
        arguments_json: serde_json::json!({"label":label}).to_string(),
        argument_error: None,
        async_execution,
    })
}
fn text(value: &str) -> Vec<AssistantPart> {
    vec![AssistantPart::Text(value.into())]
}

struct Fixture {
    agent: Agent,
    script: Arc<Script>,
    seen: Arc<Mutex<Vec<SessionOperation>>>,
    trace: Trace,
    _dir: tempfile::TempDir,
}
fn fixture(
    turns: Vec<Vec<AssistantPart>>,
    behavior: Behavior,
    append: bool,
    asynchronous: bool,
) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let held = Arc::new(Notify::new());
    let script = Arc::new(Script {
        turns: Mutex::new(turns.into()),
        calls: AtomicUsize::new(0),
        trace: trace.clone(),
        release_on_second: asynchronous.then(|| held.clone()),
    });
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId(
            if asynchronous {
                "gpt-5.4-mini-responses"
            } else {
                "gpt-4o-mini"
            }
            .into(),
        ))
        .unwrap();
    if asynchronous {
        let features = octet_ai::ResponsesFeatures {
            async_tools: true,
            ..Default::default()
        };
        Arc::make_mut(&mut model.spec)
            .capabilities
            .responses_features = features;
        Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_features = features;
    }
    let client = AiClient::new();
    client.register_host_stream_transport(model.endpoint.id.clone(), script.clone());
    let mut host = ExtensionHost::new();
    host.tool(Probe {
        trace: trace.clone(),
        fast_finished: Arc::new(Notify::new()),
        held,
    });
    host.session_operation_hook(Observe {
        seen: seen.clone(),
        trace: trace.clone(),
        behavior,
        append,
    });
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        extensions: host,
        session: Session::create(dir.path().join("session.jsonl")).unwrap(),
        system: "test".into(),
        sandbox: SandboxConfig::new(dir.path()),
        effect_broker: EffectBroker::default(),
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
    agent.set_parallel_read_wave_width(2);
    Fixture {
        agent,
        script,
        seen,
        trace,
        _dir: dir,
    }
}
fn result_ids(entries: &[Entry]) -> Vec<&str> {
    entries
        .iter()
        .flat_map(|entry| match &entry.value {
            EntryValue::Message(Message::User(user)) => user
                .content
                .iter()
                .filter_map(|part| match part {
                    UserPart::ToolResult(result) => Some(result.tool_call_id.0.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

#[tokio::test]
async fn actual_iterations_await_append_receipts_and_end_after_durable_parallel_results() {
    let mut f = fixture(
        vec![vec![call("slow", false), call("fast", false)], text("done")],
        Behavior::Continue,
        true,
        false,
    );
    let before = now_unix_millis();
    let mut run = f.agent.prompt("observe").await.unwrap();
    let mut turn_finished = 0;
    let mut run_finished = 0;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::TurnFinished { .. } => {
                    turn_finished += 1;
                    if turn_finished == 1 {
                        assert_eq!(*f.trace.lock().unwrap(), ["start:0", "provider:0"]);
                    }
                }
                AgentEvent::RunFinished { reason, .. } => {
                    assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
                    run_finished += 1;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    drop(run);
    assert_eq!((turn_finished, run_finished), (2, 1));
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        *f.trace.lock().unwrap(),
        [
            "start:0",
            "provider:0",
            "tool:fast",
            "tool:slow",
            "end:0",
            "start:1",
            "provider:1",
            "end:1"
        ]
    );
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    let expected_run = format!("run:{}", f.agent.session.entries()[0].id.0);
    for operation in seen.iter() {
        let (run_id, timestamp_ms) = match operation {
            SessionOperation::ModelTurnStart {
                run_id,
                timestamp_ms,
                ..
            }
            | SessionOperation::ModelTurnEnd {
                run_id,
                timestamp_ms,
                ..
            } => (run_id, *timestamp_ms),
            _ => panic!("unexpected operation"),
        };
        assert_eq!(run_id, &expected_run);
        assert!((before..=now_unix_millis()).contains(&timestamp_ms));
    }
    let SessionOperation::ModelTurnEnd {
        turn_index,
        tool_result_entries,
        ..
    } = &seen[1]
    else {
        panic!("missing end");
    };
    assert_eq!(*turn_index, 0);
    // Completion order was fast/slow, persistence is emitted slow/fast order.
    assert_eq!(result_ids(tool_result_entries), ["call-slow", "call-fast"]);
    assert!(
        matches!(&seen[3], SessionOperation::ModelTurnEnd { turn_index: 1, tool_result_entries, .. } if tool_result_entries.is_empty())
    );
    let path = f.agent.session.path().to_owned();
    let private_ids: Vec<_> = f
        .agent
        .session
        .entries()
        .iter()
        .filter(|entry| {
            f.agent
                .session
                .extension_entry(&entry.id, "model.turn.test")
                .is_some()
        })
        .map(|entry| entry.id.clone())
        .collect();
    assert_eq!(private_ids.len(), 4);
    drop(seen);
    drop(f.agent);
    let reopened = Session::open(&path).unwrap();
    for id in private_ids {
        assert!(reopened.extension_entry(&id, "model.turn.test").is_some());
    }
}

#[tokio::test]
async fn async_tool_end_waits_for_original_results_without_blocking_next_model_iteration() {
    let mut f = fixture(
        vec![vec![call("held", true)], text("independent"), text("done")],
        Behavior::Continue,
        false,
        true,
    );
    let output = tokio::time::timeout(Duration::from_secs(5), f.agent.complete("observe"))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 3);
    let trace = f.trace.lock().unwrap();
    let pos = |name| trace.iter().position(|item| item == name).unwrap();
    assert!(pos("start:1") < pos("end:0"));
    assert!(pos("provider:1") < pos("tool:held"));
    assert!(pos("tool:held") < pos("end:0"));
    assert!(pos("end:0") < pos("end:1"));
    assert!(pos("end:1") < pos("start:2"));
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 6);
    let end = seen
        .iter()
        .find(|op| matches!(op, SessionOperation::ModelTurnEnd { turn_index: 0, .. }))
        .unwrap();
    let SessionOperation::ModelTurnEnd {
        tool_result_entries,
        ..
    } = end
    else {
        unreachable!()
    };
    assert_eq!(result_ids(tool_result_entries), ["call-held"]);
}

#[tokio::test]
async fn reentering_request_preparation_does_not_repeat_the_logical_start() {
    let mut f = fixture(vec![text("unused")], Behavior::Continue, false, false);
    let hooks = f.agent.extensions.session_operation_hooks.clone();
    let mut turns = ModelTurnHooks::new(&hooks, "run:local");
    let cancellation = CancellationToken::default();
    turns
        .start(&mut f.agent.session, 0, &cancellation)
        .await
        .unwrap();
    turns
        .start(&mut f.agent.session, 0, &cancellation)
        .await
        .unwrap();
    assert_eq!(*f.trace.lock().unwrap(), ["start:0"]);
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn start_is_continue_only_and_failure_never_calls_provider() {
    let mut f = fixture(
        vec![text("must not dispatch")],
        Behavior::VetoStart,
        false,
        false,
    );
    assert!(f.agent.complete("observe").await.is_err());
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn end_veto_or_failure_retains_all_durable_tools_without_retry_or_next_model_call() {
    for behavior in [Behavior::VetoEnd, Behavior::FailEnd] {
        let mut f = fixture(
            vec![vec![call("one", false)], text("must not dispatch")],
            behavior,
            true,
            false,
        );
        let error = f.agent.complete("observe").await.unwrap_err();
        assert!(error.to_string().contains("durable entries retained"));
        assert!(!error.to_string().contains("private payload"));
        assert_eq!(f.script.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *f.trace.lock().unwrap(),
            ["start:0", "provider:0", "tool:one", "end:0"]
        );
        assert_eq!(f.seen.lock().unwrap().len(), 2);
        assert_eq!(result_ids(f.agent.session.entries()), ["call-one"]);
    }
}

#[tokio::test]
async fn cancellation_while_end_callback_waits_retains_assistant_tools_and_acknowledged_append() {
    let entered = Arc::new(Notify::new());
    let mut f = fixture(
        vec![vec![call("one", false)], text("must not dispatch")],
        Behavior::HoldEnd(entered.clone()),
        true,
        false,
    );
    let mut run = f.agent.prompt("observe").await.unwrap();
    let control = run.control();
    let drive = async {
        let mut terminals = 0;
        while let Some(event) = run.next().await {
            if let AgentEvent::RunFinished { reason, .. } = event {
                assert!(matches!(reason, FinishReason::Aborted), "{reason:?}");
                terminals += 1;
            }
        }
        assert_eq!(terminals, 1);
    };
    let cancel = async {
        entered.notified().await;
        control.abort();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(drive, cancel);
    })
    .await
    .unwrap();
    drop(run);
    assert_eq!(f.script.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.seen.lock().unwrap().len(), 2);
    assert_eq!(result_ids(f.agent.session.entries()), ["call-one"]);
    assert_eq!(
        f.agent
            .session
            .entries()
            .iter()
            .filter(|entry| f
                .agent
                .session
                .extension_entry(&entry.id, "model.turn.test")
                .is_some())
            .count(),
        2
    );
}
