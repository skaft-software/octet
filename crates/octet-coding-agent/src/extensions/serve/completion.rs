//! Terminal projection, completion review and the persisted run record.

use super::*;

pub(super) struct TerminalProjection {
    pub(super) state: SessionLiveState,
    pub(super) outcome: octet_serve_backend::RunOutcome,
    pub(super) message: Option<String>,
}

impl TerminalProjection {
    pub(super) fn from_host_outcome(outcome: &HostRunOutcome) -> Self {
        match outcome {
            HostRunOutcome::Completed => Self::completed(),
            HostRunOutcome::Aborted | HostRunOutcome::Shutdown => Self::stopped(),
            HostRunOutcome::Failed(error) => Self::failed(error.clone()),
            HostRunOutcome::MaxTurns => Self::failed("The maximum model-turn limit was reached."),
            HostRunOutcome::StreamLost => Self::failed(crate::modes::RUN_STREAM_LOST_MESSAGE),
        }
    }

    pub(super) fn completed() -> Self {
        Self {
            state: SessionLiveState::Done,
            outcome: octet_serve_backend::RunOutcome::Completed,
            message: None,
        }
    }

    pub(super) fn stopped() -> Self {
        Self {
            state: SessionLiveState::Stopped,
            outcome: octet_serve_backend::RunOutcome::Stopped,
            message: None,
        }
    }

    pub(super) fn failed(message: impl Into<String>) -> Self {
        Self {
            state: SessionLiveState::Failed,
            outcome: octet_serve_backend::RunOutcome::Failed,
            message: Some(message.into()),
        }
    }
}

pub(super) fn default_completion_review(
    outcome: octet_serve_backend::RunOutcome,
    duration_ms: u64,
    message: Option<&str>,
) -> CompletionReview {
    let summary = message.map_or_else(
        || match outcome {
            octet_serve_backend::RunOutcome::Completed => "Run completed.".into(),
            octet_serve_backend::RunOutcome::Stopped => "Run stopped.".into(),
            octet_serve_backend::RunOutcome::Failed => "Run failed.".into(),
        },
        |message| bounded_text(message, 2 * 1024),
    );
    CompletionReview {
        summary,
        duration_ms,
        action_count: 0,
        phases: Vec::new(),
        changed_file_item_ids: Vec::new(),
        verification_action_item_ids: Vec::new(),
        failed_action_item_ids: Vec::new(),
        warning_action_item_ids: Vec::new(),
        source_ids: Vec::new(),
        output_ids: Vec::new(),
        test_results: Vec::new(),
        evidence_coverage: EvidenceCoverage::None,
        open_questions: Vec::new(),
    }
}

