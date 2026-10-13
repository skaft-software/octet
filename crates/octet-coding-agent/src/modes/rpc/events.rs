//! The streaming translation from `AgentEvent` to the Pi event vocabulary.
//!
//! Why this is separate: the RPC command loop is a single-owner task, and the
//! hard part of it is not the loop but the state machine that decides which of
//! the mutually exclusive assistant-message, turn and tool events are legal at
//! any moment. Keeping that state machine - partial text, open turn, expected
//! tool count, retry bracket - in one module means the ordering rules can be
//! read and tested without the command dispatch that drives it, and it is the
//! only place that knows about the bounded live-progress prefix.

use std::collections::HashMap;

use octet_agent::{AgentEvent, OutputChannel, ToolProgress, UserInput};
use octet_ai::{AssistantMessage, AssistantPart, Model, Usage};
use serde_json::{json, Value};

use super::projection::{
    assistant_value, now_millis, protocol_name, rpc_messages, usage_value, user_value,
};
use super::wire::RpcOutput;
use super::{QueueState, QueuedInput};
use crate::app::App;
use crate::modes::HostRunOutcome;

// Live progress is a display projection, not the authoritative tool result.
// Keep a bounded UTF-8 prefix plus an explicit display-byte omission count.
pub(super) const MAX_RPC_TOOL_PROGRESS_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct RpcToolProgress {
    pub(super) text: String,
    pub(super) omitted: u64,
}

impl RpcToolProgress {
    pub(super) fn push_str(&mut self, text: &str) {
        let mut keep = MAX_RPC_TOOL_PROGRESS_BYTES
            .saturating_sub(self.text.len())
            .min(text.len());
        while !text.is_char_boundary(keep) {
            keep -= 1;
        }
        // Once truncated, keep a contiguous prefix (never append later bytes
        // into spare space left by a split multibyte character).
        if self.omitted != 0 {
            keep = 0;
        }
        self.text.push_str(&text[..keep]);
        self.omitted = self.omitted.saturating_add((text.len() - keep) as u64);
    }

    fn separator(&mut self) {
        if !self.text.is_empty() || self.omitted != 0 {
            self.push_str("\n");
        }
    }

    pub(super) fn snapshot(&self) -> String {
        if self.omitted == 0 {
            self.text.clone()
        } else {
            format!(
                "{}\n[{} UTF-8 display bytes omitted from live progress]",
                self.text, self.omitted
            )
        }
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static PARTIAL_SNAPSHOTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) struct EventTranslator {
    pub(super) endpoint: String,
    pub(super) model: Model,
    pub(super) api: String,
    pub(super) partial_text: String,
    pub(super) partial_reasoning: String,
    pub(super) channels: Vec<OutputChannel>,
    pub(super) message_started: bool,
    pub(super) message_timestamp: u128,
    pub(super) turn_open: bool,
    pub(super) pending_turn: Option<Value>,
    pub(super) pending_tool_results: Vec<Value>,
    pub(super) expected_tools: usize,
    pub(super) tools: HashMap<String, (String, Value, RpcToolProgress)>,
    pub(super) messages: Vec<Value>,
    pub(super) run_messages: Vec<Value>,
    pub(super) last_assistant_text: String,
    pub(super) retry_attempt: Option<usize>,
    pub(super) pending_retry_end: Option<Value>,
    pub(super) usage_uncertain: bool,
    pub(super) cache_warming_control: Option<octet_agent::RunControl>,
}

/// Maintenance events are session accounting, never assistant messages/turns.
pub(super) fn emit_cache_warming_event(
    event: &AgentEvent,
    output: &mut RpcOutput,
) -> anyhow::Result<()> {
    match event {
        AgentEvent::CacheWarmed {
            usage,
            cost,
            extension_override,
        } => output.send(json!({
            "type": "cache_warmed",
            "usage": usage_value(usage, *cost),
            "cost": cost,
            "extensionOverride": extension_override,
        })),
        AgentEvent::ProviderUsageUncertain => {
            output.send(json!({"type": "provider_usage_uncertain"}))
        }
        _ => Ok(()),
    }
}

impl EventTranslator {
    pub(super) fn new(app: &App, user: Value) -> Self {
        let mut messages = rpc_messages(app);
        messages.push(user.clone());
        Self {
            endpoint: app.model.endpoint.id.0.clone(),
            model: app.model.clone(),
            api: protocol_name(&app.model.spec.protocol).to_owned(),
            partial_text: String::new(),
            partial_reasoning: String::new(),
            channels: Vec::new(),
            message_started: false,
            message_timestamp: now_millis(),
            turn_open: true,
            pending_turn: None,
            pending_tool_results: Vec::new(),
            expected_tools: 0,
            tools: HashMap::new(),
            messages,
            run_messages: vec![user],
            last_assistant_text: String::new(),
            retry_attempt: None,
            pending_retry_end: None,
            cache_warming_control: None,
            usage_uncertain: (app.agent.session().has_uncertain_usage()
                || app.agent.session().has_unpriced_usage()),
        }
    }

