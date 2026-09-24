//! Agent-event translation into the versioned host event stream.
//!
//! Orchestration owns cancellation and settlement; this module owns the
//! observational translation of agent events, bounded text, media metadata,
//! tool progress, and the accumulated run summary.

use std::collections::{BTreeSet, HashMap};

use octet_agent::{AgentEvent, OutputChannel, ToolProgress};
use octet_ai::{AssistantMessage, AssistantPart, Media};

use crate::modes::HostRunOutcome;

use super::protocol::MAX_EVENT_TEXT_BYTES;
use super::transport::Emitter;

#[derive(Default)]
pub(crate) struct EventState {
    pending_text: String,
    pub(crate) final_output: String,
    pub(crate) tool_calls: u64,
    pub(crate) steps: u64,
    pub(crate) files_changed: BTreeSet<String>,
    active_tools: HashMap<String, (String, serde_json::Value)>,
    pub(crate) terminal_head: Option<String>,
}

/// Only diagnostic identifiers are needed from the app, leaving its agent
/// exclusively borrowed by the live run throughout event translation.
pub(crate) async fn translate(
    event: AgentEvent,
    state: &mut EventState,
    emitter: &mut Emitter<'_>,
    endpoint_id: &str,
    model_id: &str,
) -> anyhow::Result<Option<HostRunOutcome>> {
    match event {
        AgentEvent::OutputDelta { channel, text } => {
            if channel == OutputChannel::Text {
                append_bounded(&mut state.pending_text, &text, MAX_EVENT_TEXT_BYTES);
            }
            emitter
                .emit(
                    "model_delta",
                    serde_json::json!({
                        "channel": match channel {
                            OutputChannel::Text => "text",
                            OutputChannel::Reasoning => "reasoning",
                        },
                        "text": clip_text(&text, 64 * 1024),
                    }),
                )
                .await?;
        }
        AgentEvent::OutputMedia { index, media } => {
            emitter
                .emit("output_media", media_payload(index, &media))
                .await?;
        }
        AgentEvent::ProviderLifecycle { lifecycle } => {
            // This is a structured protocol event, not human-facing stdout
            // diagnostics or assistant content.
            emitter
                .emit(
                    "provider_lifecycle",
                    serde_json::json!({
                        "state": lifecycle.state.as_str(),
                        "detail": lifecycle.detail.as_deref().map(|detail| clip_text(detail, 512)),
                    }),
                )
                .await?;
        }
        AgentEvent::ProviderRetry {
            attempt,
            max_attempts,
            delay,
            error,
        } => {
            state.pending_text.clear();
            emitter
                .emit(
                    "provider_retry",
                    serde_json::json!({
                        "attempt": attempt,
                        "max_attempts": max_attempts,
                        "delay_ms": delay.as_millis(),
                        "error": clip_text(&error, 16 * 1024),
                    }),
                )
                .await?;
        }
        AgentEvent::ProviderUsageUncertain => {
            emitter
                .emit("provider_usage_uncertain", serde_json::json!({}))
                .await?;
        }
        AgentEvent::ProviderOperationRetry {
            operation,
            attempt,
            max_attempts,
            delay,
            error,
        } => {
            emitter
                .emit(
                    "provider_operation_retry",
                    serde_json::json!({
                        "operation": operation,
                        "attempt": attempt,
                        "max_attempts": max_attempts,
                        "delay_ms": delay.as_millis(),
                        "error": clip_text(&error, 16 * 1024),
                    }),
                )
                .await?;
        }
        AgentEvent::ProviderWaitingForNetwork {
            attempt,
            delay,
            error,
        } => {
            emitter
                .emit(
                    "provider_waiting_for_network",
                    serde_json::json!({
                        "attempt": attempt,
                        "delay_ms": delay.as_millis(),
                        "error": clip_text(&error, 16 * 1024),
                    }),
                )
                .await?;
        }
        AgentEvent::SteeringDelivered { messages } => {
            emitter
                .emit(
                    "steering_delivered",
                    serde_json::json!({"messages": messages}),
                )
                .await?;
        }
        AgentEvent::FollowUpDelivered { messages } => {
            emitter
                .emit(
                    "follow_up_delivered",
                    serde_json::json!({"messages": messages}),
                )
                .await?;
        }
        AgentEvent::CompactionStarted { reason } => {
            emitter
                .emit(
                    "compaction_start",
                    serde_json::json!({"reason": compaction_reason_label(reason)}),
                )
                .await?;
        }
        AgentEvent::CompactionFinished { reason, result } => {
            let data = match result {
                Ok(info) => serde_json::json!({
                    "reason": compaction_reason_label(reason),
                    "ok": true,
                    "kind": compaction_kind_payload(&info.kind),
                    "summary": clip_text(&info.summary, MAX_EVENT_TEXT_BYTES),
                    "first_kept_entry_id": info.first_kept.0,
                }),
                Err(error) => serde_json::json!({
                    "reason": compaction_reason_label(reason),
                    "ok": false,
                    "error": clip_text(&error, 64 * 1024),
                }),
            };
            emitter.emit("compaction_finish", data).await?;
        }
        AgentEvent::ToolStarted { id, name, args } => {
            state.tool_calls = state.tool_calls.saturating_add(1);
            state
                .active_tools
                .insert(id.0.clone(), (name.clone(), args.clone()));
            emitter
                .emit(
                    "tool_start",
                    serde_json::json!({
                        "toolCallId": id.0,
                        "toolName": name,
                        "input": args,
                    }),
                )
                .await?;
        }
        AgentEvent::ToolPolicyDecision { id, name, decision } => {
            emitter
                .emit(
                    "tool_policy",
                    serde_json::json!({
                        "toolCallId": id.0,
                        "toolName": name,
                        "decision": decision,
                    }),
                )
                .await?;
        }
        AgentEvent::ToolProgress { id, progress } => {
            let data = progress_payload(progress);
            emitter
                .emit(
                    "tool_progress",
                    serde_json::json!({"toolCallId": id.0, "progress": data}),
                )
                .await?;
        }
        AgentEvent::ToolFinished { id, result, .. } => {
            if result.as_ref().is_ok_and(|output| !output.is_error()) {
                if let Some((name, args)) = state.active_tools.get(&id.0) {
                    if matches!(name.as_str(), "edit" | "write") {
                        if let Some(path) = args.get("path").and_then(serde_json::Value::as_str) {
                            state.files_changed.insert(path.to_owned());
                        }
                    }
                }
            }
            let (ok, output, error) = match result {
                Ok(output) if output.is_error() => {
                    (false, String::new(), clip_text(&output.text, 64 * 1024))
                }
                Ok(output) => (true, clip_text(&output.text, 64 * 1024), String::new()),
                Err(error) => (
                    false,
                    String::new(),
                    clip_text(&error.to_string(), 64 * 1024),
                ),
            };
            emitter
                .emit(
                    "tool_finish",
                    serde_json::json!({
                        "toolCallId": id.0,
                        "ok": ok,
                        "output": output,
                        "error": error,
                    }),
                )
                .await?;
            state.active_tools.remove(&id.0);
        }
        AgentEvent::CandidateRejected {
            usage,
            run_cost_microdollars,
            session_cost_microdollars,
        } => {
            state.pending_text.clear();
            emitter
                .emit(
                    "candidate_rejected",
                    serde_json::json!({
                        "run_usage": usage,
                        "run_cost_microdollars": run_cost_microdollars,
                        "session_cost_microdollars": session_cost_microdollars,
                        "discard_provisional_output": true,
                    }),
                )
                .await?;
        }
        AgentEvent::TurnFinished {
            message,
            turn_usage,
            usage,
            session_cost_microdollars,
            run_cost_microdollars,
            ..
        } => {
            state.steps = state.steps.saturating_add(1);
            state.final_output = assistant_text(&message);
            state.pending_text.clear();
            emitter
                .emit(
                    "model_step",
                    serde_json::json!({
                        "step": state.steps,
                        "turn_usage": turn_usage,
                        "run_usage": usage,
                        "session_cost_microdollars": session_cost_microdollars,
                        "run_cost_microdollars": run_cost_microdollars,
                    }),
                )
                .await?;
        }
        AgentEvent::DelegationUpdated { snapshot } => {
            emitter
                .emit(
                    "delegation_updated",
                    serde_json::json!({ "snapshot": snapshot }),
                )
                .await?;
        }
        // Attempt boundaries are not part of the host protocol surface.
        AgentEvent::TurnStarted => {}
        AgentEvent::RunFinished { head, reason } => {
            state.terminal_head = Some(head.0);
            return Ok(Some(HostRunOutcome::from_finish_reason(
                &reason,
                endpoint_id,
                model_id,
            )));
        }
    }
    Ok(None)
}