pub(super) fn build_completion_review(
    terminal: &TerminalProjection,
    started_at_ms: u64,
    completed_at_ms: u64,
    projection: &ProjectionState,
    changed_file_item_ids: BTreeSet<ItemId>,
    source_ids: BTreeSet<SourceId>,
    output_ids: BTreeSet<ArtifactId>,
) -> CompletionReview {
    let mut phases = BTreeMap::<ActivityPhase, ActivityPhaseSummary>::new();
    let mut verification_action_item_ids = Vec::new();
    let mut failed_action_item_ids = Vec::new();
    let mut warning_action_item_ids = Vec::new();
    let mut activities = projection
        .tool_items
        .iter()
        .filter_map(|(call_id, item_id)| {
            projection
                .tool_calls
                .get(call_id)
                .map(|tool| (item_id.clone(), tool.activity.clone()))
        })
        .collect::<Vec<_>>();
    activities.sort_by(|left, right| {
        left.1
            .started_at_ms
            .cmp(&right.1.started_at_ms)
            .then_with(|| left.0.as_str().cmp(right.0.as_str()))
    });
    for (item_id, activity) in &activities {
        let phase = phases
            .entry(activity.phase)
            .or_insert(ActivityPhaseSummary {
                phase: activity.phase,
                action_count: 0,
                succeeded_count: 0,
                failed_count: 0,
                stopped_count: 0,
            });
        phase.action_count = phase.action_count.saturating_add(1);
        match activity.status {
            ToolActivityStatus::Succeeded => {
                phase.succeeded_count = phase.succeeded_count.saturating_add(1)
            }
            ToolActivityStatus::Failed => {
                phase.failed_count = phase.failed_count.saturating_add(1);
                failed_action_item_ids.push(item_id.clone());
                if terminal.outcome == octet_serve_backend::RunOutcome::Completed {
                    warning_action_item_ids.push(item_id.clone());
                }
            }
            ToolActivityStatus::Stopped | ToolActivityStatus::Running => {
                phase.stopped_count = phase.stopped_count.saturating_add(1)
            }
        }
        if activity.phase == ActivityPhase::Verified {
            verification_action_item_ids.push(item_id.clone());
        }
    }
    let phase_summaries = phases.into_values().collect::<Vec<_>>();
    let changed_file_item_ids = changed_file_item_ids.into_iter().collect::<Vec<_>>();
    let source_ids = source_ids.into_iter().collect::<Vec<_>>();
    let output_ids = output_ids.into_iter().collect::<Vec<_>>();
    let action_count = activities.len().min(u32::MAX as usize) as u32;
    let has_unbounded_mutator = activities
        .iter()
        .any(|(_, activity)| matches!(activity.kind, ToolKind::Command | ToolKind::Other));
    let linked_evidence =
        !changed_file_item_ids.is_empty() || !source_ids.is_empty() || !output_ids.is_empty();
    let evidence_coverage = if has_unbounded_mutator {
        EvidenceCoverage::Partial
    } else if !linked_evidence {
        EvidenceCoverage::None
    } else if activities.iter().all(|(_, activity)| match activity.kind {
        ToolKind::Read | ToolKind::Web | ToolKind::Skill => !activity.source_ids.is_empty(),
        ToolKind::Edit | ToolKind::Write => {
            activity.status != ToolActivityStatus::Succeeded || !activity.changed_paths.is_empty()
        }
        ToolKind::Search => false,
        ToolKind::Command | ToolKind::Other => false,
    }) {
        EvidenceCoverage::Complete
    } else {
        EvidenceCoverage::Partial
    };
    let summary = format!(
        "{} {} action{}, {} changed file{}, {} verification{}, {} failure{}, {} warning{}, and {} output{}.",
        match terminal.outcome {
            octet_serve_backend::RunOutcome::Completed => "Completed",
            octet_serve_backend::RunOutcome::Stopped => "Stopped after",
            octet_serve_backend::RunOutcome::Failed => "Failed after",
        },
        action_count,
        if action_count == 1 { "" } else { "s" },
        changed_file_item_ids.len(),
        if changed_file_item_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        verification_action_item_ids.len(),
        if verification_action_item_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        failed_action_item_ids.len(),
        if failed_action_item_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        warning_action_item_ids.len(),
        if warning_action_item_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        output_ids.len(),
        if output_ids.len() == 1 { "" } else { "s" },
    );
    let summary = if projection.usage_uncertain {
        format!("Provider usage and cost are uncertain; numeric usage values are known subtotals, not complete totals. {summary}")
    } else {
        summary
    };
    CompletionReview {
        summary: bounded_text(&summary, 2 * 1024),
        duration_ms: completed_at_ms.saturating_sub(started_at_ms),
        action_count,
        phases: phase_summaries,
        changed_file_item_ids,
        verification_action_item_ids,
        failed_action_item_ids,
        warning_action_item_ids,
        source_ids,
        output_ids,
        test_results: projection.test_results.clone(),
        evidence_coverage,
        // The adapter cannot infer unresolved questions from prose safely.
        open_questions: Vec::new(),
    }
}

