//! Cost-aware prompt-cache keepalive, following pi 1.0's request-replay contract.
//!
//! The session owner polls this state machine alongside ordinary inference/tools
//! or idle input. It never spawns a second session writer, executes generated
//! tools, or adds the refresh's input/output to model-visible history.

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use futures_util::StreamExt;
use octet_ai::{
    AiClient, AiError, CacheRetention, Model, Protocol, ReasoningConfig, Request, Response,
    StreamEvent, Usage, PICODOLLARS_PER_MICRODOLLAR,
};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::{
    agent::budget::{request_uncertainty_bound, reserve_request_cost, reserve_request_tokens},
    events::AgentEvent,
    extension::{CacheWarmingDecisionContext, CacheWarmingDecisionHook},
    session::{
        now_unix_millis, CacheWarmRecord, CacheWarmState, EntryId, EntryValue, Session,
        SessionError, UsageUncertaintyBound,
    },
};

const STREAMING_HORIZON: Duration = Duration::from_secs(60 * 60);
const IDLE_HORIZON: Duration = Duration::from_secs(30 * 60);
const MINIMUM_SAVINGS_MICRODOLLARS: i128 = 50_000;
// A hook is advisory, not authority to keep an expired entry alive.
const HOOK_BUDGET: Duration = Duration::from_millis(200);
const REQUEST_DEADLINE: Duration = Duration::from_secs(30);

type Pending<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Warming profile. Idle includes warming during active runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheWarmMode {
    /// No additional provider requests.
    Off,
    /// Keep valuable caches alive during long runs (pi's default).
    #[default]
    Streaming,
    /// Also keep valuable caches alive between runs.
    Idle,
}

/// Coalesced mode changes retain stop epochs so off -> idle cannot revive an
/// old prefix, nor streaming -> idle revive an already-idle prefix.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CacheWarmPolicy {
    pub(crate) mode: CacheWarmMode,
    off_epoch: u64,
    streaming_epoch: u64,
}

impl CacheWarmPolicy {
    pub(crate) fn set_mode(&mut self, mode: CacheWarmMode) {
        self.mode = mode;
        if mode == CacheWarmMode::Off {
            self.off_epoch += 1;
        } else if mode == CacheWarmMode::Streaming {
            self.streaming_epoch += 1;
        }
    }
}

/// Whether the owning real request's run is still active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheWarmingPhase {
    /// A run is still executing inference or tools.
    Streaming,
    /// The run has settled and the host is waiting for input.
    Idle,
}

/// Cost policy result, or an extension's override of that result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheWarmingAction {
    /// Send one refresh, subject to host admission.
    Warm,
    /// Stop until the next real request.
    Stop,
}

/// Inputs to pi's expected-savings decision, using Octet's exact monetary units.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheWarmingDecision {
    /// Running versus settled phase.
    pub phase: CacheWarmingPhase,
    /// Cache-read price for the prompt plus one output token.
    pub warm_cost_microdollars: u64,
    /// Extra price of losing the next real request's cache hit.
    pub miss_cost_microdollars: u64,
    /// 1.0 while running, 0.15 while idle; not used for money arithmetic.
    pub continuation_probability: f64,
    /// Expected avoided miss cost less refresh cost.
    pub expected_savings_microdollars: i128,
    /// Whether prompt size and positive prices are known.
    pub economics_available: bool,
    /// Warm only when expected savings are at least $0.05.
    pub action: CacheWarmingAction,
}

/// Current transient cache-warmer state (never provider output).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheWarmingState {
    /// No timer or provider request is active.
    Inactive,
    /// Waiting for the next cost decision.
    Scheduled,
    /// Deciding or refreshing an eligible cache entry.
    Refreshing,
}