    pub(super) fn partial_message(&self) -> Value {
        #[cfg(test)]
        PARTIAL_SNAPSHOTS.with(|count| count.set(count.get() + 1));
        let content = self
            .channels
            .iter()
            .map(|channel| match channel {
                OutputChannel::Text => json!({"type": "text", "text": self.partial_text}),
                OutputChannel::Reasoning => {
                    json!({"type": "thinking", "thinking": self.partial_reasoning})
                }
            })
            .collect::<Vec<_>>();
        json!({
            "role": "assistant",
            "content": content,
            "api": self.api,
            "provider": self.endpoint,
            "model": self.model.spec.id.0,
            "usage": usage_value(&Usage::default(), None),
            "stopReason": null,
            "timestamp": self.message_timestamp
        })
    }

    pub(super) fn ensure_turn_started(&mut self, output: &mut RpcOutput) -> anyhow::Result<()> {
        if !self.turn_open {
            output.send(json!({"type": "turn_start"}))?;
            self.turn_open = true;
        }
        Ok(())
    }

    pub(super) fn begin_assistant(&mut self, output: &mut RpcOutput) -> anyhow::Result<()> {
        self.ensure_turn_started(output)?;
        if !self.message_started {
            self.message_started = true;
            output.send(json!({"type": "message_start", "message": self.partial_message()}))?;
        }
        Ok(())
    }

    pub(super) fn emit_delta(
        &mut self,
        output: &mut RpcOutput,
        channel: OutputChannel,
        text: String,
    ) -> anyhow::Result<()> {
        self.begin_assistant(output)?;
        let first = !self.channels.contains(&channel);
        if first {
            self.channels.push(channel);
        }
        match channel {
            OutputChannel::Text => self.partial_text.push_str(&text),
            OutputChannel::Reasoning => self.partial_reasoning.push_str(&text),
        }
        let kind = match channel {
            OutputChannel::Text => "text",
            OutputChannel::Reasoning => "thinking",
        };
        let content_index = self
            .channels
            .iter()
            .position(|candidate| *candidate == channel)
            .unwrap_or_default();
        // JSON mode never constructs the growing prefix just to discard it.
        // RPC retains both compatibility snapshots, including on channel start.
        let mut send = |event: Value| {
            if output.delta_only {
                output.send(json!({
                    "type": "message_update",
                    "usage": usage_value(&Usage::default(), None),
                    "assistantMessageEvent": event
                }))
            } else {
                let message = self.partial_message();
                let mut event = event;
                event["partial"] = message.clone();
                output.send(json!({
                    "type": "message_update",
                    "message": message,
                    "assistantMessageEvent": event
                }))
            }
        };
        if first {
            send(json!({
                "type": format!("{kind}_start"),
                "contentIndex": content_index
            }))?;
        }
        send(json!({
            "type": format!("{kind}_delta"),
            "contentIndex": content_index,
            "delta": text
        }))
    }