// Keep the immutable run identity, timing, projection, and review inputs explicit
// at the one durable serialization boundary.
#[allow(clippy::too_many_arguments)]
pub(super) fn persist_run_projection(
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    run_id: &RunId,
    started_at_ms: u64,
    completed_at_ms: u64,
    projection: &ProjectionState,
    committed: &[SessionItem],
    review: &CompletionReview,
) -> Result<(), ServiceError> {
    let outcome_entry_id = committed
        .iter()
        .find_map(|item| {
            matches!(&item.payload, ItemPayload::RunOutcome { .. })
                .then(|| item.durable_entry_id.clone())
                .flatten()
        })
        .ok_or(ServiceError::Internal)?;
    let fallback_turn = projection.turn_id(run_id)?;
    let mut ordinals = HashMap::<String, u32>::new();
    let mut items = Vec::with_capacity(committed.len());
    for item in committed {
        let Some(durable_entry_id) = item.durable_entry_id.as_ref() else {
            continue;
        };
        let ordinal = ordinals
            .entry(durable_entry_id.as_str().to_owned())
            .or_default();
        items.push(StoredRunItemAttribution {
            durable_entry_id: durable_entry_id.as_str().to_owned(),
            ordinal: *ordinal,
            item_id: item.id.as_str().to_owned(),
            turn_id: item
                .turn_id
                .as_ref()
                .unwrap_or(&fallback_turn)
                .as_str()
                .to_owned(),
            user_delivery: match &item.payload {
                ItemPayload::UserMessage { delivery, .. } => *delivery,
                _ => None,
            },
            documents: match &item.payload {
                ItemPayload::UserMessage { documents, .. } => documents.clone(),
                _ => Vec::new(),
            },
            project_files: match &item.payload {
                ItemPayload::UserMessage { project_files, .. } => project_files.clone(),
                _ => Vec::new(),
            },
            branch_provenance: match &item.payload {
                ItemPayload::UserMessage {
                    branch_provenance, ..
                } => branch_provenance.clone(),
                _ => None,
            },
        });
        *ordinal = ordinal.saturating_add(1);
    }
    let mut tools = projection
        .tool_items
        .iter()
        .filter_map(|(tool_call_id, item_id)| {
            let tool = projection.tool_calls.get(tool_call_id)?;
            Some(StoredRunTool {
                tool_call_id: tool_call_id.clone(),
                item_id: item_id.as_str().to_owned(),
                turn_id: tool.turn_id.as_str().to_owned(),
                activity: tool.activity.clone(),
                result: tool.result.clone(),
            })
        })
        .collect::<Vec<_>>();
    tools.sort_by(|left, right| {
        left.activity
            .started_at_ms
            .cmp(&right.activity.started_at_ms)
            .then_with(|| left.item_id.cmp(&right.item_id))
    });
    let record = StoredRunRecord {
        version: STORED_RUN_RECORD_VERSION,
        session_id: session_id.as_str().to_owned(),
        run_id: run_id.as_str().to_owned(),
        outcome_entry_id: outcome_entry_id.as_str().to_owned(),
        started_at_ms,
        completed_at_ms,
        items,
        tools,
        review: review.clone(),
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| ServiceError::Internal)?;
    resources
        .persist_run_record(session_id, &outcome_entry_id, &bytes)
        .map_err(resource_store_service_error)
}

pub(super) fn load_stored_run_record(
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    outcome_entry_id: &DurableEntryId,
) -> Option<StoredRunRecord> {
    let bytes = resources.run_record(session_id, outcome_entry_id).ok()?;
    let record = serde_json::from_slice::<StoredRunRecord>(&bytes).ok()?;
    if record.version != STORED_RUN_RECORD_VERSION
        || record.session_id != session_id.as_str()
        || record.outcome_entry_id != outcome_entry_id.as_str()
        || record.started_at_ms == 0
        || record.completed_at_ms < record.started_at_ms
        || RunId::new(record.run_id.clone()).is_err()
        || record.review.validate().is_err()
    {
        return None;
    }
    for item in &record.items {
        if DurableEntryId::new(item.durable_entry_id.clone()).is_err()
            || ItemId::new(item.item_id.clone()).is_err()
            || TurnId::new(item.turn_id.clone()).is_err()
            || (ItemPayload::UserMessage {
                text: String::new(),
                attachments: Vec::new(),
                documents: item.documents.clone(),
                project_files: item.project_files.clone(),
                delivery: item.user_delivery,
                branch_provenance: item.branch_provenance.clone(),
            })
            .validate()
            .is_err()
        {
            return None;
        }
    }
    for tool in &record.tools {
        if tool.tool_call_id.len() > 512
            || ItemId::new(tool.item_id.clone()).is_err()
            || TurnId::new(tool.turn_id.clone()).is_err()
            || tool.activity.validate().is_err()
            || tool
                .result
                .as_ref()
                .is_some_and(|result| result.validate().is_err())
        {
            return None;
        }
    }
    Some(record)
}
