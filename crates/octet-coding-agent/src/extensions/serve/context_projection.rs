//! Context usage snapshots and their public projection.

use super::*;

pub(super) fn stable_tool_item_id(tool_call_id: &str) -> Result<ItemId, ServiceError> {
    let hash = stable_hash(tool_call_id.as_bytes());
    ItemId::new(format!("item-tool-{}", &hash[..24])).map_err(|_| ServiceError::Internal)
}

pub(super) fn public_compaction_reason(reason: CompactionReason) -> ContextCompactionReason {
    match reason {
        CompactionReason::Threshold => ContextCompactionReason::Threshold,
        CompactionReason::Overflow => ContextCompactionReason::Overflow,
    }
}

pub(super) fn public_run_phase(phase: AgentRunPhase) -> ServeRunPhase {
    match phase {
        AgentRunPhase::Preparing => ServeRunPhase::Preparing,
        AgentRunPhase::Responding => ServeRunPhase::Responding,
        AgentRunPhase::Retrying => ServeRunPhase::Retrying,
        AgentRunPhase::Compacting => ServeRunPhase::Compacting,
        AgentRunPhase::ExecutingTool => ServeRunPhase::ExecutingTool,
        AgentRunPhase::Finished => ServeRunPhase::Finished,
    }
}

pub(super) fn public_terminal_state(state: AgentRunTerminalState) -> ServeRunTerminalState {
    match state {
        AgentRunTerminalState::Completed => ServeRunTerminalState::Completed,
        AgentRunTerminalState::Aborted => ServeRunTerminalState::Aborted,
        AgentRunTerminalState::Failed => ServeRunTerminalState::Failed,
        AgentRunTerminalState::MaxTurns => ServeRunTerminalState::MaxTurns,
        AgentRunTerminalState::Dropped => ServeRunTerminalState::Dropped,
    }
}

pub(super) fn public_context_totals(
    context: &AgentContextBreakdown,
    projection: &RunContextProjection,
) -> Result<ContextTotals, ServiceError> {
    if context.categorized_tokens() != context.total_tokens {
        return Err(ServiceError::Internal);
    }
    let project_instructions = projection
        .project_instruction_tokens
        .min(context.instruction_tokens);
    let base_instructions = context
        .instruction_tokens
        .saturating_sub(project_instructions);
    let system = context
        .system_tokens
        .checked_add(base_instructions)
        .ok_or(ServiceError::Internal)?;
    let documents = projection
        .document_context_tokens
        .min(context.conversation_tokens);
    let remaining_conversation = context.conversation_tokens.saturating_sub(documents);
    let project_files = projection
        .project_file_context_tokens
        .min(remaining_conversation);
    let conversation = remaining_conversation.saturating_sub(project_files);
    ContextTotals::try_new(
        vec![
            ContextCategoryTotal {
                category: ContextCategory::System,
                tokens: system,
            },
            ContextCategoryTotal {
                category: ContextCategory::ProjectInstructions,
                tokens: project_instructions,
            },
            ContextCategoryTotal {
                category: ContextCategory::Conversation,
                tokens: conversation,
            },
            ContextCategoryTotal {
                category: ContextCategory::ToolResults,
                tokens: context.tool_result_tokens,
            },
            ContextCategoryTotal {
                category: ContextCategory::Attachments,
                tokens: context.attachment_tokens,
            },
            ContextCategoryTotal {
                category: ContextCategory::Documents,
                tokens: documents,
            },
            ContextCategoryTotal {
                category: ContextCategory::ProjectFiles,
                tokens: project_files,
            },
            ContextCategoryTotal {
                category: ContextCategory::CompactionSummaries,
                tokens: context.compaction_summary_tokens,
            },
            ContextCategoryTotal {
                category: ContextCategory::Other,
                tokens: context.other_tokens,
            },
        ],
        context.total_tokens,
    )
    .map_err(|_| ServiceError::Internal)
}

pub(super) fn public_compaction_id(
    run_id: &RunId,
    compaction_id: u64,
) -> Result<RuntimeId, ServiceError> {
    let source = format!("{}:{compaction_id}", run_id.as_str());
    RuntimeId::new(format!(
        "compaction.{}",
        &stable_hash(source.as_bytes())[..24]
    ))
    .map_err(|_| ServiceError::Internal)
}