/// Frontend-neutral diagnostics for the next decision or last stop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheWarmingStatus {
    /// Timer/request state.
    pub state: CacheWarmingState,
    /// Reason the scheduler stopped, if any.
    pub reason: Option<String>,
    /// Next decision's best-effort wall-clock timestamp.
    pub next_warm_at_unix_ms: Option<u64>,
    /// Current economics, or the decision that stopped warming.
    pub decision: Option<CacheWarmingDecision>,
    /// Whether an extension changed the cost policy's action.
    pub extension_override: bool,
}

impl CacheWarmingStatus {
    pub(crate) fn inactive(reason: &str) -> Self {
        Self {
            state: CacheWarmingState::Inactive,
            reason: Some(reason.into()),
            next_warm_at_unix_ms: None,
            decision: None,
            extension_override: false,
        }
    }
}

/// Refresh at 90% of the declared TTL, preserving at least ten seconds.
pub fn cache_warming_delay(ttl: Duration) -> Option<Duration> {
    let millis = u64::try_from(ttl.as_millis()).ok()?;
    (millis > 10_000).then(|| {
        Duration::from_millis(
            (millis / 10 * 9 + millis % 10 * 9 / 10)
                .min(millis - 10_000)
                .max(1),
        )
    })
}

/// Human-readable next-decision diagnostics for session/status frontends.
pub fn format_cache_warming_status(status: &CacheWarmingStatus) -> String {
    let Some(decision) = &status.decision else {
        return format!(
            "Inactive ({})",
            status.reason.as_deref().unwrap_or("unknown reason")
        );
    };
    let economics = if decision.economics_available {
        let probability = if decision.phase == CacheWarmingPhase::Idle {
            15
        } else {
            100
        };
        let savings = decision.expected_savings_microdollars;
        format!(
            "{}% continuation probability, expected savings {}${}.{:03} {} $0.050",
            probability,
            if savings < 0 { "-" } else { "" },
            savings.abs() / 1_000_000,
            savings.abs() % 1_000_000 / 1_000,
            if decision.action == CacheWarmingAction::Warm {
                ">="
            } else {
                "<"
            }
        )
    } else {
        "cache economics unavailable".into()
    };
    let details = format!(
        "{}{} -> {}",
        if status.extension_override {
            "extension override, "
        } else {
            ""
        },
        economics,
        if decision.action == CacheWarmingAction::Warm {
            "warm"
        } else {
            "stop"
        }
    );
    match status.state {
        CacheWarmingState::Inactive => format!(
            "Stopped ({}; {})",
            status.reason.as_deref().unwrap_or("inactive"),
            details
        ),
        CacheWarmingState::Refreshing => format!("Warming cache ({details})"),
        CacheWarmingState::Scheduled => {
            let seconds = status
                .next_warm_at_unix_ms
                .unwrap_or(0)
                .saturating_sub(now_unix_millis())
                .div_ceil(1_000);
            format!(
                "Decision in {}m {}s ({details})",
                seconds / 60,
                seconds % 60
            )
        }
    }
}

struct ActiveCache {
    model: Model,
    request: Request,
    anchor: Option<EntryId>,
    input_tokens: u64,
    tool_generation: u64,
    ttl: Duration,
    delay: Duration,
    started_at: Instant,
    phase: CacheWarmingPhase,
    next_at: Instant,
    refresh_deadline: Instant,
    next_wall_ms: u64,
    extension_override: bool,
    decision: Option<CacheWarmingDecision>,
}

impl ActiveCache {
    fn is_current(&self, session: &Session, tool_generation: u64) -> bool {
        if tool_generation != self.tool_generation {
            return false;
        }
        let mut cursor = session.head();
        while cursor != self.anchor {
            let Some(entry) = cursor.as_ref().and_then(|id| session.entry(id)) else {
                return false;
            };
            // Ancestry alone does not establish canonical context after a summary.
            if matches!(
                &entry.value,
                EntryValue::Compaction { .. } | EntryValue::ResponsesCompaction { .. }
            ) {
                return false;
            }
            cursor = entry.parent.clone();
        }
        true
    }

