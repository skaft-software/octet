//! Tool call admission: argument checks, policy decisions and effect reservation.

use super::*;

pub(super) struct CompletedToolExecution {
    pub(super) result: Result<ToolOutput, ToolError>,
    /// Host-owned effect admission result, absent only when the call never
    /// reached a registered tool's effect boundary.
    pub(super) policy_decision: Option<ToolPolicyDecision>,
    /// Wall time taken for the call.
    pub(super) duration: std::time::Duration,
    /// Unix milliseconds just before the tool's effects were admitted
    /// (`None` when the call never reached the effect gate).
    pub(super) started_unix_ms: Option<u64>,
    /// Unix milliseconds when the call's outcome was finalized.
    pub(super) finished_unix_ms: Option<u64>,
    pub(super) progress_rx: mpsc::Receiver<ToolProgress>,
    pub(super) progress_sink: ToolProgressSink,
    pub(super) cancellation_won: bool,
}

/// Synthetic, secret-safe result for a normalized call rejected by the exact
/// request schema. Keep this static: provider arguments may contain secrets.
pub(super) const SCHEMA_MISMATCH_TOOL_ERROR: &str =
    "tool call was not executed because its arguments do not satisfy the advertised schema; correct the arguments and try again";

pub(super) fn rejected_argument_tool_error(error: ToolCallArgumentError) -> ToolError {
    let message = match error {
        ToolCallArgumentError::SchemaMismatch => SCHEMA_MISMATCH_TOOL_ERROR,
    };
    ToolError::new(message)
}

pub(super) fn rejected_argument_tool_execution(
    error: ToolCallArgumentError,
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
) -> CompletedToolExecution {
    let (progress_tx, progress_rx) = mpsc::channel(PROGRESS_CHANNEL_CAPACITY);
    CompletedToolExecution {
        result: Err(rejected_argument_tool_error(error)),
        // The exact request schema rejected this call before tool effect
        // classification. Still expose the host-owned pre-admission denial so
        // policy diagnostics remain complete without exposing provider args.
        policy_decision: Some(policy_decision(
            sandbox,
            broker,
            None,
            None,
            Some(ToolPolicyDenialCode::InvalidToolArguments),
        )),
        duration: std::time::Duration::ZERO,
        started_unix_ms: None,
        finished_unix_ms: Some(crate::session::now_unix_millis()),
        progress_rx,
        progress_sink: ToolProgressSink::live(progress_tx),
        cancellation_won: false,
    }
}

pub(super) struct ToolEffectAdmission {
    pub(super) intent: EffectIntent,
    pub(super) reservation: EffectReservation,
    pub(super) effect: ToolEffect,
}

pub(super) struct ToolEffectAdmissionError {
    pub(super) error: ToolError,
    pub(super) decision: ToolPolicyDecision,
}

pub(super) fn policy_decision(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
    effect: Option<ToolEffect>,
    authorization: Option<crate::effect::EffectAuthorization>,
    denial_code: Option<ToolPolicyDenialCode>,
) -> ToolPolicyDecision {
    ToolPolicyDecision {
        effect,
        allowed: authorization.is_some(),
        authorization,
        denial_code,
        policy: sandbox.effective_tool_policy(broker.policy()),
    }
}

pub(super) fn denied_tool_policy(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
    effect: Option<ToolEffect>,
    denial_code: ToolPolicyDenialCode,
    message: impl Into<String>,
) -> (ToolError, ToolPolicyDecision) {
    let decision = policy_decision(sandbox, broker, effect, None, Some(denial_code));
    let error = ToolError::policy_denied(denial_code, message);
    (error, decision)
}

/// Classify pre-admission argument parsing failures without retaining provider
/// argument text in an error result or diagnostic.
pub(super) fn invalid_tool_arguments_denial(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
) -> (ToolError, ToolPolicyDecision) {
    denied_tool_policy(
        sandbox,
        broker,
        None,
        ToolPolicyDenialCode::InvalidToolArguments,
        "invalid tool arguments",
    )
}

