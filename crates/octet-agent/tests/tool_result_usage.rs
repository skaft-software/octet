//! Per-tool-result usage accounting (row 1c.10 consumer half).
//!
//! Pi's `ToolResultMessage.usage` is billed tool work that is explicitly *not*
//! part of main LLM context accounting. The agent folds it into the run's
//! cumulative billed totals and never into the assistant turn's context usage.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use octet_agent::{
    Agent, AgentConfig, AgentEvent, EffectBroker, EffectPolicy, ExtensionHost, FinishReason,
    SandboxConfig, Session, Tool, ToolContext, ToolEffect, ToolError, ToolOutput,
};
use octet_ai::{
    AiClient, AiError, AssistantMessage, AssistantPart, Diagnostic, HostStreamModel,
    HostStreamTransport, Model, ModelCatalog, ModelId, Request, Response, ResponseStream,
    StopReason, StreamEvent, ToolCall, ToolCallId, ToolDef, Usage,
};

fn test_model() -> Model {
    ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap()
}

const TOOL_NAME: &str = "usage_probe";

struct UsageProbeTool {
    usage: Usage,
    terminate: bool,
}

#[async_trait::async_trait]
impl Tool for UsageProbeTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: TOOL_NAME.to_owned(),
            description: "reports provider usage for the tool call itself".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let output = ToolOutput::new("probe result").with_usage(self.usage);
        Ok(if self.terminate {
            output.requesting_termination()
        } else {
            output
        })
    }
}

/// One scripted canonical stream per provider call: a tool call, then an
/// answer, then more tool calls if the script has them.
struct ScriptedTurn {
    stop_reason: StopReason,
    content: Vec<AssistantPart>,
    usage: Usage,
}

struct ScriptedTransport {
    calls: AtomicUsize,
    turns: Vec<ScriptedTurn>,
}

fn response(model: &HostStreamModel, turn: &ScriptedTurn) -> Response {
    Response {
        message: AssistantMessage {
            content: turn.content.clone(),
            model: model.id.clone(),
            protocol: model.protocol,
        },
        stop_reason: turn.stop_reason.clone(),
        usage: turn.usage,
        cost: None,
        response_id: None,
        responses_output: None,
        deferred: None,
        diagnostics: Vec::new(),
    }
}

fn stream_for(model: &HostStreamModel, turn: &ScriptedTurn) -> ResponseStream {
    let mut events = vec![Ok(StreamEvent::Started { response_id: None })];
    match turn.stop_reason {
        StopReason::ToolUse => {
            for part in &turn.content {
                if let AssistantPart::ToolCall(call) = part {
                    events.push(Ok(StreamEvent::ToolCallStart {
                        index: 0,
                        id: call.id.clone(),
                        name: call.name.clone(),
                    }));
                    events.push(Ok(StreamEvent::ToolCallArgsDelta {
                        index: 0,
                        delta: call.arguments_json.clone(),
                    }));
                    // `ToolCallEnd` is emitted at the codec level, before the
                    // host stream normalizes and schema-checks the completed
                    // arguments, so `None` is the only honest codec value here:
                    // this fixture never claims a schema verdict it did not make.
                    events.push(Ok(StreamEvent::ToolCallEnd {
                        index: 0,
                        argument_error: None,
                    }));
                }
            }
        }
        _ => {
            events.push(Ok(StreamEvent::TextStart { index: 0 }));
            events.push(Ok(StreamEvent::TextDelta {
                index: 0,
                delta: "done".to_owned(),
            }));
            events.push(Ok(StreamEvent::TextEnd { index: 0 }));
        }
    }
    events.push(Ok(StreamEvent::Finished(response(model, turn))));
    Box::pin(futures_util::stream::iter(events))
}

#[async_trait::async_trait]
impl HostStreamTransport for ScriptedTransport {
    async fn stream(
        &self,
        model: HostStreamModel,
        _request: Request,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let turn = self
            .turns
            .get(call)
            .or_else(|| self.turns.last())
            .expect("at least one scripted turn");
        Ok(stream_for(&model, turn))
    }
}

fn tool_call_turn(call_id: &str, usage: Usage) -> ScriptedTurn {
    ScriptedTurn {
        stop_reason: StopReason::ToolUse,
        content: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId(call_id.to_owned()),
            name: TOOL_NAME.to_owned(),
            arguments_json: "{}".to_owned(),
            argument_error: None,
        })],
        usage,
    }
}