    pub(super) fn emit_content_ends(
        &self,
        output: &mut RpcOutput,
        message: &Value,
    ) -> anyhow::Result<()> {
        for channel in &self.channels {
            let (kind, content) = match channel {
                OutputChannel::Text => ("text", self.partial_text.as_str()),
                OutputChannel::Reasoning => ("thinking", self.partial_reasoning.as_str()),
            };
            let content_index = self
                .channels
                .iter()
                .position(|candidate| candidate == channel)
                .unwrap_or_default();
            output.send(json!({
                "type": "message_update",
                "message": message,
                "assistantMessageEvent": {
                    "type": format!("{kind}_end"),
                    "contentIndex": content_index,
                    "content": content,
                    "partial": message
                }
            }))?;
        }
        Ok(())
    }

    pub(super) fn emit_tool_call_updates(
        &self,
        output: &mut RpcOutput,
        assistant: &AssistantMessage,
        value: &Value,
    ) -> anyhow::Result<()> {
        for (content_index, part) in assistant.content.iter().enumerate() {
            let AssistantPart::ToolCall(call) = part else {
                continue;
            };
            let tool_call = json!({
                "type": "toolCall",
                "id": call.id.0,
                "name": call.name,
                "arguments": serde_json::from_str::<Value>(&call.arguments_json).unwrap_or(Value::Null)
            });
            output.send(json!({
                "type": "message_update",
                "message": value,
                "assistantMessageEvent": {
                    "type": "toolcall_start",
                    "contentIndex": content_index,
                    "id": call.id.0,
                    "name": call.name,
                    "partial": value
                }
            }))?;
            output.send(json!({
                "type": "message_update",
                "message": value,
                "assistantMessageEvent": {
                    "type": "toolcall_end",
                    "contentIndex": content_index,
                    "toolCall": tool_call,
                    "partial": value
                }
            }))?;
        }
        Ok(())
    }

    pub(super) fn finish_pending_turn(&mut self, output: &mut RpcOutput) -> anyhow::Result<()> {
        let Some(message) = self.pending_turn.take() else {
            return Ok(());
        };
        output.send(json!({
            "type": "turn_end",
            "message": message,
            "toolResults": std::mem::take(&mut self.pending_tool_results)
        }))?;
        self.turn_open = false;
        Ok(())
    }

    pub(super) fn reset_partial(&mut self) {
        self.partial_text.clear();
        self.partial_reasoning.clear();
        self.channels.clear();
        self.message_started = false;
        self.message_timestamp = now_millis();
    }

    pub(super) fn deliver_queued(
        &mut self,
        output: &mut RpcOutput,
        queue: &mut QueueState,
        steering: bool,
        summaries: Vec<String>,
    ) -> anyhow::Result<()> {
        let mut delivered = if steering {
            queue.take_steering(summaries.len())
        } else {
            queue.take_follow_up(summaries.len())
        };
        output.send(queue.event())?;
        self.ensure_turn_started(output)?;
        if delivered.is_empty() {
            delivered.extend(summaries.into_iter().map(|text| QueuedInput {
                message: user_value(&text),
                input: UserInput::from(text.clone()),
                text,
            }));
        }
        for queued in delivered {
            output.send(json!({"type": "message_start", "message": queued.message}))?;
            output.send(json!({"type": "message_end", "message": queued.message}))?;
            self.messages.push(queued.message.clone());
            self.run_messages.push(queued.message);
        }
        Ok(())
    }

    pub(super) fn finish_interrupted(
        &mut self,
        output: &mut RpcOutput,
        stop_reason: &str,
        error_message: Option<&str>,
    ) -> anyhow::Result<()> {
        if !self.turn_open {
            return Ok(());
        }
        self.begin_assistant(output)?;
        let mut message = self.partial_message();
        if let Some(object) = message.as_object_mut() {
            object.insert("stopReason".into(), Value::String(stop_reason.to_owned()));
            if let Some(error) = error_message {
                object.insert("errorMessage".into(), Value::String(error.to_owned()));
            }
        }
        self.emit_content_ends(output, &message)?;
        output.send(json!({"type": "message_end", "message": message}))?;
        self.messages.push(message.clone());
        self.run_messages.push(message.clone());
        self.pending_turn = Some(message);
        self.reset_partial();
        self.finish_pending_turn(output)
    }