/// Classify a trusted hook veto without exposing hook-provided text to the model.
pub(super) fn secondary_hook_denial(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
    effect: Option<ToolEffect>,
) -> (ToolError, ToolPolicyDecision) {
    denied_tool_policy(
        sandbox,
        broker,
        effect,
        ToolPolicyDenialCode::SecondaryHookDenied,
        "tool call denied by host policy",
    )
}

/// Classify a reservation that was invalidated between admission and dispatch.
pub(super) fn effect_reservation_commit_denial(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
    effect: ToolEffect,
    error: &crate::effect::EffectBrokerError,
) -> (ToolError, ToolPolicyDecision) {
    denied_tool_policy(
        sandbox,
        broker,
        Some(effect),
        error.policy_denial_code(),
        "effect reservation could not be committed",
    )
}

/// Replace an earlier broker admission with a later trusted tool-boundary
/// denial, such as a resolved symlink escaping workspace confinement.
pub(super) fn apply_execution_policy_denial(
    policy_decision: &mut Option<ToolPolicyDecision>,
    result: &Result<ToolOutput, ToolError>,
) {
    let (Some(decision), Err(error)) = (policy_decision, result) else {
        return;
    };
    let Some(denial_code) = error.policy_denial_code() else {
        return;
    };
    decision.allowed = false;
    decision.authorization = None;
    decision.denial_code = Some(denial_code);
}

pub(super) fn effect_is_repeatable_observation(effect: ToolEffect) -> bool {
    matches!(effect, ToolEffect::Pure | ToolEffect::WorkspaceRead)
}

/// Live read scheduling is broader than crash replay. Host reads are allowed
/// to overlap only after the exact host-owned classification and policy
/// admission; they remain ineligible for automatic recovery after a crash.
pub(super) fn effect_is_parallel_observation(effect: ToolEffect) -> bool {
    matches!(
        effect,
        ToolEffect::Pure | ToolEffect::WorkspaceRead | ToolEffect::HostRead
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn reserve_tool_effect(
    broker: &EffectBroker,
    tool: &dyn Tool,
    name: &str,
    arguments: &serde_json::Value,
    context: &ToolContext<'_>,
    principal: &str,
    run_id: &str,
    generation: u64,
    request_id: &octet_ai::ToolCallId,
    interactive: bool,
) -> Result<ToolEffectAdmission, ToolEffectAdmissionError> {
    let effect = tool.effect(arguments, context).map_err(|error| {
        let denial_code = error
            .policy_denial_code()
            .unwrap_or(ToolPolicyDenialCode::InvalidToolArguments);
        ToolEffectAdmissionError {
            decision: policy_decision(context.sandbox, broker, None, None, Some(denial_code)),
            error,
        }
    })?;
    let intent = EffectIntent::new(
        principal,
        run_id,
        generation,
        request_id.0.clone(),
        name,
        effect,
        arguments,
    )
    .map_err(|error| {
        let denial_code = error.policy_denial_code();
        ToolEffectAdmissionError {
            decision: policy_decision(
                context.sandbox,
                broker,
                Some(effect),
                None,
                Some(denial_code),
            ),
            error: ToolError::new(error.to_string()),
        }
    })?;
    let reservation = broker
        .reserve(&intent, interactive.then_some(&context.progress))
        .await
        .map_err(|error| {
            let denial_code = error.policy_denial_code();
            ToolEffectAdmissionError {
                decision: policy_decision(
                    context.sandbox,
                    broker,
                    Some(effect),
                    None,
                    Some(denial_code),
                ),
                error: ToolError::new(error.to_string()),
            }
        })?;
    Ok(ToolEffectAdmission {
        intent,
        reservation,
        effect,
    })
}

/// Terminal status of one tool boundary: a hard error or a tool-reported
/// error result is an error span; a tool-reported success is not.
pub(super) fn tool_execution_failed(result: &Result<ToolOutput, ToolError>) -> bool {
    match result {
        Ok(output) => output.is_error(),
        Err(_) => true,
    }
}