fn assistant_text(message: &AssistantMessage) -> String {
    let mut text = String::new();
    for part in &message.content {
        if let AssistantPart::Text(value) = part {
            append_bounded(&mut text, value, MAX_EVENT_TEXT_BYTES);
        }
    }
    text
}

fn media_payload(index: usize, media: &Media) -> serde_json::Value {
    match media {
        Media::Image(image) => {
            let (source, bytes) = match &image.source {
                octet_ai::ImageSource::Url(_) => ("url", None),
                octet_ai::ImageSource::Inline(data) => ("inline", Some(data.len())),
                octet_ai::ImageSource::ProviderRef(_) => ("provider_ref", None),
            };
            serde_json::json!({
                "index": index,
                "kind": "image",
                "media_type": image.media_type.as_ref().map(ToString::to_string),
                "source": source,
                "bytes": bytes,
                "payload_omitted": true,
            })
        }
        Media::Audio(audio) => {
            let (source, bytes) = match &audio.payload {
                octet_ai::AudioPayload::Inline(data) => ("inline", Some(data.len())),
                octet_ai::AudioPayload::ProviderRef(_) => ("provider_ref", None),
                octet_ai::AudioPayload::InlineWithProviderRef { data, .. } => {
                    ("inline_with_provider_ref", Some(data.len()))
                }
            };
            serde_json::json!({
                "index": index,
                "kind": "audio",
                "format": format!("{:?}", audio.format).to_ascii_lowercase(),
                "source": source,
                "bytes": bytes,
                "transcript": audio.transcript.as_deref().map(|text| clip_text(text, 64 * 1024)),
                "payload_omitted": true,
            })
        }
    }
}