pub(super) fn project_context_snapshot(
    snapshot: AgentContextSnapshot,
    run_id: &RunId,
    projection: &mut RunContextProjection,
) -> Result<Option<ContextUsage>, ServiceError> {
    if let Some(previous) = &projection.last_agent_snapshot {
        if snapshot.revision < previous.revision {
            return Err(ServiceError::Internal);
        }
        if snapshot.revision == previous.revision {
            return if &snapshot == previous {
                Ok(None)
            } else {
                Err(ServiceError::Internal)
            };
        }
    }
    let observed_at_ms = now_ms().max(projection.context_updated_at_ms);
    let new_finished = snapshot.last_compaction.as_ref().is_some_and(|finished| {
        projection
            .last_compaction
            .as_ref()
            .is_none_or(|(id, _)| *id != finished.id)
    });

    if new_finished {
        let finished = snapshot
            .last_compaction
            .as_ref()
            .ok_or(ServiceError::Internal)?;
        let (started_at_ms, before) = match projection.active_compaction.as_ref() {
            Some((id, active)) if *id == finished.id => {
                (active.started_at_ms, active.before.clone())
            }
            Some(_) => return Err(ServiceError::Internal),
            None => (
                observed_at_ms,
                public_context_totals(&finished.before, projection)?,
            ),
        };
        if finished.succeeded {
            projection.clear_auxiliary_sources();
        }
        let after = public_context_totals(&finished.after, projection)?;
        let reclaimed_tokens = before
            .total_tokens
            .checked_sub(after.total_tokens)
            .ok_or(ServiceError::Internal)?;
        if !finished.succeeded && (before != after || reclaimed_tokens != 0) {
            return Err(ServiceError::Internal);
        }
        let finished_at_ms = observed_at_ms.max(started_at_ms);
        projection.last_compaction = Some((
            finished.id,
            CompletedCompaction {
                id: public_compaction_id(run_id, finished.id)?,
                reason: public_compaction_reason(finished.reason),
                before,
                after,
                reclaimed_tokens,
                succeeded: finished.succeeded,
                started_at_ms,
                finished_at_ms,
            },
        ));
        projection.active_compaction = None;
        projection.context_updated_at_ms = finished_at_ms;
    }

    let current = public_context_totals(&snapshot.context, projection)?;
    if new_finished
        && projection
            .last_compaction
            .as_ref()
            .is_none_or(|(_, completed)| completed.after != current)
    {
        return Err(ServiceError::Internal);
    }
    if projection.current_totals.as_ref() != Some(&current) {
        projection.current_totals = Some(current.clone());
        let mut updated_at_ms = observed_at_ms;
        if !new_finished {
            if let Some((_, completed)) = &projection.last_compaction {
                if updated_at_ms <= completed.finished_at_ms && current != completed.after {
                    updated_at_ms = completed
                        .finished_at_ms
                        .checked_add(1)
                        .ok_or(ServiceError::Internal)?;
                }
            }
        }
        projection.context_updated_at_ms = updated_at_ms;
    }

    if let Some(active) = &snapshot.active_compaction {
        let before = public_context_totals(&active.before, projection)?;
        if before != current {
            return Err(ServiceError::Internal);
        }
        match projection.active_compaction.as_ref() {
            Some((id, existing)) if *id == active.id => {
                if existing.before != before
                    || existing.reason != public_compaction_reason(active.reason)
                {
                    return Err(ServiceError::Internal);
                }
            }
            Some(_) => return Err(ServiceError::Internal),
            None => {
                let started_at_ms = observed_at_ms.max(projection.context_updated_at_ms);
                projection.active_compaction = Some((
                    active.id,
                    ActiveCompaction {
                        id: public_compaction_id(run_id, active.id)?,
                        reason: public_compaction_reason(active.reason),
                        before,
                        started_at_ms,
                    },
                ));
            }
        }
    } else if !new_finished && projection.active_compaction.is_some() {
        return Err(ServiceError::Internal);
    }

    let compactions =
        u32::try_from(snapshot.compactions_completed).map_err(|_| ServiceError::Internal)?;
    let usage = &snapshot.run_usage;
    let context = ContextUsage {
        usage_uncertain: projection.usage_uncertain,
        usage: UsageSnapshot {
            input_tokens: usage
                .input_tokens
                .saturating_add(usage.cache_read_tokens)
                .saturating_add(usage.cache_write_tokens),
            output_tokens: usage.output_tokens,
            context_tokens: current.total_tokens,
            context_limit: Some(snapshot.context.context_limit),
        },
        compactions,
        status: ContextStatus {
            current,
            updated_at_ms: projection.context_updated_at_ms,
            active_compaction: projection
                .active_compaction
                .as_ref()
                .map(|(_, active)| active.clone()),
            last_compaction: projection
                .last_compaction
                .as_ref()
                .map(|(_, completed)| completed.clone()),
        },
        run: Some(AgentRunTelemetry {
            phase: public_run_phase(snapshot.phase),
            terminal_state: snapshot.terminal_state.map(public_terminal_state),
            responses_started: snapshot.responses_started,
            responses_finished: snapshot.responses_finished,
            responses_discarded: snapshot.responses_discarded,
            response_active: snapshot.response_active,
            tool_calls_started: snapshot.tool_calls_started,
            tool_calls_finished: snapshot.tool_calls_finished,
            tool_executions_started: snapshot.tool_executions_started,
            tool_executions_finished: snapshot.tool_executions_finished,
            compactions_started: snapshot.compactions_started,
            compactions_completed: snapshot.compactions_completed,
            compactions_failed: snapshot.compactions_failed,
        }),
    };
    context.validate().map_err(|_| ServiceError::Internal)?;
    projection.last_agent_snapshot = Some(snapshot);
    if projection.last_published.as_ref() == Some(&context) {
        return Ok(None);
    }
    projection.last_published = Some(context.clone());
    Ok(Some(context))
}

pub(super) async fn publish_context_snapshot(
    snapshot: AgentContextSnapshot,
    run_id: &RunId,
    projection: &mut RunContextProjection,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    let Some(context) = project_context_snapshot(snapshot, run_id, projection)? else {
        return Ok(());
    };
    events
        .send(event(EventPayload::ContextUpdated { context }))
        .await
        .map_err(|_| ServiceError::Unavailable)
}

pub(super) fn attribute_delivered_prompt_context(
    projection: &mut ProjectionState,
    context_projection: &mut RunContextProjection,
    delivery: UserMessageDelivery,
    delivered_count: usize,
) -> Result<(), ServiceError> {
    let mut attributed = 0usize;
    for pending in &mut projection.pending_user_items {
        if attributed == delivered_count {
            break;
        }
        if pending.delivery != delivery || pending.context_attributed {
            continue;
        }
        context_projection.attribute_sources(
            pending.document_context_tokens,
            pending.project_file_context_tokens,
        );
        pending.context_attributed = true;
        attributed = attributed.saturating_add(1);
    }
    if attributed != delivered_count {
        return Err(ServiceError::Internal);
    }
    Ok(())
}