    fn horizon(&self) -> Duration {
        if self.phase == CacheWarmingPhase::Idle {
            IDLE_HORIZON
        } else {
            STREAMING_HORIZON
        }
    }

    fn limit_reason(&self) -> &'static str {
        if self.phase == CacheWarmingPhase::Idle {
            "30-minute idle safety limit reached"
        } else {
            "one-hour safety limit reached"
        }
    }
}

struct Flight {
    future: Pending<Result<Response, bool>>,
    client: AiClient,
    record: CacheWarmRecord,
    bound: Option<UsageUncertaintyBound>,
}

enum Work {
    Waiting,
    Deciding(Pending<CacheWarmingAction>),
    Refreshing(Box<Flight>),
}

/// Cancellation-safe scheduler, owned by the same agent that owns the session.
pub(crate) struct CacheWarmer {
    mode_tx: tokio::sync::watch::Sender<CacheWarmPolicy>,
    mode_rx: tokio::sync::watch::Receiver<CacheWarmPolicy>,
    seen_policy: CacheWarmPolicy,
    status_tx: tokio::sync::watch::Sender<CacheWarmingStatus>,
    active: Option<ActiveCache>,
    work: Work,
    inactive: CacheWarmingStatus,
}

impl Default for CacheWarmer {
    fn default() -> Self {
        let (mode_tx, mode_rx) = tokio::sync::watch::channel(CacheWarmPolicy::default());
        let inactive = CacheWarmingStatus::inactive("waiting for first request");
        let (status_tx, _) = tokio::sync::watch::channel(inactive.clone());
        Self {
            mode_tx,
            mode_rx,
            seen_policy: CacheWarmPolicy::default(),
            status_tx,
            active: None,
            work: Work::Waiting,
            inactive,
        }
    }
}

/// A completed stage of the stored timer/hook/provider future.
pub(crate) enum CacheWarmStep {
    Due,
    Decided(CacheWarmingAction),
    ModeChanged,
    Expired,
    Finished(Result<Box<Response>, bool>),
}

/// Hard host ceilings; an active real request retains its own reservation.
#[derive(Clone, Copy, Default)]
pub(crate) struct CacheWarmLimits {
    pub max_session_tokens: Option<u64>,
    pub max_session_cost_microdollars: Option<u64>,
    pub pending_request: Option<UsageUncertaintyBound>,
}

/// Single-writer admission/settlement boundary supplied by the agent run owner.
pub(crate) struct CacheWarmHost<'a> {
    pub session: &'a mut Session,
    pub client: &'a AiClient,
    pub hooks: &'a [Arc<dyn CacheWarmingDecisionHook>],
    pub resource_owner: &'a str,
    pub tool_generation: u64,
    pub limits: CacheWarmLimits,
}

impl CacheWarmer {
    pub(crate) fn mode(&self) -> CacheWarmMode {
        self.mode_rx.borrow().mode
    }

    pub(crate) fn mode_control(&self) -> tokio::sync::watch::Sender<CacheWarmPolicy> {
        self.mode_tx.clone()
    }

    pub(crate) fn diagnostics(&self) -> tokio::sync::watch::Receiver<CacheWarmingStatus> {
        self.status_tx.subscribe()
    }

    fn publish_status(&self, session: &Session, tool_generation: u64) {
        self.status_tx
            .send_replace(self.status(session, tool_generation));
    }

    pub(crate) fn inherit_mode_control(
        &mut self,
        control: tokio::sync::watch::Sender<CacheWarmPolicy>,
    ) {
        self.seen_policy = *control.borrow();
        self.mode_rx = control.subscribe();
        self.mode_tx = control;
    }

    pub(crate) fn set_mode(
        &mut self,
        mode: CacheWarmMode,
        session: &mut Session,
    ) -> Result<(), SessionError> {
        self.mode_tx.send_modify(|policy| policy.set_mode(mode));
        self.reconcile_mode(session)
    }

