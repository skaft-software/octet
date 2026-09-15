#![allow(missing_docs)]

//! Run lifecycle projection and request-scoped presentation state.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use octet_agent::{AgentEvent, FinishReason};
use octet_ai::{AssistantPart, ProviderLifecycleState};

use super::changed_files::{
    output_contains_hash_token, project_candidates, reported_output_hash, ChangedFileCandidate,
    WorkspaceSnapshot,
};
use super::request::{RequestThroughput, RequestThroughputTracker, RequestTimingSample};
use super::tool_display::{summarize_tool_with_workspace, tool_result_is_failure};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RunId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    pub files_changed: usize,
    pub tool_calls: usize,
    pub warnings: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Completed {
        elapsed: Duration,
        summary: RunSummary,
    },
    CompletedWithWarnings {
        elapsed: Duration,
        warnings: usize,
        summary: RunSummary,
    },
    Failed {
        elapsed: Duration,
        reason: String,
    },
    Interrupted {
        elapsed: Duration,
    },
    NeedsInput {
        prompt: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunPhase {
    Preparing {
        summary: String,
    },
    AwaitingProvider {
        provider: String,
    },
    /// An opted-in HTTP endpoint is preparing a cold model before generation.
    ProviderLifecycle {
        provider: String,
        state: ProviderLifecycleState,
        detail: Option<String>,
    },
    Thinking,
    StreamingResponse,
    PreparingToolCall,
    RunningTool {
        summary: String,
    },
    #[allow(dead_code)]
    AwaitingApproval {
        prompt: String,
    },
    Finished(RunOutcome),
}

impl RunPhase {
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Finished(_))
    }
}

#[derive(Clone, Debug)]
struct TrackedTool {
    name: String,
    args: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct RunPresentation {
    id: RunId,
    provider: String,
    endpoint: String,
    model: String,
    started_at: Instant,
    phase_started_at: Instant,
    phase: RunPhase,
    pending_tools: BTreeSet<String>,
    tools: HashMap<String, TrackedTool>,
    changed_files: BTreeSet<String>,
    changed_candidates: HashMap<String, ChangedFileCandidate>,
    workspace_before: Option<WorkspaceSnapshot>,
    workspace_after: Option<WorkspaceSnapshot>,
    request: RequestThroughputTracker,
    tool_calls: usize,
    warnings: usize,
    usage_uncertain: bool,
}

impl RunPresentation {
    pub fn id(&self) -> RunId {
        self.id
    }