fn answer_turn(usage: Usage) -> ScriptedTurn {
    ScriptedTurn {
        stop_reason: StopReason::EndTurn,
        content: vec![AssistantPart::Text("done".to_owned())],
        usage,
    }
}

fn usage_agent(
    model: Model,
    transport: Arc<ScriptedTransport>,
    tool_terminates: bool,
    tool_usage: Usage,
) -> (Agent, tempfile::TempDir) {
    let client = AiClient::new();
    client.register_host_stream_transport(model.endpoint.id.clone(), transport);
    let workspace = tempfile::tempdir().unwrap();
    let mut extensions = ExtensionHost::new();
    extensions.tool(UsageProbeTool {
        usage: tool_usage,
        terminate: tool_terminates,
    });
    let agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(workspace.path().join("tool-usage.jsonl")).unwrap(),
        system: "system".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    (agent, workspace)
}

#[tokio::test]
async fn per_tool_result_usage_folds_into_run_totals_without_inflating_turn_context() {
    let model = test_model();
    let tool_usage = Usage {
        input_tokens: 100,
        output_tokens: 50,
        total_tokens: 150,
        ..Usage::default()
    };
    let first_provider_usage = Usage {
        input_tokens: 10,
        output_tokens: 5,
        total_tokens: 15,
        ..Usage::default()
    };
    let second_provider_usage = Usage {
        input_tokens: 20,
        output_tokens: 7,
        total_tokens: 27,
        ..Usage::default()
    };
    let transport = Arc::new(ScriptedTransport {
        calls: AtomicUsize::new(0),
        turns: vec![
            tool_call_turn("usage-call-1", first_provider_usage),
            answer_turn(second_provider_usage),
        ],
    });
    let (mut agent, _workspace) = usage_agent(model, transport.clone(), false, tool_usage);

    let mut run = agent.prompt("probe usage").await.unwrap();
    let mut turns = Vec::new();
    let mut tool_finished_usage = None;
    while let Some(event) = run.next().await {
        match event {
            AgentEvent::TurnFinished {
                turn_usage,
                usage,
                stop_reason,
                ..
            } => turns.push((stop_reason, turn_usage, usage)),
            AgentEvent::ToolFinished {
                result: Ok(output), ..
            } => tool_finished_usage = output.usage().copied(),
            _ => {}
        }
    }
    drop(run);

    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert_eq!(tool_finished_usage, Some(tool_usage));
    assert_eq!(turns.len(), 2, "one provider turn per request: {turns:?}");
    assert_eq!(turns[0].0, StopReason::ToolUse);
    assert_eq!(turns[0].1, first_provider_usage);
    assert_eq!(turns[0].2, first_provider_usage);
    // The tool's billed usage is in the run's cumulative totals...
    assert_eq!(
        turns[1].2.total_tokens,
        first_provider_usage.total_tokens
            + tool_usage.total_tokens
            + second_provider_usage.total_tokens
    );
    // ...but never in the assistant turn's context usage.
    assert_eq!(turns[1].0, StopReason::EndTurn);
    assert_eq!(turns[1].1, second_provider_usage);
    assert_ne!(turns[1].1, turns[1].2);
}

#[tokio::test]
async fn a_run_ending_on_a_tool_batch_still_reports_the_tool_usage() {
    let model = test_model();
    let tool_usage = Usage {
        input_tokens: 11,
        cache_read_tokens: 4,
        output_tokens: 5,
        total_tokens: 20,
        ..Usage::default()
    };
    let provider_usage = Usage {
        input_tokens: 3,
        output_tokens: 2,
        total_tokens: 5,
        ..Usage::default()
    };
    let transport = Arc::new(ScriptedTransport {
        calls: AtomicUsize::new(0),
        turns: vec![tool_call_turn("usage-call-2", provider_usage)],
    });
    let (mut agent, _workspace) = usage_agent(model, transport, true, tool_usage);

    let output = agent.complete("terminate via the tool").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    // The run ends on the tool batch, after the last `TurnFinished`, so the
    // settled output folds the trailing tool usage into the billed total.
    assert_eq!(
        output.usage.total_tokens,
        provider_usage.total_tokens + tool_usage.total_tokens
    );
    assert_eq!(output.usage.cache_read_tokens, tool_usage.cache_read_tokens);
}
