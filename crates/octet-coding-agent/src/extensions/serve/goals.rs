//! Session goals as the web UI sees them: the goal store, events and mutations.

use super::*;

/// Adapter from the optional Serve goal persistence to the provider-neutral
/// continuation driver. The actor owns when this adapter is invoked; the
/// store itself remains durable and frontend-independent.
#[derive(Clone)]
pub(super) struct ServeGoalStore {
    pub(super) store: GoalStore,
}

impl ServeGoalStore {
    pub(super) fn session_id(raw: &str) -> Result<SessionId, String> {
        SessionId::new(raw.to_owned()).map_err(|_| "invalid session id".to_owned())
    }

    pub(super) fn state(state: octet_serve_backend::GoalState) -> AgentGoalState {
        state
    }

    pub(super) fn result(
        result: Result<octet_serve_backend::GoalState, octet_serve_backend::GoalStoreError>,
    ) -> Result<AgentGoalState, String> {
        result.map(Self::state).map_err(|error| error.to_string())
    }
}

impl AgentGoalStore for ServeGoalStore {
    fn get(&self, session_id: &str) -> Result<Option<AgentGoalState>, String> {
        let session_id = Self::session_id(session_id)?;
        self.store
            .get(&session_id)
            .map(|state| state.map(Self::state))
            .map_err(|error| error.to_string())
    }

    fn record_turn(&self, session_id: &str) -> Result<AgentGoalState, String> {
        let session_id = Self::session_id(session_id)?;
        Self::result(self.store.record_turn(&session_id))
    }

    fn mark_complete(&self, session_id: &str) -> Result<AgentGoalState, String> {
        let session_id = Self::session_id(session_id)?;
        Self::result(self.store.mark_complete(&session_id))
    }

    fn mark_blocked(&self, session_id: &str) -> Result<AgentGoalState, String> {
        let session_id = Self::session_id(session_id)?;
        Self::result(self.store.mark_blocked(&session_id))
    }

    fn pause(&self, session_id: &str) -> Result<AgentGoalState, String> {
        let session_id = Self::session_id(session_id)?;
        let state = self
            .store
            .apply(&session_id, GoalAction::Pause)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "goal was cleared".to_owned())?;
        Ok(Self::state(state))
    }
}

pub(super) fn goal_service_error(error: GoalStoreError) -> ServiceError {
    match error {
        GoalStoreError::InvalidObjective
        | GoalStoreError::InvalidTurnBudget
        | GoalStoreError::NotFound
        | GoalStoreError::InvalidTransition => ServiceError::InvalidGoal,
        GoalStoreError::UnsafePath | GoalStoreError::CorruptState | GoalStoreError::Storage(_) => {
            ServiceError::Internal
        }
    }
}

pub(super) fn goal_event(goal: Option<ServeGoalState>, revision: u64) -> TimestampedEvent {
    event(EventPayload::GoalChanged { goal, revision })
}

pub(super) fn current_goal(
    store: Option<&GoalStore>,
    session_id: &SessionId,
) -> Result<Option<ServeGoalState>, ServiceError> {
    store
        .map(|store| store.get(session_id).map_err(goal_service_error))
        .transpose()
        .map(|goal| goal.flatten())
}

pub(super) fn current_goal_event(
    store: Option<&GoalStore>,
    session_id: &SessionId,
) -> Result<TimestampedEvent, ServiceError> {
    let goal = current_goal(store, session_id)?;
    let revision = store
        .map(|store| store.revision(session_id).map_err(goal_service_error))
        .transpose()?
        .unwrap_or(0);
    Ok(goal_event(goal, revision))
}

pub(super) fn apply_goal_command(
    store: &GoalStore,
    session_id: &SessionId,
    command: SessionCommand,
) -> Result<Option<ServeGoalState>, ServiceError> {
    match command {
        SessionCommand::SetGoal {
            objective,
            turn_budget,
        } => store
            .set(session_id, &objective, turn_budget)
            .map(Some)
            .map_err(goal_service_error),
        SessionCommand::PauseGoal => store
            .apply(session_id, GoalAction::Pause)
            .map_err(goal_service_error),
        SessionCommand::ResumeGoal => store
            .apply(session_id, GoalAction::Resume)
            .map_err(goal_service_error),
        SessionCommand::ClearGoal => store
            .apply(session_id, GoalAction::Clear)
            .map_err(goal_service_error),
        _ => Err(ServiceError::InvalidBoundary),
    }
}

pub(super) fn goal_deadline_after_user_change(
    goal_driver: Option<&GoalDriver>,
) -> Result<Option<tokio::time::Instant>, ServiceError> {
    let Some(goal_driver) = goal_driver else {
        return Ok(None);
    };
    goal_driver.user_spoke();
    match goal_driver
        .turn_settled(GoalTurnSource::User, "", false)
        .map_err(|_| ServiceError::Internal)?
    {
        GoalDecision::Wait { delay, .. } => Ok(Some(tokio::time::Instant::now() + delay)),
        _ => Ok(None),
    }
}

pub(super) fn goal_mutation_outcome(
    plan: &WorkerPlan,
    command: SessionCommand,
) -> Result<DriverCommandOutcome, ServiceError> {
    let Some(store) = plan.goal_store.as_ref() else {
        return Err(ServiceError::InvalidBoundary);
    };
    let goal = apply_goal_command(store, &plan.session_id, command)?;
    let revision = store
        .revision(&plan.session_id)
        .map_err(goal_service_error)?;
    Ok(DriverCommandOutcome::with_events(vec![goal_event(
        goal, revision,
    )]))
}

pub(super) fn schedule_goal(decision: Option<GoalDecision>) -> Option<tokio::time::Instant> {
    match decision {
        Some(GoalDecision::Wait { delay, .. }) => Some(tokio::time::Instant::now() + delay),
        _ => None,
    }
}