    pub fn phase(&self) -> &RunPhase {
        &self.phase
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn is_active(&self) -> bool {
        self.phase.is_active()
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn phase_elapsed_at(&self, now: Instant) -> Duration {
        match &self.phase {
            RunPhase::Finished(_) => Duration::ZERO,
            _ => now.saturating_duration_since(self.phase_started_at),
        }
    }

    /// Total run time. Terminal outcomes carry their frozen elapsed value, so
    /// completion telemetry can never resume ticking after the run settles.
    pub fn elapsed_at(&self, now: Instant) -> Duration {
        match &self.phase {
            RunPhase::Finished(RunOutcome::Completed { elapsed, .. })
            | RunPhase::Finished(RunOutcome::CompletedWithWarnings { elapsed, .. })
            | RunPhase::Finished(RunOutcome::Failed { elapsed, .. })
            | RunPhase::Finished(RunOutcome::Interrupted { elapsed }) => *elapsed,
            RunPhase::Finished(RunOutcome::NeedsInput { .. }) => self
                .phase_started_at
                .saturating_duration_since(self.started_at),
            _ => now.saturating_duration_since(self.started_at),
        }
    }

    pub fn changed_files(&self) -> &BTreeSet<String> {
        &self.changed_files
    }

    pub fn usage_uncertain(&self) -> bool {
        self.usage_uncertain
    }

    pub fn request_throughput(&self) -> Option<&RequestThroughput> {
        self.request.latest()
    }

    pub fn request_timing(&self) -> Option<RequestTimingSample> {
        self.request.latest_timing()
    }

    pub fn active_request_timing(&self) -> Option<RequestTimingSample> {
        self.request.active().map(|timing| timing.sample())
    }

    fn summary(&self) -> RunSummary {
        RunSummary {
            files_changed: self.changed_files.len(),
            tool_calls: self.tool_calls,
            warnings: self.warnings,
        }
    }

    fn transition(&mut self, phase: RunPhase, now: Instant) {
        if self.phase == phase {
            return;
        }
        self.phase = phase;
        self.phase_started_at = now;
    }

    fn finish(&mut self, outcome: RunOutcome, now: Instant) -> Option<RunOutcome> {
        if !self.is_active() {
            return None;
        }
        self.phase = RunPhase::Finished(outcome.clone());
        self.phase_started_at = now;
        Some(outcome)
    }

    fn reproject_changed_files(&mut self) {
        self.changed_files = project_candidates(
            self.workspace_before.as_ref(),
            self.workspace_after.as_ref(),
            self.changed_candidates.values(),
        );
    }
}

#[derive(Clone, Debug, Default)]
pub struct RunTracker {
    next_id: u64,
    current: Option<RunPresentation>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunUpdate {
    pub accepted: bool,
    pub outcome: Option<RunOutcome>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl RunTracker {
    pub fn current(&self) -> Option<&RunPresentation> {
        self.current.as_ref()
    }

    pub fn current_id(&self) -> Option<RunId> {
        self.current.as_ref().map(RunPresentation::id)
    }

    pub fn is_active(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(RunPresentation::is_active)
    }

    /// Clear session-local presentation without reusing an earlier run ID.
    pub fn clear(&mut self) {
        self.current = None;
    }

    pub fn begin_at(
        &mut self,
        provider: impl Into<String>,
        now: Instant,
    ) -> Result<RunId, &'static str> {
        let provider = provider.into();
        self.begin_route_at(provider.clone(), provider, "unknown", now)
    }

    pub fn begin_for_model(
        &mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<RunId, &'static str> {
        self.begin_for_model_at(provider, model, Instant::now())
    }

    pub fn begin_for_model_at(
        &mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
        now: Instant,
    ) -> Result<RunId, &'static str> {
        let provider = provider.into();
        self.begin_route_at(provider.clone(), provider, model, now)
    }

    pub fn begin_route(
        &mut self,
        provider: impl Into<String>,
        endpoint: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<RunId, &'static str> {
        self.begin_route_at(provider, endpoint, model, Instant::now())
    }

    fn begin_route_at(
        &mut self,
        provider: impl Into<String>,
        endpoint: impl Into<String>,
        model: impl Into<String>,
        now: Instant,
    ) -> Result<RunId, &'static str> {
        if self.is_active() {
            return Err("a run is already active");
        }
        self.next_id = self.next_id.saturating_add(1);
        let id = RunId(self.next_id);
        self.current = Some(RunPresentation {
            id,
            provider: provider.into(),
            endpoint: endpoint.into(),
            model: model.into(),
            started_at: now,
            phase_started_at: now,
            phase: RunPhase::Preparing {
                summary: "checking context".into(),
            },
            pending_tools: BTreeSet::new(),
            tools: HashMap::new(),
            changed_files: BTreeSet::new(),
            changed_candidates: HashMap::new(),
            workspace_before: None,
            workspace_after: None,
            request: RequestThroughputTracker::default(),
            tool_calls: 0,
            warnings: 0,
            usage_uncertain: false,
        });
        Ok(id)
    }

    pub fn set_preparing(&mut self, id: RunId, summary: impl Into<String>) -> bool {
        self.set_phase(
            id,
            RunPhase::Preparing {
                summary: summary.into(),
            },
        )
    }

    pub fn awaiting_provider(&mut self, id: RunId) -> bool {
        self.awaiting_provider_at(id, Instant::now())
    }

    pub fn awaiting_provider_at(&mut self, id: RunId, now: Instant) -> bool {
        let Some(run) = self.active_mut(id) else {
            return false;
        };
        let provider = run.provider.clone();
        run.transition(RunPhase::AwaitingProvider { provider }, now);
        true
    }

    pub fn awaiting_approval(&mut self, id: RunId, prompt: impl Into<String>) -> bool {
        self.set_phase(
            id,
            RunPhase::AwaitingApproval {
                prompt: prompt.into(),
            },
        )
    }

    pub fn set_phase(&mut self, id: RunId, phase: RunPhase) -> bool {
        self.set_phase_at(id, phase, Instant::now())
    }

    pub fn set_phase_at(&mut self, id: RunId, phase: RunPhase, now: Instant) -> bool {
        if matches!(phase, RunPhase::Finished(_)) {
            return false;
        }
        let Some(run) = self.active_mut(id) else {
            return false;
        };
        run.transition(phase, now);
        true
    }

    /// Attach actual pre/post workspace snapshots. Changed-file counts remain
    /// empty until both snapshots validate a completed mutation candidate.
    pub fn set_workspace_snapshots(
        &mut self,
        id: RunId,
        before: WorkspaceSnapshot,
        after: WorkspaceSnapshot,
    ) -> bool {
        let Some(run) = self.current_mut(id) else {
            return false;
        };
        run.workspace_before = Some(before);
        run.workspace_after = Some(after);
        run.reproject_changed_files();
        true
    }

    pub fn interrupt(&mut self, id: RunId) -> Option<RunOutcome> {
        self.interrupt_at(id, Instant::now())
    }

    pub fn interrupt_at(&mut self, id: RunId, now: Instant) -> Option<RunOutcome> {
        let run = self.active_mut(id)?;
        run.request.abort_attempt();
        let elapsed = now.saturating_duration_since(run.started_at);
        run.finish(RunOutcome::Interrupted { elapsed }, now)
    }

    pub fn fail(&mut self, id: RunId, reason: impl Into<String>) -> Option<RunOutcome> {
        self.fail_at(id, reason, Instant::now())
    }

    pub fn fail_at(
        &mut self,
        id: RunId,
        reason: impl Into<String>,
        now: Instant,
    ) -> Option<RunOutcome> {
        let run = self.active_mut(id)?;
        run.request.abort_attempt();
        let elapsed = now.saturating_duration_since(run.started_at);
        run.finish(
            RunOutcome::Failed {
                elapsed,
                reason: reason.into(),
            },
            now,
        )
    }

    pub fn needs_input(&mut self, id: RunId, prompt: impl Into<String>) -> Option<RunOutcome> {
        self.needs_input_at(id, prompt, Instant::now())
    }

    pub fn needs_input_at(
        &mut self,
        id: RunId,
        prompt: impl Into<String>,
        now: Instant,
    ) -> Option<RunOutcome> {
        let run = self.active_mut(id)?;
        run.request.abort_attempt();
        run.finish(
            RunOutcome::NeedsInput {
                prompt: prompt.into(),
            },
            now,
        )
    }

    /// Record a request submission before its stream-opening event. The first
    /// provider event remains unset until an actual provider event is applied.
    pub fn request_submitted_at(&mut self, id: RunId, now: Instant) -> bool {
        let Some(run) = self.active_mut(id) else {
            return false;
        };
        if run.request.active().is_none() {
            run.request.begin_at(now);
        }
        true
    }

    pub fn apply_event(&mut self, id: RunId, event: &AgentEvent) -> RunUpdate {
        self.apply_event_at(id, event, Instant::now())
    }

    pub fn apply_event_at(&mut self, id: RunId, event: &AgentEvent, now: Instant) -> RunUpdate {
        let Some(run) = self.active_mut(id) else {
            return RunUpdate::default();
        };

        let mut outcome = None;
        match event {
            AgentEvent::OutputDelta { channel, text } => {
                // Submission is an explicit handoff from the provider/agent
                // owner. Without it, event receipt must not be used as a
                // fabricated request origin.
                if run.request.active().is_some() {
                    run.request.provider_event_at(now);
                    if !text.is_empty() {
                        run.request.generated_at(now);
                    }
                }
                let phase = match channel {
                    octet_agent::OutputChannel::Reasoning => RunPhase::Thinking,
                    octet_agent::OutputChannel::Text => RunPhase::StreamingResponse,
                };
                run.transition(phase, now);
            }
            AgentEvent::OutputMedia { .. } => {
                if run.request.active().is_some() {
                    run.request.provider_event_at(now);
                    run.request.generated_at(now);
                }
                run.transition(RunPhase::StreamingResponse, now);
            }
            AgentEvent::ProviderLifecycle { lifecycle } => {
                if run.request.active().is_some() {
                    run.request.provider_event_at(now);
                }
                // Readiness feedback is useful only before model output or
                // tool work begins. Never let a delayed advisory comment move
                // a visible response back into a waiting state.
                if matches!(
                    &run.phase,
                    RunPhase::AwaitingProvider { .. } | RunPhase::ProviderLifecycle { .. }
                ) {
                    let provider = run.provider.clone();
                    run.transition(
                        RunPhase::ProviderLifecycle {
                            provider,
                            state: lifecycle.state,
                            detail: lifecycle.detail.clone(),
                        },
                        now,
                    );
                }
            }
            // Auxiliary recovery belongs to the operation already in progress;
            // it must not reset the main answer or its compaction phase.
            AgentEvent::ProviderOperationRetry { .. } => {}
            AgentEvent::ProviderUsageUncertain => {
                if !run.usage_uncertain {
                    run.usage_uncertain = true;
                    run.warnings = run.warnings.saturating_add(1);
                }
            }
            AgentEvent::ProviderRetry { .. } | AgentEvent::ProviderWaitingForNetwork { .. } => {
                // The physical attempt is no longer usable. Backoff starts
                // outside the next request's timing interval.
                run.request.abort_attempt();
                let provider = run.provider.clone();
                run.transition(RunPhase::AwaitingProvider { provider }, now);
            }
            AgentEvent::CandidateRejected { .. } => {
                // A completed candidate can have a latest rate, but it was not
                // accepted. Do not expose it as the next answer's throughput.
                run.request.clear_latest();
                let provider = run.provider.clone();
                run.transition(RunPhase::AwaitingProvider { provider }, now);
            }
            AgentEvent::SteeringDelivered { .. } | AgentEvent::FollowUpDelivered { .. } => {
                let provider = run.provider.clone();
                run.transition(RunPhase::AwaitingProvider { provider }, now);
            }
            AgentEvent::CompactionStarted { .. } => {
                run.transition(
                    RunPhase::Preparing {
                        summary: "compacting".into(),
                    },
                    now,
                );
            }
            AgentEvent::CompactionFinished { .. } => {
                let provider = run.provider.clone();
                run.transition(RunPhase::AwaitingProvider { provider }, now);
            }
            AgentEvent::TurnFinished {
                message,
                turn_usage,
                ..
            } => {
                run.pending_tools.clear();
                let mut has_tool_arguments = false;
                for part in &message.content {
                    if let AssistantPart::ToolCall(call) = part {
                        has_tool_arguments = true;
                        run.pending_tools.insert(call.id.0.clone());
                    }
                }
                // Raw tool-argument deltas are not exposed as events. The
                // assembled ToolCall is the authoritative representable marker.
                if has_tool_arguments && run.request.active().is_some() {
                    // Assembled tool arguments are generation activity when a
                    // request origin was supplied by the provider owner.
                    run.request.provider_event_at(now);
                    run.request.generated_at(now);
                }
                let _ = run.request.finish_at(turn_usage.output_tokens, now);
                // TurnFinished follows the durable assistant/usage write. Keep
                // commit accounting separate even when both observations share
                // one event timestamp.
                run.request.commit_at(now);
                if !run.pending_tools.is_empty() {
                    run.transition(RunPhase::PreparingToolCall, now);
                }
            }
            AgentEvent::ToolStarted { id, name, args } => {
                run.tool_calls = run.tool_calls.saturating_add(1);
                run.pending_tools.insert(id.0.clone());
                run.tools.insert(
                    id.0.clone(),
                    TrackedTool {
                        name: name.clone(),
                        args: args.clone(),
                    },
                );
                let workspace = run.workspace_before.as_ref().map(WorkspaceSnapshot::root);
                run.transition(
                    RunPhase::RunningTool {
                        // The pinned status line has no workspace context, so
                        // keep its tool summary concise rather than leaking a
                        // host-specific absolute path.
                        summary: summarize_tool_with_workspace(name, args, workspace)
                            .compact_active,
                    },
                    now,
                );
            }
            // Policy diagnostics are emitted to telemetry and the host
            // protocol; they do not alter the interactive phase machine.
            AgentEvent::ToolPolicyDecision { .. } | AgentEvent::ToolProgress { .. } => {}
            AgentEvent::DelegationUpdated { .. } => {}
            AgentEvent::ToolFinished { id, result, .. } => {
                run.pending_tools.remove(&id.0);
                if let Some(tool) = run.tools.get(&id.0) {
                    if tool_result_is_failure(&tool.name, result) {
                        run.warnings = run.warnings.saturating_add(1);
                    } else if matches!(tool.name.as_str(), "edit" | "write") {
                        if let Some(path) = tool.args.get("path").and_then(|value| value.as_str()) {
                            let output = result.as_ref().ok();
                            let reported_hash = output
                                .filter(|output| !output.is_error())
                                .and_then(|output| {
                                    if output_contains_hash_token(&output.text) {
                                        reported_output_hash(&output.text)
                                    } else {
                                        None
                                    }
                                });
                            run.changed_candidates.insert(
                                id.0.clone(),
                                ChangedFileCandidate {
                                    path: path.to_owned(),
                                    reported_hash,
                                },
                            );
                            run.reproject_changed_files();
                        }
                    }
                } else if result.is_err() || result.as_ref().is_ok_and(|output| output.is_error()) {
                    run.warnings = run.warnings.saturating_add(1);
                }
                if run.pending_tools.is_empty() {
                    let provider = run.provider.clone();
                    run.transition(RunPhase::AwaitingProvider { provider }, now);
                } else {
                    run.transition(RunPhase::PreparingToolCall, now);
                }
            }
            AgentEvent::TurnStarted => {
                // The provider/agent owner must call `request_submitted_at`
                // before this stream marker. A stream-open timestamp is not a
                // substitute for the request submission origin.
                let _ = run.request.stream_opened_at(now);
            }
            AgentEvent::RunFinished { reason, .. } => {
                // A normal run has already finished its request at TurnFinished;
                // an early terminal event must discard any provisional attempt.
                run.request.abort_attempt();
                let elapsed = now.saturating_duration_since(run.started_at);
                let terminal = match reason {
                    FinishReason::Completed if run.warnings > 0 => {
                        RunOutcome::CompletedWithWarnings {
                            elapsed,
                            warnings: run.warnings,
                            summary: run.summary(),
                        }
                    }
                    FinishReason::Completed => RunOutcome::Completed {
                        elapsed,
                        summary: run.summary(),
                    },
                    FinishReason::Aborted => RunOutcome::Interrupted { elapsed },
                    FinishReason::Failed(error) => RunOutcome::Failed {
                        elapsed,
                        reason: octet_agent::public_error_diagnostic(
                            error,
                            &run.endpoint,
                            &run.model,
                        ),
                    },
                    FinishReason::MaxTurns => RunOutcome::Failed {
                        elapsed,
                        reason: "maximum turns reached".into(),
                    },
                };
                outcome = run.finish(terminal, now);
            }
        }

        RunUpdate {
            accepted: true,
            outcome,
        }
    }

    fn current_mut(&mut self, id: RunId) -> Option<&mut RunPresentation> {
        self.current.as_mut().filter(|run| run.id == id)
    }

    fn active_mut(&mut self, id: RunId) -> Option<&mut RunPresentation> {
        self.current_mut(id).filter(|run| run.is_active())
    }
}
