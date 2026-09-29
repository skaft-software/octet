//! Context-window accounting: totals, sources, and compaction.
//! The breakdown has to reconcile every authoritative source and replay exactly,
//! queued prompt sources may only be attributed at a delivery boundary, and a
//! successful, failed, or unobserved compaction must each leave the totals and
//! the source attribution in the right state.

use super::*;

fn agent_context_breakdown(
    system_tokens: u64,
    instruction_tokens: u64,
    conversation_tokens: u64,
    tool_result_tokens: u64,
    attachment_tokens: u64,
    compaction_summary_tokens: u64,
    other_tokens: u64,
) -> AgentContextBreakdown {
    let total_tokens = system_tokens
        .checked_add(instruction_tokens)
        .and_then(|total| total.checked_add(conversation_tokens))
        .and_then(|total| total.checked_add(tool_result_tokens))
        .and_then(|total| total.checked_add(attachment_tokens))
        .and_then(|total| total.checked_add(compaction_summary_tokens))
        .and_then(|total| total.checked_add(other_tokens))
        .unwrap();
    AgentContextBreakdown {
        system_tokens,
        instruction_tokens,
        conversation_tokens,
        tool_result_tokens,
        attachment_tokens,
        compaction_summary_tokens,
        other_tokens,
        total_tokens,
        structural_tokens: total_tokens,
        provider_tokens: Some(total_tokens),
        context_limit: 1_000,
    }
}

fn agent_context_snapshot(revision: u64, context: AgentContextBreakdown) -> AgentContextSnapshot {
    AgentContextSnapshot {
        revision,
        context,
        ..AgentContextSnapshot::default()
    }
}

fn category_tokens(totals: &ContextTotals, category: ContextCategory) -> u64 {
    totals
        .categories
        .iter()
        .find(|total| total.category == category)
        .map_or(0, |total| total.tokens)
}

#[test]
fn context_projection_reconciles_authoritative_sources_and_replays_exactly() {
    let context = agent_context_breakdown(10, 50, 100, 5, 6, 7, 8);
    let mut snapshot = agent_context_snapshot(1, context);
    snapshot.phase = AgentRunPhase::Responding;
    snapshot.responses_started = 2;
    snapshot.responses_finished = 1;
    snapshot.response_active = true;
    snapshot.tool_calls_started = 2;
    snapshot.tool_calls_finished = 1;
    snapshot.tool_executions_started = 1;
    snapshot.run_usage = octet_ai::Usage {
        input_tokens: 11,
        cache_read_tokens: 12,
        cache_write_tokens: 13,
        output_tokens: 14,
        total_tokens: 50,
        ..octet_ai::Usage::default()
    };
    let run_id = RunId::new("run-context-sources").unwrap();
    let mut projection = RunContextProjection::new(20, 30, 40);

    let projected = project_context_snapshot(snapshot.clone(), &run_id, &mut projection)
        .unwrap()
        .unwrap();
    assert_eq!(projected.status.current.total_tokens, 186);
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::System),
        40
    );
    assert_eq!(
        category_tokens(
            &projected.status.current,
            ContextCategory::ProjectInstructions
        ),
        20
    );
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::Conversation),
        30
    );
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::Documents),
        30
    );
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::ProjectFiles),
        40
    );
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::ToolResults),
        5
    );
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::Attachments),
        6
    );
    assert_eq!(
        category_tokens(
            &projected.status.current,
            ContextCategory::CompactionSummaries
        ),
        7
    );
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::Other),
        8
    );
    assert_eq!(projected.usage.input_tokens, 36);
    assert_eq!(projected.usage.output_tokens, 14);
    assert_eq!(projected.usage.context_tokens, 186);
    assert_eq!(projected.usage.context_limit, Some(1_000));
    assert_eq!(
        projected.run.as_ref().map(|run| run.phase),
        Some(ServeRunPhase::Responding)
    );
    projected.validate().unwrap();

    assert!(
        project_context_snapshot(snapshot.clone(), &run_id, &mut projection)
            .unwrap()
            .is_none()
    );
    snapshot.phase = AgentRunPhase::Retrying;
    assert!(matches!(
        project_context_snapshot(snapshot, &run_id, &mut projection),
        Err(ServiceError::Internal)
    ));
}