    fn reconcile_mode(&mut self, session: &mut Session) -> Result<(), SessionError> {
        let policy = *self.mode_rx.borrow_and_update();
        let stopped = policy.off_epoch != self.seen_policy.off_epoch;
        let idle_stopped = policy.streaming_epoch != self.seen_policy.streaming_epoch;
        self.seen_policy = policy;
        if stopped || policy.mode == CacheWarmMode::Off {
            self.cancel(session, "cache warming disabled")?;
        } else if (idle_stopped || policy.mode == CacheWarmMode::Streaming)
            && self
                .active
                .as_ref()
                .is_some_and(|run| run.phase == CacheWarmingPhase::Idle)
        {
            self.cancel(session, "agent run settled")?;
        }
        Ok(())
    }

    pub(crate) fn status(&self, session: &Session, tool_generation: u64) -> CacheWarmingStatus {
        if self.mode() == CacheWarmMode::Off {
            return CacheWarmingStatus::inactive("cache warming disabled");
        }
        let Some(run) = &self.active else {
            return self.inactive.clone();
        };
        if !run.is_current(session, tool_generation) {
            return CacheWarmingStatus::inactive("conversation context changed");
        }
        let decision = evaluate(run, session);
        let state = if matches!(self.work, Work::Waiting) {
            CacheWarmingState::Scheduled
        } else {
            CacheWarmingState::Refreshing
        };
        if state == CacheWarmingState::Scheduled && !decision.economics_available {
            return CacheWarmingStatus::inactive("cache economics unavailable");
        }
        CacheWarmingStatus {
            state,
            reason: None,
            next_warm_at_unix_ms: Some(run.next_wall_ms),
            decision: Some(decision),
            extension_override: run.extension_override,
        }
    }

    pub(crate) fn start(
        &mut self,
        model: &Model,
        request: &Request,
        anchor: Option<EntryId>,
        input_tokens: u64,
        tool_generation: u64,
        session: &mut Session,
    ) -> Result<(), SessionError> {
        let result = self.start_inner(
            model,
            request,
            anchor,
            input_tokens,
            tool_generation,
            session,
        );
        self.publish_status(session, tool_generation);
        result
    }

    fn start_inner(
        &mut self,
        model: &Model,
        request: &Request,
        anchor: Option<EntryId>,
        input_tokens: u64,
        tool_generation: u64,
        session: &mut Session,
    ) -> Result<(), SessionError> {
        self.reconcile_mode(session)?;
        self.cancel(session, "waiting for first request")?;
        if self.mode() == CacheWarmMode::Off {
            self.inactive = CacheWarmingStatus::inactive("cache warming disabled");
            return Ok(());
        }
        if session
            .cache_warm_records()
            .last()
            .is_some_and(|record| record.state == CacheWarmState::Started)
        {
            // A prior process may have been billed. Do not erase its unknown
            // exposure or let auxiliary warming fail the next real run.
            self.inactive =
                CacheWarmingStatus::inactive("previous cache refresh remains unresolved");
            return Ok(());
        }
        if !is_replayable(model, request) {
            self.inactive = CacheWarmingStatus::inactive("request cannot be replayed safely");
            return Ok(());
        }
        let lifetimes = &model.spec.cache.prompt_cache;
        let seconds = match request.cache_retention {
            CacheRetention::None => {
                self.inactive = CacheWarmingStatus::inactive("request disabled prompt caching");
                return Ok(());
            }
            CacheRetention::Short => lifetimes.short,
            CacheRetention::Long if model.spec.cache.supports_long_retention => lifetimes.long,
            CacheRetention::Long => None,
        };
        let Some((ttl, delay)) = seconds.and_then(|seconds| {
            let ttl = Duration::from_secs(seconds);
            cache_warming_delay(ttl).map(|delay| (ttl, delay))
        }) else {
            self.inactive = CacheWarmingStatus::inactive("cache lifetime unavailable");
            return Ok(());
        };
        let now = Instant::now();
        self.active = Some(ActiveCache {
            model: model.clone(),
            request: request.clone(),
            anchor,
            input_tokens,
            tool_generation,
            ttl,
            delay,
            started_at: now,
            phase: CacheWarmingPhase::Streaming,
            next_at: now,
            refresh_deadline: now,
            next_wall_ms: 0,
            extension_override: false,
            decision: None,
        });
        self.schedule(session)
    }