fn compaction_reason_label(reason: octet_agent::CompactionReason) -> &'static str {
    match reason {
        octet_agent::CompactionReason::Threshold => "threshold",
        octet_agent::CompactionReason::Overflow => "overflow",
    }
}

fn compaction_kind_payload(kind: &octet_agent::CompactionKind) -> serde_json::Value {
    match kind {
        octet_agent::CompactionKind::Local => serde_json::json!({"type": "local"}),
        octet_agent::CompactionKind::Snapcompact => serde_json::json!({"type": "snapcompact"}),
        octet_agent::CompactionKind::NativeResponses {
            checkpoint,
            covered_through,
        } => serde_json::json!({
            "type": "native_responses",
            "checkpoint_entry_id": checkpoint.0,
            "covered_through_entry_id": covered_through.0,
        }),
    }
}

fn progress_payload(progress: ToolProgress) -> serde_json::Value {
    match progress {
        ToolProgress::Output { stream, bytes } => serde_json::json!({
            "type": "output",
            "stream": match stream {
                octet_agent::OutputStream::Stdout => "stdout",
                octet_agent::OutputStream::Stderr => "stderr",
            },
            "text": clip_text(&String::from_utf8_lossy(&bytes), 64 * 1024),
        }),
        ToolProgress::Status(message) => serde_json::json!({
            "type": "status",
            "message": clip_text(&message, 16 * 1024),
        }),
        ToolProgress::Decoration(decoration) => serde_json::json!({
            "type": "decoration",
            "label": clip_text(decoration.label(), 256),
            "detail": decoration.detail().map(|detail| clip_text(detail, 4 * 1024)),
        }),
        ToolProgress::Confirmation(request) => {
            let payload = serde_json::json!({
                "type": "confirmation_required",
                "prompt": clip_text(&request.prompt, 16 * 1024),
                "detail": request.detail.as_deref().map(|detail| clip_text(detail, 16 * 1024)),
                "destructive": request.destructive,
                "default": false,
                "denied": true,
            });
            request.respond(false);
            payload
        }
        ToolProgress::Input(request) => {
            let payload = serde_json::json!({
                "type": "input_required",
                "prompt": clip_text(&request.prompt, 16 * 1024),
                "secret": request.secret,
                "cancelled": true,
            });
            request.cancel();
            payload
        }
        ToolProgress::Dropped { bytes, events } => serde_json::json!({
            "type": "dropped",
            "bytes": bytes,
            "events": events,
        }),
        ToolProgress::SessionEvent(_, _) => serde_json::json!({
            "type": "session_event",
        }),
    }
}

pub(crate) fn clip_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = text.len().saturating_sub(end);
    format!("{}\n[… {omitted} bytes omitted]", &text[..end])
}
fn append_bounded(target: &mut String, text: &str, max_bytes: usize) {
    if target.len() >= max_bytes {
        return;
    }
    let remaining = max_bytes - target.len();
    if text.len() <= remaining {
        target.push_str(text);
        return;
    }
    let mut end = remaining;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
}

#[cfg(test)]
mod tests;