#[tokio::test]
async fn context_publication_emits_complete_state_once_per_revision() {
    let snapshot = agent_context_snapshot(1, agent_context_breakdown(2, 3, 5, 7, 11, 13, 17));
    let run_id = RunId::new("run-context-publication").unwrap();
    let mut projection = RunContextProjection::new(3, 2, 1);
    let (events, mut received) = mpsc::channel(2);

    publish_context_snapshot(snapshot.clone(), &run_id, &mut projection, &events)
        .await
        .unwrap();
    let event = received.recv().await.unwrap();
    let EventPayload::ContextUpdated { context } = event.payload else {
        panic!("expected a complete context update");
    };
    assert_eq!(context.status.current.total_tokens, 58);
    context.validate().unwrap();

    publish_context_snapshot(snapshot, &run_id, &mut projection, &events)
        .await
        .unwrap();
    assert!(matches!(
        received.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn queued_prompt_sources_are_attributed_only_at_matching_delivery_boundaries() {
    let mut items = ProjectionState::new(0);
    items.pending_user_items.extend([
        PendingUserItem {
            id: ItemId::new("queued-steer-1").unwrap(),
            delivery: UserMessageDelivery::Steer,
            turn_id: TurnId::new("queued-turn-1").unwrap(),
            documents: Vec::new(),
            project_files: Vec::new(),
            document_context_tokens: 11,
            project_file_context_tokens: 7,
            context_attributed: false,
            branch_provenance: None,
        },
        PendingUserItem {
            id: ItemId::new("queued-follow-up").unwrap(),
            delivery: UserMessageDelivery::FollowUp,
            turn_id: TurnId::new("queued-turn-2").unwrap(),
            documents: Vec::new(),
            project_files: Vec::new(),
            document_context_tokens: 13,
            project_file_context_tokens: 5,
            context_attributed: false,
            branch_provenance: None,
        },
        PendingUserItem {
            id: ItemId::new("queued-steer-2").unwrap(),
            delivery: UserMessageDelivery::Steer,
            turn_id: TurnId::new("queued-turn-3").unwrap(),
            documents: Vec::new(),
            project_files: Vec::new(),
            document_context_tokens: 17,
            project_file_context_tokens: 3,
            context_attributed: false,
            branch_provenance: None,
        },
    ]);
    let mut context = RunContextProjection::new(0, 0, 0);
    assert_eq!(context.document_context_tokens, 0);
    assert_eq!(context.project_file_context_tokens, 0);

    attribute_delivered_prompt_context(&mut items, &mut context, UserMessageDelivery::Steer, 1)
        .unwrap();
    assert_eq!(context.document_context_tokens, 11);
    assert_eq!(context.project_file_context_tokens, 7);
    assert!(items.pending_user_items[0].context_attributed);
    assert!(!items.pending_user_items[1].context_attributed);
    assert!(!items.pending_user_items[2].context_attributed);

    attribute_delivered_prompt_context(&mut items, &mut context, UserMessageDelivery::FollowUp, 1)
        .unwrap();
    assert_eq!(context.document_context_tokens, 24);
    assert_eq!(context.project_file_context_tokens, 12);

    attribute_delivered_prompt_context(&mut items, &mut context, UserMessageDelivery::Steer, 1)
        .unwrap();
    assert_eq!(context.document_context_tokens, 41);
    assert_eq!(context.project_file_context_tokens, 15);
    assert!(items
        .pending_user_items
        .iter()
        .all(|pending| pending.context_attributed));
}

#[test]
fn successful_context_compaction_reconciles_sources_and_later_timestamps() {
    let before = agent_context_breakdown(10, 60, 100, 10, 5, 0, 5);
    let after = agent_context_breakdown(10, 20, 20, 5, 0, 50, 5);
    let run_id = RunId::new("run-successful-compaction").unwrap();
    let mut projection = RunContextProjection::new(20, 30, 40);

    let mut active = agent_context_snapshot(1, before.clone());
    active.phase = AgentRunPhase::Compacting;
    active.active_compaction = Some(octet_agent::ActiveContextCompaction {
        id: 1,
        reason: CompactionReason::Threshold,
        before: before.clone(),
    });
    active.compactions_started = 1;
    let projected_active = project_context_snapshot(active, &run_id, &mut projection)
        .unwrap()
        .unwrap();
    let active_status = projected_active.status.active_compaction.unwrap();
    assert_eq!(active_status.before, projected_active.status.current);
    assert_eq!(
        category_tokens(&active_status.before, ContextCategory::Documents),
        30
    );
    assert_eq!(
        category_tokens(&active_status.before, ContextCategory::ProjectFiles),
        40
    );

    let mut finished = agent_context_snapshot(2, after.clone());
    finished.last_compaction = Some(octet_agent::FinishedContextCompaction {
        id: 1,
        reason: CompactionReason::Threshold,
        before,
        after: after.clone(),
        succeeded: true,
    });
    finished.compactions_started = 1;
    finished.compactions_completed = 1;
    let projected = project_context_snapshot(finished.clone(), &run_id, &mut projection)
        .unwrap()
        .unwrap();
    let completed = projected.status.last_compaction.as_ref().unwrap();
    assert!(completed.succeeded);
    assert_eq!(completed.id, active_status.id);
    assert_eq!(completed.reclaimed_tokens, 80);
    assert_eq!(completed.before.total_tokens, 190);
    assert_eq!(completed.after.total_tokens, 110);
    assert_eq!(projected.status.current, completed.after);
    assert!(projected.status.active_compaction.is_none());
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::Documents),
        0
    );
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::ProjectFiles),
        0
    );
    assert_eq!(projection.document_context_tokens, 0);
    assert_eq!(projection.project_file_context_tokens, 0);
    projected.validate().unwrap();

    let forced_finish = u64::MAX - 2;
    projection.context_updated_at_ms = forced_finish;
    projection
        .last_compaction
        .as_mut()
        .unwrap()
        .1
        .finished_at_ms = forced_finish;
    let mut later = finished;
    later.revision = 3;
    later.context = agent_context_breakdown(10, 20, 20, 5, 0, 50, 6);
    let projected_later = project_context_snapshot(later, &run_id, &mut projection)
        .unwrap()
        .unwrap();
    assert_eq!(projected_later.status.updated_at_ms, forced_finish + 1);
    assert!(
        projected_later.status.updated_at_ms
            > projected_later
                .status
                .last_compaction
                .as_ref()
                .unwrap()
                .finished_at_ms
    );
    projected_later.validate().unwrap();
}