    pub(crate) fn on_agent_settled(&mut self, session: &mut Session) -> Result<(), SessionError> {
        let generation = self.active.as_ref().map_or(0, |run| run.tool_generation);
        let result = self
            .reconcile_mode(session)
            .and_then(|()| self.on_agent_settled_inner(session));
        self.publish_status(session, generation);
        result
    }

    fn on_agent_settled_inner(&mut self, session: &mut Session) -> Result<(), SessionError> {
        if self.mode() != CacheWarmMode::Idle {
            return self.cancel(session, "agent run settled");
        }
        if let Some(run) = self.active.as_mut() {
            run.phase = CacheWarmingPhase::Idle;
            if run.next_at > run.started_at + IDLE_HORIZON
                || Instant::now() >= run.started_at + IDLE_HORIZON
            {
                return self.cancel(session, "30-minute idle safety limit reached");
            }
        }
        Ok(())
    }

    // Futures stay in Work: losing a select to input/control never restarts a
    // hook or drops an accepted POST. Cancellation settles it explicitly.
    pub(crate) async fn next_step(&mut self) -> CacheWarmStep {
        let next_work = async {
            let Some(run) = self.active.as_ref() else {
                return std::future::pending().await;
            };
            match &mut self.work {
                Work::Waiting => {
                    tokio::time::sleep_until(run.next_at).await;
                    CacheWarmStep::Due
                }
                Work::Deciding(future) => CacheWarmStep::Decided(future.await),
                Work::Refreshing(flight)
                    if !flight.client.request_may_have_been_sent()
                        && (Instant::now() > run.refresh_deadline
                            || Instant::now() >= run.started_at + run.horizon()) =>
                {
                    // Admission and the first provider poll can be separated by
                    // a suspended consumer. Never dispatch a now-stale refresh.
                    CacheWarmStep::Expired
                }
                Work::Refreshing(flight) => {
                    CacheWarmStep::Finished(flight.future.as_mut().await.map(Box::new))
                }
            }
        };
        tokio::select! {
            biased;
            _ = self.mode_rx.changed() => CacheWarmStep::ModeChanged,
            step = next_work => step,
        }
    }

    pub(crate) fn advance(
        &mut self,
        step: CacheWarmStep,
        host: CacheWarmHost<'_>,
    ) -> Result<Option<AgentEvent>, SessionError> {
        let result = self.advance_inner(
            step,
            CacheWarmHost {
                session: &mut *host.session,
                ..host
            },
        );
        self.publish_status(host.session, host.tool_generation);
        result
    }

