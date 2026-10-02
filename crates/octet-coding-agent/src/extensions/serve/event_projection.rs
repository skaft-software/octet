//! Projecting agent events and session entries into transcript items.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn project_agent_event(
    agent_event: AgentEvent,
    run_id: &RunId,
    plan: &WorkerPlan,
    provider_model: &Model,
    projection: &mut ProjectionState,
    context_projection: &mut RunContextProjection,
    events: &mpsc::Sender<TimestampedEvent>,
    response_text: &mut String,
) -> Result<Option<HostRunOutcome>, ServiceError> {
    match agent_event {
        AgentEvent::ProviderUsageUncertain => {
            projection.usage_uncertain = true;
            context_projection.usage_uncertain = true;
            // Publish independently of host-marker persistence: a failed append
            // must not hide the session's already-known accounting uncertainty.
            let mut context = context_projection
                .last_published
                .clone()
                .unwrap_or_default();
            context.usage_uncertain = true;
            context_projection.last_published = Some(context.clone());
            projection.last_context = Some(context.clone());
            let persisted = plan
                .usage
                .lock()
                .map_err(|_| ServiceError::Internal)
                .and_then(|mut usage| {
                    usage
                        .record_uncertainty(plan.session_id.as_str())
                        .map_err(usage_store_service_error)
                });
            events
                .send(event(EventPayload::ContextUpdated { context }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
            crate::output::stderr!("warning: provider usage and cost are uncertain; displayed numeric usage is a known subtotal, not a complete total.");
            persisted?;
        }
        AgentEvent::CacheWarmed {
            cost,
            extension_override,
            ..
        } => {
            // The durable ledger is mirrored by sync_session_usage on settle.
            // This auxiliary operation creates no assistant/turn/context item.
            if plan.config.show_cache_miss_notices {
                crate::output::stderr_line(crate::commands::cache_warmed_notice(
                    cost,
                    extension_override,
                ));
            }
        }
        AgentEvent::TurnStarted => {}
        AgentEvent::ProviderLifecycle { .. }
        | AgentEvent::ProviderWaitingForNetwork { .. }
        | AgentEvent::ProviderOperationRetry { .. } => {
            // Serve's durable item protocol intentionally has no endpoint-status
            // item. Keep transport readiness out of session projections. A
            // pre-send wait neither settles the run nor retracts committed items.
        }
        AgentEvent::OutputDelta { channel, text } => {
            let text = bounded_text(&text, MAX_ITEM_TEXT_BYTES);
            let turn_id = projection.turn_id(run_id)?;
            let (slot, kind, payload, delta) = match channel {
                OutputChannel::Text => (
                    &mut projection.assistant_item,
                    "assistant",
                    ItemPayload::AssistantMessage {
                        text: String::new(),
                    },
                    ItemDelta::AssistantText {
                        append: text.clone(),
                    },
                ),
                OutputChannel::Reasoning => (
                    &mut projection.reasoning_item,
                    "reasoning",
                    ItemPayload::Reasoning {
                        text: String::new(),
                    },
                    ItemDelta::ReasoningText {
                        append: text.clone(),
                    },
                ),
            };
            if let Some(item_id) = slot.clone() {
                events
                    .send(event(EventPayload::ItemDelta { item_id, delta }))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            } else {
                let item_id = ItemId::new(format!(
                    "item-{}-{kind}-{}-{}",
                    run_id.as_str(),
                    projection.turn_counter,
                    projection.provider_attempt
                ))
                .map_err(|_| ServiceError::Internal)?;
                let payload = match payload {
                    ItemPayload::AssistantMessage { .. } => ItemPayload::AssistantMessage { text },
                    ItemPayload::Reasoning { .. } => ItemPayload::Reasoning { text },
                    _ => unreachable!(),
                };
                events
                    .send(event(EventPayload::ItemStarted {
                        item: SessionItem {
                            id: item_id.clone(),
                            run_id: Some(run_id.clone()),
                            turn_id: Some(turn_id.clone()),
                            provider_attempt: Some(projection.provider_attempt),
                            lifecycle: ItemLifecycle::Provisional,
                            durable_entry_id: None,
                            payload,
                        },
                    }))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
                projection.item_turns.insert(item_id.clone(), turn_id);
                *slot = Some(item_id);
            }
        }
        AgentEvent::OutputMedia { .. } => {
            // The shipped Serve protocol has no generated-media item type.
            // TurnFinished still carries the durable assistant message.
        }
        AgentEvent::ProviderRetry { .. } | AgentEvent::CandidateRejected { .. } => {
            retract_attempt(run_id, projection, events).await?;
            projection.provider_attempt = projection.provider_attempt.saturating_add(1);
        }
        AgentEvent::ToolStarted { id, name, args } => {
            let item_id = stable_tool_item_id(&id.0)?;
            let turn_id = projection.turn_id(run_id)?;
            let started_at_ms = now_ms();
            projection.tool_items.insert(id.0.clone(), item_id.clone());
            let arguments = if octet_serve_backend::validate_json(
                "tool.arguments",
                &args,
                256 * 1024,
            )
            .is_ok()
            {
                args
            } else {
                serde_json::Value::Null
            };
            let activity =
                semantic_tool_activity(&name, &arguments, &plan.config.workspace, started_at_ms);
            projection.tool_calls.insert(
                id.0.clone(),
                ProjectedToolCall {
                    name: name.clone(),
                    arguments,
                    activity: activity.clone(),
                    result: None,
                    turn_id: turn_id.clone(),
                },
            );
            projection
                .item_turns
                .insert(item_id.clone(), turn_id.clone());
            events
                .send(event(EventPayload::ItemStarted {
                    item: SessionItem {
                        id: item_id,
                        run_id: Some(run_id.clone()),
                        turn_id: Some(turn_id),
                        provider_attempt: Some(projection.provider_attempt),
                        lifecycle: ItemLifecycle::Provisional,
                        durable_entry_id: None,
                        payload: ItemPayload::ToolCall(activity),
                    },
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
        }
        AgentEvent::ToolPolicyDecision { .. } => {
            // The Serve protocol currently has no policy-decision item or
            // delta. ToolFinished still projects the corresponding denial;
            // keep this explicit so adding agent-side policy provenance does
            // not silently change the graphical projection contract.
        }
        AgentEvent::ToolProgress { id, progress } => {
            project_tool_progress(id, progress, run_id, projection, events).await?;
        }
        AgentEvent::ToolFinished {
            id,
            result,
            duration: _,
        } => {
            let tool_item_id = projection.tool_items.get(&id.0).cloned();
            let projected = projection.tool_calls.get(&id.0).cloned();
            if let (Some(item_id), Some(mut tool)) = (tool_item_id.clone(), projected) {
                let progress = projection.tool_progress.remove(&id.0).unwrap_or_default();
                let (activity, mut semantic_result) = complete_tool_activity(
                    tool.activity.clone(),
                    &tool.name,
                    &result,
                    now_ms(),
                    progress,
                );
                semantic_result.tool_call_item_id = item_id.clone();
                tool.activity = activity.clone();
                tool.result = Some(semantic_result);
                projection.tool_calls.insert(id.0.clone(), tool);
                if let Ok(output) = result.as_ref() {
                    if let Some(test_results) = project_test_results(&item_id, &activity, output) {
                        projection.test_results.push(test_results);
                    }
                }
                events
                    .send(event(EventPayload::ItemDelta {
                        item_id,
                        delta: ItemDelta::ToolActivity { activity },
                    }))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            }
            if let (Some(_), Some(tool), Some(tool_item_id), Ok(output)) = (
                plan.resources.as_ref(),
                projection.tool_calls.get(&id.0).cloned(),
                tool_item_id,
                result.as_ref(),
            ) {
                projection
                    .pending_tool_evidence
                    .push_back(CompletedToolEvidence {
                        tool_call_id: id.0,
                        tool_item_id,
                        turn_id: projection.turn_id(run_id)?,
                        tool,
                        output: output.clone(),
                    });
            }
        }
        AgentEvent::TurnFinished { message, .. } => {
            response_text.clear();
            response_text.push_str(&super::super::assistant_text(&message));
            projection.finish_turn();
        }
        AgentEvent::RunFinished { reason, .. } => {
            return Ok(Some(HostRunOutcome::from_finish_reason(
                &reason,
                &provider_model.endpoint.id.0,
                &provider_model.spec.id.0,
            )));
        }
        AgentEvent::SteeringDelivered { messages } => {
            attribute_delivered_prompt_context(
                projection,
                context_projection,
                UserMessageDelivery::Steer,
                messages.len(),
            )?;
        }
        AgentEvent::FollowUpDelivered { messages } => {
            attribute_delivered_prompt_context(
                projection,
                context_projection,
                UserMessageDelivery::FollowUp,
                messages.len(),
            )?;
        }
        AgentEvent::RecoveredOutput { .. } | AgentEvent::DelegationUpdated { .. } => {
            // Serve projects owner-fenced subagent state through extension
            // presentation snapshots; native telemetry is TUI-local run chrome.
        }
        AgentEvent::CompactionStarted { .. } | AgentEvent::CompactionFinished { .. } => {}
    }
    Ok(None)
}

pub(super) async fn expire_private_requests(
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
    actor_generation: u64,
) -> Result<(), ServiceError> {
    for (id, request) in projection.private_requests.drain() {
        match request.response {
            PrivateResponse::Approval(respond) => respond(false),
            PrivateResponse::Input(respond) => respond(None),
        }
        events
            .send(event(EventPayload::PendingRequestChanged {
                request: PendingRequest {
                    id,
                    actor_generation,
                    kind: request.kind,
                    state: RequestState::Expired,
                },
            }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    projection.tool_items.clear();
    projection.tool_calls.clear();
    projection.tool_progress.clear();
    Ok(())
}

pub(super) async fn retract_attempt(
    _run_id: &RunId,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    for item_id in [
        projection.assistant_item.take(),
        projection.reasoning_item.take(),
    ]
    .into_iter()
    .flatten()
    {
        events
            .send(event(EventPayload::ItemRetracted {
                item_id,
                provider_attempt: projection.provider_attempt,
                reason: "The provider attempt was replaced before commit.".into(),
            }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    Ok(())
}

pub(super) fn approval_action(prompt: &str, detail: Option<&str>) -> String {
    let mut action = prompt.to_owned();
    if let Some(detail) = detail {
        action.push_str("\n\n");
        action.push_str(detail);
    }
    action
}

pub(super) async fn project_tool_progress(
    id: ToolCallId,
    progress: ToolProgress,
    run_id: &RunId,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    match progress {
        ToolProgress::Output { bytes, .. } => {
            let entry = projection.tool_progress.entry(id.0.clone()).or_default();
            entry.observed_output_bytes = entry
                .observed_output_bytes
                .saturating_add(bytes.len() as u64);
            publish_tool_progress(&id.0, projection, events).await?;
        }
        ToolProgress::Status(_) | ToolProgress::Decoration(_) => {}
        ToolProgress::Dropped { bytes, .. } => {
            let entry = projection.tool_progress.entry(id.0.clone()).or_default();
            entry.dropped_output_bytes = entry.dropped_output_bytes.saturating_add(bytes);
            publish_tool_progress(&id.0, projection, events).await?;
        }
        ToolProgress::Confirmation(request) => {
            projection.request_counter = projection.request_counter.saturating_add(1);
            let request_id = RequestId::new(format!(
                "request-{}-{}",
                run_id.as_str(),
                projection.request_counter
            ))
            .map_err(|_| ServiceError::Internal)?;
            let action = approval_action(&request.prompt, request.detail.as_deref());
            let pending = PendingRequest {
                id: request_id.clone(),
                actor_generation: projection_actor_generation(run_id),
                kind: RequestKind::Approval {
                    action: bounded_text(&action, 8 * 1024),
                    item_id: projection.tool_items.get(&id.0).cloned(),
                },
                state: RequestState::Pending,
            };
            let kind = pending.kind.clone();
            projection.private_requests.insert(
                request_id,
                PrivateRequest {
                    kind,
                    response: PrivateResponse::Approval(Box::new(move |allowed| {
                        request.respond(allowed);
                    })),
                },
            );
            events
                .send(event(EventPayload::PendingRequestChanged {
                    request: pending,
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
            events
                .send(event(EventPayload::SessionStateChanged {
                    state: SessionLiveState::NeedsApproval,
                    active_run_id: Some(run_id.clone()),
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
        }
        ToolProgress::Input(request) => {
            projection.request_counter = projection.request_counter.saturating_add(1);
            let request_id = RequestId::new(format!(
                "request-{}-{}",
                run_id.as_str(),
                projection.request_counter
            ))
            .map_err(|_| ServiceError::Internal)?;
            let pending = PendingRequest {
                id: request_id.clone(),
                actor_generation: projection_actor_generation(run_id),
                kind: RequestKind::UserInput {
                    // The extension-owned prompt is private tool progress. Do
                    // not forward it verbatim across the public boundary.
                    prompt: "A tool needs additional input to continue.".into(),
                    choices: Vec::new(),
                },
                state: RequestState::Pending,
            };
            let kind = pending.kind.clone();
            projection.private_requests.insert(
                request_id,
                PrivateRequest {
                    kind,
                    response: PrivateResponse::Input(Box::new(move |answer| match answer {
                        Some(answer) => request.respond(answer),
                        None => request.cancel(),
                    })),
                },
            );
            events
                .send(event(EventPayload::PendingRequestChanged {
                    request: pending,
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
            events
                .send(event(EventPayload::SessionStateChanged {
                    state: SessionLiveState::NeedsInput,
                    active_run_id: Some(run_id.clone()),
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
        }
        ToolProgress::SessionEvent(_, _) => {}
    }
    Ok(())
}

pub(super) async fn publish_tool_progress(
    tool_call_id: &str,
    projection: &ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    let Some(item_id) = projection.tool_items.get(tool_call_id).cloned() else {
        return Ok(());
    };
    let Some(tool) = projection.tool_calls.get(tool_call_id) else {
        return Ok(());
    };
    let progress = projection
        .tool_progress
        .get(tool_call_id)
        .cloned()
        .unwrap_or_default();
    let mut activity = tool.activity.clone();
    activity.observed_output_bytes = progress.observed_output_bytes;
    activity.dropped_output_bytes = progress.dropped_output_bytes;
    events
        .send(event(EventPayload::ItemDelta {
            item_id,
            delta: ItemDelta::ToolActivity { activity },
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)
}

pub(super) fn is_local_synthetic_assistant(entry: &Entry) -> bool {
    entry
        .metadata
        .as_ref()
        .is_some_and(|metadata| metadata.local_synthetic_assistant)
}

pub(super) fn project_new_entries(
    session: &Session,
    workspace: &Path,
    projection: &mut ProjectionState,
    run_id: Option<&RunId>,
    completion_review: Option<&CompletionReview>,
    attachment_store: Option<&AttachmentStore>,
    session_id: &SessionId,
) -> Result<Vec<SessionItem>, ServiceError> {
    let entries = session.entries();
    let start = projection.known_entries.min(entries.len());
    let mut items = Vec::new();
    for entry in &entries[start..] {
        if is_local_synthetic_assistant(entry) {
            continue;
        }
        let attachments = attachment_refs_for_entry(
            entry,
            attachment_store,
            session_id,
            &mut projection.pending_attachments,
        )?;
        let (
            preferred,
            user_delivery,
            preferred_reasoning,
            resolved_documents,
            resolved_project_files,
            branch_provenance,
        ) = match &entry.value {
            EntryValue::Message(Message::User(message))
                if message
                    .content
                    .iter()
                    .any(|part| matches!(part, UserPart::Text(_) | UserPart::Media(_))) =>
            {
                match projection.pending_user_items.pop_front() {
                    Some(pending) => {
                        projection
                            .item_turns
                            .insert(pending.id.clone(), pending.turn_id);
                        (
                            Some(pending.id),
                            Some(pending.delivery),
                            None,
                            pending.documents,
                            pending.project_files,
                            pending.branch_provenance,
                        )
                    }
                    None => (None, None, None, Vec::new(), Vec::new(), None),
                }
            }
            EntryValue::Message(Message::Assistant(_)) => (
                projection
                    .completed_assistant_items
                    .pop_front()
                    .flatten()
                    .map(|(item_id, turn_id)| {
                        projection.item_turns.insert(item_id.clone(), turn_id);
                        item_id
                    }),
                None,
                projection
                    .completed_reasoning_items
                    .pop_front()
                    .flatten()
                    .map(|(item_id, turn_id)| {
                        projection.item_turns.insert(item_id.clone(), turn_id);
                        item_id
                    }),
                Vec::new(),
                Vec::new(),
                None,
            ),
            _ => (None, None, None, Vec::new(), Vec::new(), None),
        };
        let mut projected = project_entry(
            entry,
            workspace,
            run_id.cloned(),
            preferred,
            user_delivery,
            preferred_reasoning,
            &mut projection.tool_items,
            &mut projection.tool_calls,
            completion_review,
            attachments,
        )?;
        if let Some(user_item) = projected
            .iter_mut()
            .find(|item| matches!(item.payload, ItemPayload::UserMessage { .. }))
        {
            if let ItemPayload::UserMessage {
                documents,
                project_files,
                branch_provenance: projected_provenance,
                ..
            } = &mut user_item.payload
            {
                *documents = resolved_documents;
                *project_files = resolved_project_files;
                *projected_provenance = branch_provenance;
            }
        }
        for item in &mut projected {
            let turn_id =
                projection
                    .item_turns
                    .get(&item.id)
                    .cloned()
                    .or_else(|| match &item.payload {
                        ItemPayload::ToolCall(_) => {
                            projection.tool_items.iter().find_map(|(call_id, item_id)| {
                                if item_id == &item.id {
                                    projection
                                        .tool_calls
                                        .get(call_id)
                                        .map(|tool| tool.turn_id.clone())
                                } else {
                                    None
                                }
                            })
                        }
                        ItemPayload::ToolResult(result) => projection
                            .item_turns
                            .get(&result.tool_call_item_id)
                            .cloned(),
                        _ => run_id.and_then(|run_id| projection.turn_id(run_id).ok()),
                    });
            item.turn_id = turn_id;
        }
        items.extend(projected);
    }
    projection.known_entries = entries.len();
    Ok(items)
}

// Entry projection has several independent identity hints and output indexes;
// keeping them explicit avoids an ambiguous partially populated parameter bag.
#[allow(clippy::too_many_arguments)]
pub(super) fn project_entry(
    entry: &Entry,
    workspace: &Path,
    run_id: Option<RunId>,
    preferred: Option<ItemId>,
    user_delivery: Option<UserMessageDelivery>,
    preferred_reasoning: Option<ItemId>,
    tool_items: &mut HashMap<String, ItemId>,
    tool_calls: &mut HashMap<String, ProjectedToolCall>,
    completion_review: Option<&CompletionReview>,
    attachments: Vec<AttachmentRef>,
) -> Result<Vec<SessionItem>, ServiceError> {
    let durable_id =
        DurableEntryId::new(entry.id.0.clone()).map_err(|_| ServiceError::InvalidSeed)?;
    let mut items = Vec::new();
    match &entry.value {
        EntryValue::Message(Message::User(message)) => {
            let mut user_text = Vec::new();
            for part in &message.content {
                match part {
                    UserPart::Text(text) => user_text.push(text.as_str()),
                    UserPart::Media(_) => {}
                    UserPart::ToolResult(result) => {
                        let content = result
                            .content
                            .iter()
                            .map(|part| match part {
                                ToolResultPart::Text(text) => text.as_str(),
                                ToolResultPart::Media(_) => "[media output]",
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let tool_call_item_id = tool_items
                            .get(&result.tool_call_id.0)
                            .cloned()
                            .unwrap_or(stable_tool_item_id(&result.tool_call_id.0)?);
                        let durable_result = if result.is_error {
                            Err(ToolError::new(content.clone()))
                        } else {
                            Ok(ToolOutput::new(content.clone()))
                        };
                        let semantic_result = if let Some(mut tool) =
                            tool_calls.get(&result.tool_call_id.0).cloned()
                        {
                            if let Some(summary) = tool.result.clone() {
                                summary
                            } else {
                                let (activity, mut summary) = complete_tool_activity(
                                    tool.activity,
                                    &tool.name,
                                    &durable_result,
                                    1,
                                    ProjectedToolProgress::default(),
                                );
                                summary.tool_call_item_id = tool_call_item_id.clone();
                                tool.activity = activity;
                                tool.result = Some(summary.clone());
                                tool_calls.insert(result.tool_call_id.0.clone(), tool);
                                summary
                            }
                        } else {
                            let fallback_turn = TurnId::new("turn-history")
                                .map_err(|_| ServiceError::InvalidSeed)?;
                            let mut fallback = ProjectedToolCall {
                                name: "tool".into(),
                                arguments: serde_json::Value::Null,
                                activity: semantic_tool_activity(
                                    "tool",
                                    &serde_json::Value::Null,
                                    workspace,
                                    1,
                                ),
                                result: None,
                                turn_id: fallback_turn,
                            };
                            let (activity, mut summary) = complete_tool_activity(
                                fallback.activity,
                                &fallback.name,
                                &durable_result,
                                1,
                                ProjectedToolProgress::default(),
                            );
                            summary.tool_call_item_id = tool_call_item_id.clone();
                            fallback.activity = activity;
                            fallback.result = Some(summary.clone());
                            tool_calls.insert(result.tool_call_id.0.clone(), fallback);
                            summary
                        };
                        items.push(committed_item(
                            item_id_for_entry(entry, items.len())?,
                            run_id.clone(),
                            durable_id.clone(),
                            ItemPayload::ToolResult(ToolResultSummary {
                                tool_call_item_id,
                                ..semantic_result
                            }),
                        ));
                    }
                }
            }
            if !user_text.is_empty()
                || message
                    .content
                    .iter()
                    .any(|part| matches!(part, UserPart::Media(_)))
            {
                let visible = entry
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.display_text.as_deref())
                    .unwrap_or_else(|| user_text.first().copied().unwrap_or(""));
                let text = if entry
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.display_text.as_ref())
                    .is_some()
                {
                    visible.to_owned()
                } else {
                    user_text.join("\n")
                };
                items.insert(
                    0,
                    committed_item(
                        preferred.unwrap_or(item_id_for_entry(entry, items.len())?),
                        run_id,
                        durable_id,
                        ItemPayload::UserMessage {
                            text: bounded_text(&text, MAX_PROMPT_BYTES),
                            attachments,
                            documents: Vec::new(),
                            project_files: Vec::new(),
                            delivery: user_delivery,
                            branch_provenance: None,
                        },
                    ),
                );
            }
        }
        EntryValue::Message(Message::Assistant(message)) => {
            let text = message
                .content
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                items.push(committed_item(
                    preferred.unwrap_or(item_id_for_entry(entry, items.len())?),
                    run_id.clone(),
                    durable_id.clone(),
                    ItemPayload::AssistantMessage {
                        text: bounded_text(&text, MAX_ITEM_TEXT_BYTES),
                    },
                ));
            }
            let reasoning = message
                .content
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Reasoning(reasoning) => reasoning.text.as_deref(),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if !reasoning.is_empty() {
                items.push(committed_item(
                    preferred_reasoning.unwrap_or(item_id_for_entry(entry, items.len())?),
                    run_id.clone(),
                    durable_id.clone(),
                    ItemPayload::Reasoning {
                        text: bounded_text(&reasoning, MAX_ITEM_TEXT_BYTES),
                    },
                ));
            }
            for call in message.content.iter().filter_map(|part| match part {
                AssistantPart::ToolCall(call) => Some(call),
                _ => None,
            }) {
                let arguments = call.arguments_value().unwrap_or(serde_json::Value::Null);
                let arguments =
                    if octet_serve_backend::validate_json("tool.arguments", &arguments, 256 * 1024)
                        .is_ok()
                    {
                        arguments
                    } else {
                        serde_json::Value::Null
                    };
                let item_id = tool_items
                    .get(&call.id.0)
                    .cloned()
                    .unwrap_or(stable_tool_item_id(&call.id.0)?);
                tool_items.insert(call.id.0.clone(), item_id.clone());
                let projected =
                    tool_calls
                        .entry(call.id.0.clone())
                        .or_insert_with(|| ProjectedToolCall {
                            name: call.name.clone(),
                            arguments: arguments.clone(),
                            activity: semantic_tool_activity(&call.name, &arguments, workspace, 1),
                            result: None,
                            turn_id: TurnId::new("turn-history")
                                .expect("static historical turn ID is valid"),
                        });
                items.push(committed_item(
                    item_id,
                    run_id.clone(),
                    durable_id.clone(),
                    ItemPayload::ToolCall(projected.activity.clone()),
                ));
            }
        }
        EntryValue::Compaction { summary, .. } => {
            items.push(committed_item(
                item_id_for_entry(entry, 0)?,
                run_id,
                durable_id,
                ItemPayload::Compaction {
                    reason: bounded_text(summary, 4 * 1024),
                },
            ));
        }
        EntryValue::Config { .. } => {
            if let Some(outcome) = entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.run_outcome.as_ref())
            {
                let outcome = match outcome.status {
                    SessionRunOutcomeStatus::Completed => {
                        octet_serve_backend::RunOutcome::Completed
                    }
                    SessionRunOutcomeStatus::Stopped => octet_serve_backend::RunOutcome::Stopped,
                    SessionRunOutcomeStatus::Failed => octet_serve_backend::RunOutcome::Failed,
                };
                let message = entry
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.run_outcome.as_ref())
                    .and_then(|outcome| outcome.message.as_deref())
                    .map(|message| bounded_text(message, 8 * 1024));
                items.push(committed_item(
                    item_id_for_entry(entry, 0)?,
                    run_id,
                    durable_id,
                    ItemPayload::RunOutcome {
                        outcome,
                        review: completion_review.cloned().unwrap_or_else(|| {
                            default_completion_review(outcome, 0, message.as_deref())
                        }),
                        message,
                    },
                ));
            }
        }
        EntryValue::ResponsesTurn { .. }
        | EntryValue::ResponsesCompaction { .. }
        | EntryValue::PromptTemplateSelected { .. }
        | EntryValue::SkillActivated { .. }
        | EntryValue::SkillResourceRead { .. }
        | EntryValue::ResponsesSteering { .. }
        | EntryValue::ResponsesReasoning { .. }
        | EntryValue::SkillDeactivated { .. } => {}
    }
    Ok(items)
}

pub(super) fn committed_item(
    id: ItemId,
    run_id: Option<RunId>,
    durable_entry_id: DurableEntryId,
    payload: ItemPayload,
) -> SessionItem {
    SessionItem {
        id,
        run_id,
        turn_id: None,
        provider_attempt: None,
        lifecycle: ItemLifecycle::Committed,
        durable_entry_id: Some(durable_entry_id),
        payload,
    }
}

pub(super) fn item_id_for_entry(entry: &Entry, part: usize) -> Result<ItemId, ServiceError> {
    ItemId::new(format!("item-entry-{}-part-{part}", entry.id.0))
        .map_err(|_| ServiceError::InvalidSeed)
}

pub(super) fn rehydrate_stored_evidence(
    resources: &octet_serve_backend::ResourceStore,
    session: &Session,
    session_id: &SessionId,
    result_entry: &Entry,
    active_entry_ids: &std::collections::BTreeSet<&str>,
    tool_items: &HashMap<String, ItemId>,
) -> Option<EvidenceProjection> {
    let durable_result_id = DurableEntryId::new(result_entry.id.0.clone()).ok()?;
    let bytes = resources.record(session_id, &durable_result_id).ok()?;
    let record = serde_json::from_slice::<StoredToolEvidence>(&bytes).ok()?;
    if !matches!(record.version, 1 | STORED_EVIDENCE_VERSION)
        || record.session_id != session_id.as_str()
        || record.result_entry_id != result_entry.id.0
        || !active_entry_ids.contains(record.call_entry_id.as_str())
        || !result_entry_has_successful_tool_result(result_entry, &record.tool_call_id)
    {
        return None;
    }
    let call_entry = session.entry(&EntryId(record.call_entry_id.clone()))?;
    if !entry_has_tool_call(call_entry, &record.tool_call_id) {
        return None;
    }
    project_stored_evidence(
        resources,
        session_id,
        &record,
        None,
        None,
        tool_items.get(&record.tool_call_id).cloned(),
    )
    .ok()
}

pub(super) fn result_entry_has_successful_tool_result(entry: &Entry, tool_call_id: &str) -> bool {
    matches!(
        &entry.value,
        EntryValue::Message(Message::User(message))
            if message.content.iter().any(|part| {
                matches!(
                    part,
                    UserPart::ToolResult(result)
                        if result.tool_call_id.0 == tool_call_id && !result.is_error
                )
            })
    )
}

pub(super) fn entry_has_tool_call(entry: &Entry, tool_call_id: &str) -> bool {
    matches!(
        &entry.value,
        EntryValue::Message(Message::Assistant(message))
            if message.content.iter().any(|part| {
                matches!(
                    part,
                    AssistantPart::ToolCall(call) if call.id.0 == tool_call_id
                )
            })
    )
}