#[test]
fn completed_compaction_projects_correctly_when_active_revision_was_not_observed() {
    let before = agent_context_breakdown(10, 60, 100, 10, 5, 0, 5);
    let after = agent_context_breakdown(10, 20, 20, 5, 0, 50, 5);
    let mut snapshot = agent_context_snapshot(2, after.clone());
    snapshot.last_compaction = Some(octet_agent::FinishedContextCompaction {
        id: 1,
        reason: CompactionReason::Overflow,
        before,
        after,
        succeeded: true,
    });
    snapshot.compactions_started = 1;
    snapshot.compactions_completed = 1;
    let mut projection = RunContextProjection::new(20, 30, 40);
    let projected = project_context_snapshot(
        snapshot,
        &RunId::new("run-missed-active-compaction").unwrap(),
        &mut projection,
    )
    .unwrap()
    .unwrap();
    let completed = projected.status.last_compaction.unwrap();
    assert_eq!(
        category_tokens(&completed.before, ContextCategory::Documents),
        30
    );
    assert_eq!(
        category_tokens(&completed.before, ContextCategory::ProjectFiles),
        40
    );
    assert_eq!(
        category_tokens(&completed.after, ContextCategory::Documents),
        0
    );
    assert_eq!(completed.reclaimed_tokens, 80);
}

#[test]
fn failed_context_compaction_preserves_totals_and_source_attribution() {
    let before = agent_context_breakdown(10, 60, 100, 10, 5, 0, 5);
    let run_id = RunId::new("run-failed-compaction").unwrap();
    let mut projection = RunContextProjection::new(20, 30, 40);
    let mut active = agent_context_snapshot(1, before.clone());
    active.phase = AgentRunPhase::Compacting;
    active.active_compaction = Some(octet_agent::ActiveContextCompaction {
        id: 1,
        reason: CompactionReason::Overflow,
        before: before.clone(),
    });
    active.compactions_started = 1;
    project_context_snapshot(active, &run_id, &mut projection)
        .unwrap()
        .unwrap();

    let mut failed = agent_context_snapshot(2, before.clone());
    failed.last_compaction = Some(octet_agent::FinishedContextCompaction {
        id: 1,
        reason: CompactionReason::Overflow,
        before: before.clone(),
        after: before,
        succeeded: false,
    });
    failed.compactions_started = 1;
    failed.compactions_failed = 1;
    let projected = project_context_snapshot(failed, &run_id, &mut projection)
        .unwrap()
        .unwrap();
    let completed = projected.status.last_compaction.as_ref().unwrap();
    assert!(!completed.succeeded);
    assert_eq!(completed.before, completed.after);
    assert_eq!(completed.reclaimed_tokens, 0);
    assert_eq!(projected.status.current, completed.after);
    assert_eq!(projection.document_context_tokens, 30);
    assert_eq!(projection.project_file_context_tokens, 40);
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::Documents),
        30
    );
    assert_eq!(
        category_tokens(&projected.status.current, ContextCategory::ProjectFiles),
        40
    );
    projected.validate().unwrap();
}
