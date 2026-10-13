//! DelegationManager telemetry and span context.

use super::*;

impl DelegationManager {
    pub(super) fn attach_telemetry(&self) -> watch::Receiver<Option<DelegationTelemetrySnapshot>> {
        let (sender, receiver) = watch::channel(None);
        {
            let mut telemetry = self
                .telemetry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            telemetry.sender = Some(sender);
        }
        self.publish_telemetry(None, None);
        receiver
    }

    pub(super) fn publish_external_failure(&self, class: &str, reason: &str) {
        self.publish_telemetry(
            Some(class.to_owned()),
            Some(bounded_text_to(reason, MAX_TELEMETRY_FAILURE_BYTES)),
        );
    }

    pub(super) fn publish_telemetry(
        &self,
        failure_class: Option<String>,
        failure_reason: Option<String>,
    ) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let captured_at_ms = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
        let children = state
            .records
            .values()
            .map(|record| {
                let elapsed_end = record.completed_at_ms.unwrap_or(captured_at_ms);
                let started = record.started_at_ms.unwrap_or(record.created_at_ms);
                let (failure_class_for_child, failure_reason_for_child) = match &record.status {
                    DelegatedAgentStatus::Failed { error } => (
                        Some(classify_delegation_failure(error).to_owned()),
                        Some(bounded_text_to(error, MAX_TELEMETRY_FAILURE_BYTES)),
                    ),
                    DelegatedAgentStatus::TimedOut => (
                        Some("timeout".to_owned()),
                        Some("worker exceeded its host-owned wall-time deadline".to_owned()),
                    ),
                    DelegatedAgentStatus::Interrupted => (
                        Some("cancellation".to_owned()),
                        Some("worker was interrupted by the owner".to_owned()),
                    ),
                    DelegatedAgentStatus::LimitReached {
                        turn_count,
                        turn_limit,
                        ..
                    } => (
                        Some("limit".to_owned()),
                        Some(format!(
                            "worker exhausted its turn budget ({turn_count}/{turn_limit} turns)"
                        )),
                    ),
                    DelegatedAgentStatus::Shutdown => (
                        Some("cancellation".to_owned()),
                        Some("worker was shut down by its owning run".to_owned()),
                    ),
                    DelegatedAgentStatus::AwaitingApproval { reason } => (
                        Some("approval".to_owned()),
                        Some(bounded_text_to(reason, MAX_TELEMETRY_FAILURE_BYTES)),
                    ),
                    DelegatedAgentStatus::Detached => (
                        Some("detached".to_owned()),
                        Some("worker outlived its owning run and awaits reattachment".to_owned()),
                    ),
                    _ => (None, None),
                };
                DelegationTelemetryChild {
                    child_id: record.identity.id.clone(),
                    task_name: record
                        .display_task_name
                        .clone()
                        .unwrap_or_else(|| record.task_name.clone()),
                    profile: record.extension_profile.clone(),
                    model: record
                        .extension_policy
                        .as_ref()
                        .and_then(|p| p.resolved_model.as_ref())
                        .map(|m| m.model.clone())
                        .unwrap_or_else(|| self.template.model.spec.id.0.clone()),
                    state: record.status.label().to_owned(),
                    phase: if !record.active_tools.is_empty() {
                        "using_tool".to_owned()
                    } else {
                        match &record.status {
                            DelegatedAgentStatus::Pending => "queued",
                            DelegatedAgentStatus::Idle => "idle",
                            DelegatedAgentStatus::Running => "thinking",
                            DelegatedAgentStatus::Completed { .. } => "completed",
                            DelegatedAgentStatus::LimitReached { .. } => "limit_reached",
                            DelegatedAgentStatus::Interrupted => "interrupted",
                            DelegatedAgentStatus::Failed { .. } => "failed",
                            DelegatedAgentStatus::TimedOut => "timed_out",
                            DelegatedAgentStatus::Detached => "detached",
                            DelegatedAgentStatus::AwaitingApproval { .. } => "awaiting_approval",
                            DelegatedAgentStatus::Shutdown => "shutdown",
                        }
                        .to_owned()
                    },
                    current_tool: record.active_tools.values().next_back().cloned(),
                    tool_use_count: record.tool_call_count,
                    input_tokens: record.usage.input_tokens,
                    cache_read_tokens: record.usage.cache_read_tokens,
                    cache_write_tokens: record.usage.cache_write_tokens,
                    output_tokens: record.usage.output_tokens,
                    estimated_output_tokens: (record.streamed_output_bytes > 0).then(|| {
                        record
                            .usage
                            .output_tokens
                            .saturating_add(record.streamed_output_bytes.div_ceil(4))
                    }),
                    reasoning_tokens: record.usage.reasoning_tokens,
                    total_tokens: record.usage.total_tokens,
                    cost: if record.usage_uncertain {
                        None
                    } else {
                        record.cost
                    },
                    cost_microdollars: record.cost_microdollars,
                    elapsed_ms: elapsed_end.saturating_sub(started),
                    failure_class: failure_class_for_child,
                    failure_reason: failure_reason_for_child,
                    effective_tool_policy: record.effective_tool_policy.clone(),
                    orchestration_provenance: record.orchestration_provenance.clone(),
                    session: delegated_session_reference(&record.session_path),
                }
            })
            .collect::<Vec<_>>();

        let total_cost_microdollars = children
            .iter()
            .map(|child| child.cost_microdollars)
            .try_fold(0u64, |total, cost| Some(total.saturating_add(cost?)));
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        telemetry.revision = telemetry.revision.saturating_add(1);
        let snapshot = DelegationTelemetrySnapshot {
            revision: telemetry.revision,
            captured_at_ms,
            children,
            total_cost_microdollars,
            failure_reason,
            failure_class,
        };
        if let Some(sender) = telemetry.sender.clone() {
            if sender.send(Some(snapshot)).is_err() {
                telemetry.sender = None;
            }
        }
        // Keep the state snapshot and telemetry revision/send in one ordered
        // critical section. Otherwise an older captured roster can be
        // published after a newer one and become the watch channel's latest.
        drop(telemetry);
        drop(state);
    }

    pub(super) fn detach_telemetry(&self) {
        self.telemetry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sender = None;
    }

    /// Installs the owning agent's explicit span observer for child runs.
    pub(crate) fn set_span_context(&self, context: TelemetryContext) {
        *self
            .span_context
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = context;
    }

    /// Returns the installed span observer (inert by default).
    pub(super) fn span_context(&self) -> TelemetryContext {
        self.span_context
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}