    pub(super) fn observe(
        &mut self,
        event: AgentEvent,
        output: &mut RpcOutput,
        queue: &mut QueueState,
    ) -> anyhow::Result<Option<HostRunOutcome>> {
        match event {
            AgentEvent::OutputDelta { channel, text } => self.emit_delta(output, channel, text)?,
            // The final TurnFinished message carries generated media in the
            // Pi-compatible content array; no provisional RPC event exists.
            AgentEvent::RecoveredOutput { .. } | AgentEvent::OutputMedia { .. } => {}
            AgentEvent::ProviderInference { metrics } => {
                output.send(json!({"type": "provider_inference", "metrics": metrics}))?;
            }
            AgentEvent::ProviderLifecycle { lifecycle } => {
                // Deliberately outside assistant-message updates: readiness is
                // transient endpoint telemetry, never model content.
                output.send(json!({
                    "type": "provider_lifecycle",
                    "state": lifecycle.state.as_str(),
                    "detail": lifecycle.detail,
                }))?;
            }
            AgentEvent::ProviderRetry {
                attempt,
                max_attempts,
                delay,
                error,
            } => {
                if self.message_started {
                    output
                        .send(json!({"type": "message_end", "message": self.partial_message()}))?;
                }
                self.reset_partial();
                self.retry_attempt = Some(attempt);
                output.send(json!({
                    "type": "auto_retry_start",
                    "attempt": attempt,
                    "maxAttempts": max_attempts,
                    "delayMs": delay.as_millis(),
                    "errorMessage": error
                }))?;
            }
            AgentEvent::CacheWarmed { cost, .. } => {
                self.usage_uncertain |= cost.is_none();
                emit_cache_warming_event(&event, output)?;
            }
            AgentEvent::ExtensionObservationWarning { message } => {
                output
                    .send(json!({"type": "extension_observation_warning", "message": message}))?;
            }
            AgentEvent::ProviderUsageUncertain => {
                self.usage_uncertain = true;
                output.send(json!({"type": "provider_usage_uncertain"}))?;
            }
            AgentEvent::ProviderOperationRetry {
                operation,
                attempt,
                max_attempts,
                delay,
                error,
            } => {
                output.send(json!({
                    "type": "provider_operation_retry", "operation": operation,
                    "attempt": attempt, "maxAttempts": max_attempts,
                    "delayMs": delay.as_millis(), "errorMessage": error,
                }))?;
            }
            AgentEvent::ProviderWaitingForNetwork {
                attempt,
                delay,
                error,
            } => {
                // A pre-send wait is not an assistant message or a discarded
                // inference attempt; retain existing committed RPC history.
                output.send(json!({
                    "type": "provider_waiting_for_network",
                    "attempt": attempt,
                    "delayMs": delay.as_millis(),
                    "errorMessage": error
                }))?;
            }
            AgentEvent::SteeringDelivered { messages } => {
                self.deliver_queued(output, queue, true, messages)?;
            }
            AgentEvent::FollowUpDelivered { messages } => {
                self.deliver_queued(output, queue, false, messages)?;
            }
            AgentEvent::CompactionStarted { reason } => {
                output.send(json!({
                    "type": "compaction_start",
                    "reason": format!("{reason:?}").to_ascii_lowercase()
                }))?;
            }
            AgentEvent::CompactionFinished { reason, result } => match result {
                Ok(info) => output.send(json!({
                    "type": "compaction_end",
                    "reason": format!("{reason:?}").to_ascii_lowercase(),
                    "result": {"summary": info.summary, "firstKeptEntryId": info.first_kept.0},
                    "aborted": false,
                    "willRetry": false
                }))?,
                Err(error) => output.send(json!({
                    "type": "compaction_end",
                    "reason": format!("{reason:?}").to_ascii_lowercase(),
                    "aborted": false,
                    "willRetry": false,
                    "errorMessage": error
                }))?,
            },
            AgentEvent::ToolStarted { id, name, args } => {
                self.tools.insert(
                    id.0.clone(),
                    (name.clone(), args.clone(), RpcToolProgress::default()),
                );
                output.send(json!({
                    "type": "tool_execution_start",
                    "toolCallId": id.0,
                    "toolName": name,
                    "args": args
                }))?;
            }
            // Keep the Pi-compatible RPC event vocabulary unchanged. Native
            // host clients receive this secret-safe diagnostic as `tool_policy`.
            AgentEvent::ToolPolicyDecision { .. } => {}
            AgentEvent::ToolProgress { id, progress } => {
                if let Some((name, args, accumulated)) = self.tools.get_mut(&id.0) {
                    match progress {
                        ToolProgress::Output { bytes, .. } => {
                            accumulated.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        ToolProgress::Status(status) => {
                            accumulated.separator();
                            accumulated.push_str(&status);
                        }
                        ToolProgress::Decoration(decoration) => {
                            accumulated.separator();
                            accumulated.push_str(decoration.label());
                            if let Some(detail) = decoration.detail() {
                                accumulated.push_str(" · ");
                                accumulated.push_str(detail);
                            }
                        }
                        ToolProgress::Dropped { bytes, events } => {
                            accumulated.push_str(&format!(
                                "\n[dropped {bytes} bytes and {events} events]"
                            ));
                        }
                        ToolProgress::PartialResult(result) => {
                            let content = result
                                .content_parts()
                                .iter()
                                .map(|part| match part {
                                    octet_agent::ToolOutputContentPart::Text(text) => {
                                        json!({"type":"text","text":text})
                                    }
                                    octet_agent::ToolOutputContentPart::Media(media) => {
                                        super::projection::media_content(media)
                                    }
                                })
                                .collect::<Vec<_>>();
                            let mut partial =
                                json!({"content":content,"isError":result.is_error()});
                            if let Some(details) = result
                                .metadata()
                                .and_then(|metadata| metadata.get("pi_details"))
                            {
                                partial["details"] = details.clone();
                            }
                            if let Some(structured) = result.structured_content() {
                                partial["structuredContent"] = structured.clone();
                            }
                            output.send(json!({
                                "type":"tool_execution_update", "toolCallId":id.0,
                                "toolName":name, "args":args, "partialResult":partial,
                            }))?;
                            return Ok(None);
                        }
                        ToolProgress::Confirmation(_)
                        | ToolProgress::Input(_)
                        | ToolProgress::SessionEvent(_, _)
                        | ToolProgress::SessionMetadataEvent(_, _) => {}
                    }
                    output.send(json!({
                        "type": "tool_execution_update",
                        "toolCallId": id.0,
                        "toolName": name,
                        "args": args,
                        "partialResult": {"content": [{"type": "text", "text": accumulated.snapshot()}]}
                    }))?;
                }
            }
            AgentEvent::ToolFinished { id, result, .. } => {
                let (name, _args, _) = self
                    .tools
                    .remove(&id.0)
                    .unwrap_or_else(|| (String::new(), Value::Null, RpcToolProgress::default()));
                let (text, is_error) = match result {
                    Ok(output_value) => {
                        let is_error = output_value.is_error();
                        (output_value.text, is_error)
                    }
                    Err(error) => (error.to_string(), true),
                };
                let result = json!({"content": [{"type": "text", "text": text}]});
                output.send(json!({
                    "type": "tool_execution_end",
                    "toolCallId": id.0,
                    "toolName": name,
                    "result": result,
                    "isError": is_error
                }))?;
                let tool_message = json!({
                    "role": "toolResult",
                    "toolCallId": id.0,
                    "toolName": name,
                    "content": [{"type": "text", "text": text}],
                    "isError": is_error,
                    "timestamp": now_millis()
                });
                output.send(json!({"type": "message_start", "message": tool_message}))?;
                output.send(json!({"type": "message_end", "message": tool_message}))?;
                self.messages.push(tool_message.clone());
                self.run_messages.push(tool_message.clone());
                self.pending_tool_results.push(tool_message);
                self.expected_tools = self.expected_tools.saturating_sub(1);
                if self.expected_tools == 0 {
                    self.finish_pending_turn(output)?;
                }
            }
            AgentEvent::CandidateRejected { .. } => {
                if self.message_started {
                    let mut message = self.partial_message();
                    message["stopReason"] = Value::String("stop".into());
                    self.emit_content_ends(output, &message)?;
                    output.send(json!({"type": "message_end", "message": message}))?;
                    output.send(json!({
                        "type": "turn_end",
                        "message": message,
                        "toolResults": []
                    }))?;
                }
                self.turn_open = false;
                self.reset_partial();
            }
            AgentEvent::TurnFinished {
                message,
                stop_reason,
                turn_usage,
                turn_cost,
                ..
            } => {
                self.ensure_turn_started(output)?;
                self.usage_uncertain |= turn_cost.is_none();
                let value = assistant_value(
                    &message,
                    &self.endpoint,
                    &turn_usage,
                    turn_cost,
                    &stop_reason,
                    Some(now_millis() as u64),
                );
                if !self.message_started {
                    output.send(json!({"type": "message_start", "message": value}))?;
                } else {
                    self.emit_content_ends(output, &value)?;
                }
                self.emit_tool_call_updates(output, &message, &value)?;
                output.send(json!({"type": "message_end", "message": value}))?;
                if let Some(attempt) = self.retry_attempt.take() {
                    output.send(json!({
                        "type": "auto_retry_end",
                        "success": true,
                        "attempt": attempt
                    }))?;
                }
                self.last_assistant_text = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        AssistantPart::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                self.expected_tools = message
                    .content
                    .iter()
                    .filter(|part| matches!(part, AssistantPart::ToolCall(_)))
                    .count();
                self.messages.push(value.clone());
                self.run_messages.push(value.clone());
                self.pending_turn = Some(value);
                self.reset_partial();
                if self.expected_tools == 0 {
                    self.finish_pending_turn(output)?;
                }
            }
            AgentEvent::CustomMessageCommitted { .. } | AgentEvent::DelegationUpdated { .. } => {}
            // Attempt boundaries are not part of the RPC protocol surface.
            AgentEvent::TurnStarted => {}
            AgentEvent::RunFinished { reason, .. } => {
                let outcome = HostRunOutcome::from_finish_reason(
                    &reason,
                    &self.model.endpoint.id.0,
                    &self.model.spec.id.0,
                );
                self.settle(outcome.clone(), output)?;
                return Ok(Some(outcome));
            }
        }
        Ok(None)
    }

    pub(super) fn settle(
        &mut self,
        outcome: HostRunOutcome,
        output: &mut RpcOutput,
    ) -> anyhow::Result<()> {
        self.finish_pending_turn(output)?;
        let (stop_reason, owned_error) = match &outcome {
            HostRunOutcome::Completed => (None, None),
            HostRunOutcome::Aborted => (Some("aborted"), Some("Operation aborted".to_owned())),
            HostRunOutcome::MaxTurns => (
                Some("error"),
                Some("Maximum agent turns reached".to_owned()),
            ),
            HostRunOutcome::Failed(error) => (Some("error"), Some(error.clone())),
            HostRunOutcome::StreamLost => (
                Some("error"),
                Some(crate::modes::RUN_STREAM_LOST_MESSAGE.to_owned()),
            ),
            HostRunOutcome::Shutdown => (
                Some("aborted"),
                Some(crate::modes::RUN_SHUTDOWN_MESSAGE.to_owned()),
            ),
        };
        if let Some(stop_reason) = stop_reason {
            self.finish_interrupted(output, stop_reason, owned_error.as_deref())?;
        }
        if let Some(attempt) = self.retry_attempt.take() {
            self.pending_retry_end = Some(json!({
                "type": "auto_retry_end",
                "success": false,
                "attempt": attempt,
                "finalError": owned_error
            }));
        }
        Ok(())
    }
}

pub(super) fn active_state_value(
    base: &Value,
    translator: &EventTranslator,
    queue: &QueueState,
) -> Value {
    let mut state = base.clone();
    if let Some(object) = state.as_object_mut() {
        object.insert("isStreaming".into(), Value::Bool(true));
        object.insert("usageUncertain".into(), json!(translator.usage_uncertain));
        object.insert("messageCount".into(), json!(translator.messages.len()));
        object.insert("pendingMessageCount".into(), json!(queue.len()));
        if let Some(control) = &translator.cache_warming_control {
            object.insert(
                "cacheWarmingMode".into(),
                json!(control.cache_warming_mode()),
            );
            object.insert(
                "cacheWarmingStatus".into(),
                json!(control.cache_warming_status()),
            );
        }
    }
    state
}