    fn advance_inner(
        &mut self,
        step: CacheWarmStep,
        host: CacheWarmHost<'_>,
    ) -> Result<Option<AgentEvent>, SessionError> {
        if !matches!(step, CacheWarmStep::Finished(_)) {
            let dispatched = matches!(&self.work, Work::Refreshing(flight) if flight.client.request_may_have_been_sent());
            self.reconcile_mode(host.session)?;
            if dispatched && self.active.is_none() {
                return Ok(Some(AgentEvent::ProviderUsageUncertain));
            }
            if matches!(step, CacheWarmStep::ModeChanged) {
                return Ok(None);
            }
        }
        // Completed usage belongs to the session even if the context changed
        // during dispatch; abandoning it would undercount a billable request.
        if let CacheWarmStep::Finished(result) = step {
            let Work::Refreshing(mut flight) = std::mem::replace(&mut self.work, Work::Waiting)
            else {
                unreachable!("completed warm owns its flight");
            };
            let run = self.active.as_ref().expect("active refresh");
            let event = match result {
                Ok(response) => {
                    host.session.record_cache_warm_usage(
                        run.model.endpoint.id.clone(),
                        run.model.spec.id.clone(),
                        response.usage,
                        response.cost,
                    )?;
                    flight.record.state = CacheWarmState::Completed;
                    Some(AgentEvent::CacheWarmed {
                        usage: response.usage,
                        cost: response.cost,
                        extension_override: run.extension_override,
                    })
                }
                Err(timed_out) => {
                    let uncertain = flight.client.request_may_have_been_sent();
                    if uncertain {
                        host.session.record_usage_uncertainty_with_bound(
                            run.model.endpoint.id.clone(),
                            run.model.spec.id.clone(),
                            "cache_warm",
                            flight.bound,
                        )?;
                    }
                    flight.record.state = if timed_out {
                        CacheWarmState::TimedOut
                    } else {
                        CacheWarmState::Failed
                    };
                    uncertain.then_some(AgentEvent::ProviderUsageUncertain)
                }
            };
            flight.record.at_unix_ms = now_unix_millis();
            host.session.record_cache_warm_status(flight.record)?;
            self.reconcile_mode(host.session)?;
            if !self.valid(host.session, host.tool_generation)? {
                return Ok(event);
            }
            self.schedule(host.session)?;
            return Ok(event);
        }
        if !self.valid(host.session, host.tool_generation)? {
            return Ok(None);
        }
        let run = self.active.as_ref().expect("validated cache");
        if matches!(step, CacheWarmStep::Expired) || Instant::now() > run.refresh_deadline {
            self.cancel(host.session, "cache refresh deadline missed")?;
            return Ok(None);
        }
        match step {
            CacheWarmStep::Due => {
                let decision = evaluate(run, host.session);
                let context = CacheWarmingDecisionContext {
                    model: run.model.spec.id.clone(),
                    resource_owner: host.resource_owner.into(),
                    decision: decision.clone(),
                };
                let hooks = host.hooks.to_vec();
                let deadline = (Instant::now() + HOOK_BUDGET).min(run.refresh_deadline);
                let initial = decision.action;
                self.active.as_mut().unwrap().decision = Some(decision);
                self.work = Work::Deciding(Box::pin(async move {
                    let mut action = initial;
                    for hook in hooks {
                        match tokio::time::timeout_at(
                            deadline,
                            hook.cache_warming_decision(&context),
                        )
                        .await
                        {
                            Ok(Some(override_action)) => action = override_action,
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    }
                    action
                }));
            }
            CacheWarmStep::Decided(action) => {
                let run = self.active.as_mut().unwrap();
                // Like pi, the hook answers the decision it was given. A phase
                // transition while it awaits must not discard its last action.
                let decision = run
                    .decision
                    .as_ref()
                    .expect("decision preceded hook")
                    .clone();
                let override_action = action != decision.action;
                run.extension_override = action != decision.action;
                run.decision = Some(decision.clone());
                if action == CacheWarmingAction::Stop {
                    let reason = if run.extension_override {
                        "stopped by extension"
                    } else if decision.economics_available {
                        "expected savings below threshold"
                    } else {
                        "cache economics unavailable"
                    };
                    self.cancel(host.session, reason)?;
                    self.inactive.decision = Some(decision);
                    self.inactive.extension_override = override_action;
                    return Ok(None);
                }
                let input_tokens = run.input_tokens.max(prompt_tokens(host.session));
                let bound = request_uncertainty_bound(
                    &run.model,
                    input_tokens,
                    1,
                    None,
                    run.request.cache_retention,
                );
                let token_limit = host.limits.max_session_tokens.map(|limit| {
                    limit.saturating_sub(
                        host.limits
                            .pending_request
                            .map_or(0, |pending| pending.tokens),
                    )
                });
                let cost_limit = match (
                    host.limits.max_session_cost_microdollars,
                    host.limits.pending_request,
                ) {
                    (Some(limit), Some(pending)) => match pending.cost_microdollars {
                        Some(cost) => Some(limit.saturating_sub(cost)),
                        None => {
                            self.cancel(host.session, "session cost ceiling unavailable")?;
                            return Ok(None);
                        }
                    },
                    (limit, _) => limit,
                };
                if input_tokens.saturating_add(1) > run.model.spec.limits.context_window
                    || reserve_request_tokens(host.session, input_tokens, 1, token_limit).is_err()
                    || reserve_request_cost(
                        host.session,
                        &run.model,
                        input_tokens,
                        1,
                        cost_limit,
                        run.request.cache_retention,
                    )
                    .is_err()
                    || ((token_limit.is_some() || cost_limit.is_some()) && bound.is_none())
                {
                    self.cancel(host.session, "session ceiling or request capacity reached")?;
                    return Ok(None);
                }
                let attempt = host
                    .session
                    .cache_warm_records()
                    .last()
                    .map_or(1, |record| record.attempt + 1);
                let record = CacheWarmRecord {
                    attempt,
                    endpoint: run.model.endpoint.id.clone(),
                    model: run.model.spec.id.clone(),
                    state: CacheWarmState::Started,
                    at_unix_ms: now_unix_millis(),
                    anchor: run.anchor.clone(),
                    extension_override: run.extension_override,
                };
                host.session.record_cache_warm_status(record.clone())?;
                let client = host.client.track_request_dispatch();
                let tracked = client.clone();
                let model = run.model.clone();
                let mut request = run.request.clone();
                // The ONLY semantic request change. No synthetic user suffix,
                // assistant cache boundary, tool choice, or thinking change.
                request.max_output_tokens = Some(1);
                let future = Box::pin(async move {
                    let call = async {
                        let mut stream = tracked.stream(&model, request).await?;
                        while let Some(event) = stream.next().await {
                            if let StreamEvent::Finished(response) = event? {
                                return Ok::<_, AiError>(response);
                            }
                        }
                        Err(AiError::Canceled)
                    };
                    match tokio::time::timeout(REQUEST_DEADLINE, call).await {
                        Ok(Ok(response)) => Ok(response),
                        Ok(Err(_)) => Err(false),
                        Err(_) => Err(true),
                    }
                });
                self.work = Work::Refreshing(Box::new(Flight {
                    future,
                    client,
                    record,
                    bound,
                }));
            }
            CacheWarmStep::ModeChanged | CacheWarmStep::Expired | CacheWarmStep::Finished(_) => {
                unreachable!("settled above")
            }
        }
        Ok(None)
    }

    pub(crate) fn cancel(
        &mut self,
        session: &mut Session,
        reason: &str,
    ) -> Result<(), SessionError> {
        let work = std::mem::replace(&mut self.work, Work::Waiting);
        self.active = None;
        self.inactive = CacheWarmingStatus::inactive(reason);
        self.status_tx.send_replace(self.inactive.clone());
        if let Work::Refreshing(mut flight) = work {
            // Drop the provider future before exposing a cancelled scheduler.
            drop(flight.future);
            if flight.client.request_may_have_been_sent() {
                session.record_usage_uncertainty_with_bound(
                    flight.record.endpoint.clone(),
                    flight.record.model.clone(),
                    "cache_warm",
                    flight.bound,
                )?;
            }
            flight.record.state = CacheWarmState::Failed;
            flight.record.at_unix_ms = now_unix_millis();
            session.record_cache_warm_status(flight.record)?;
        }
        Ok(())
    }

    fn valid(&mut self, session: &mut Session, generation: u64) -> Result<bool, SessionError> {
        let Some(run) = self.active.as_ref() else {
            return Ok(false);
        };
        let reason = if self.mode() == CacheWarmMode::Off {
            Some("cache warming disabled")
        } else if self.mode() == CacheWarmMode::Streaming && run.phase == CacheWarmingPhase::Idle {
            Some("agent run settled")
        } else if !run.is_current(session, generation) {
            Some("conversation context changed")
        } else if Instant::now() >= run.started_at + run.horizon() {
            Some(run.limit_reason())
        } else {
            None
        };
        if let Some(reason) = reason {
            self.cancel(session, reason)?;
            return Ok(false);
        }
        Ok(true)
    }

    fn schedule(&mut self, session: &mut Session) -> Result<(), SessionError> {
        let run = self.active.as_mut().expect("only active cache schedules");
        let now = Instant::now();
        run.extension_override = false;
        run.next_at = now + run.delay;
        run.refresh_deadline = run.next_at + (run.ttl - run.delay) / 2;
        run.next_wall_ms = now_unix_millis().saturating_add(run.delay.as_millis() as u64);
        if run.next_at > run.started_at + run.horizon() || now >= run.started_at + run.horizon() {
            let reason = run.limit_reason();
            self.cancel(session, reason)?;
        }
        self.work = Work::Waiting;
        Ok(())
    }
}

fn is_replayable(model: &Model, request: &Request) -> bool {
    octet_ai::effective_output_token_cap(model, Some(1)) == Some(1)
        && (request.reasoning == ReasoningConfig::Off
            || model.spec.protocol != Protocol::AnthropicMessages
            || model
                .spec
                .preset
                .anthropic_compat
                .as_ref()
                .is_some_and(|compat| compat.force_adaptive_thinking == Some(true)))
}

fn prompt_tokens(session: &Session) -> u64 {
    session.latest_active_assistant_usage().map_or(0, |record| {
        record
            .usage
            .input_tokens
            .saturating_add(record.usage.cache_read_tokens)
            .saturating_add(record.usage.cache_write_tokens)
    })
}

fn exact_price(model: &Model, usage: Usage) -> Option<i128> {
    let cost = octet_ai::pricing::cost_of(model.spec.pricing.as_ref()?, &usage).ok()?;
    Some(
        i128::from(cost.total) * i128::from(PICODOLLARS_PER_MICRODOLLAR)
            + i128::from(cost.total_picodollars_remainder),
    )
}

fn evaluate(run: &ActiveCache, session: &Session) -> CacheWarmingDecision {
    let tokens = prompt_tokens(session);
    let hit = exact_price(
        &run.model,
        Usage {
            cache_read_tokens: tokens,
            ..Usage::default()
        },
    );
    let miss = exact_price(
        &run.model,
        if run
            .model
            .spec
            .pricing
            .as_ref()
            .is_some_and(|price| price.cache_write_5m.0 > 0)
        {
            Usage {
                cache_write_tokens: tokens,
                ..Usage::default()
            }
        } else {
            Usage {
                input_tokens: tokens,
                ..Usage::default()
            }
        },
    );
    let warm = exact_price(
        &run.model,
        Usage {
            cache_read_tokens: tokens,
            output_tokens: 1,
            ..Usage::default()
        },
    );
    let available = tokens > 0
        && hit.zip(miss).is_some_and(|(hit, miss)| hit > 0 || miss > 0)
        && warm.is_some();
    let hit = hit.unwrap_or(0);
    let miss = (miss.unwrap_or(0) - hit).max(0);
    let warm = warm.unwrap_or(0);
    let numerator = if run.phase == CacheWarmingPhase::Idle {
        15
    } else {
        100
    };
    let savings = miss * numerator / 100 - warm;
    let unit = i128::from(PICODOLLARS_PER_MICRODOLLAR);
    CacheWarmingDecision {
        phase: run.phase,
        warm_cost_microdollars: (warm / unit) as u64,
        miss_cost_microdollars: (miss / unit) as u64,
        continuation_probability: if numerator == 15 { 0.15 } else { 1.0 },
        expected_savings_microdollars: savings / unit,
        economics_available: available,
        action: if available && savings >= MINIMUM_SAVINGS_MICRODOLLARS * unit {
            CacheWarmingAction::Warm
        } else {
            CacheWarmingAction::Stop
        },
    }
}

#[cfg(test)]
mod tests;
