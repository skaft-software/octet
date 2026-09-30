//! Token and cost budgets: reservations, worst-case request cost and usage accounting.

use super::*;

pub(super) fn reasoning_token_budget(model: &Model, reasoning: &ReasoningConfig) -> u64 {
    match reasoning {
        ReasoningConfig::Budget(budget) => *budget,
        ReasoningConfig::Effort(effort) => model
            .spec
            .capabilities
            .reasoning
            .as_ref()
            .filter(|capability| capability.control == octet_ai::ReasoningControl::TokenBudget)
            .and_then(|capability| {
                let budgets = capability.effort_budgets?;
                let effort = (*effort).min(capability.max_effort);
                Some(match effort {
                    octet_ai::ReasoningEffort::Minimal => budgets.minimal,
                    octet_ai::ReasoningEffort::Low => budgets.low,
                    octet_ai::ReasoningEffort::Medium => budgets.medium,
                    octet_ai::ReasoningEffort::High => budgets.high,
                    octet_ai::ReasoningEffort::Xhigh => budgets.xhigh,
                    octet_ai::ReasoningEffort::Max | octet_ai::ReasoningEffort::Ultra => {
                        budgets.max
                    }
                })
            })
            .unwrap_or_default(),
        ReasoningConfig::Off | ReasoningConfig::On => 0,
    }
}

pub(super) fn agent_compaction_reserve_tokens(model: &Model, reasoning: &ReasoningConfig) -> u64 {
    let model_max = model.spec.limits.max_output_tokens.max(1);
    let reasoning_floor = reasoning_token_budget(model, reasoning)
        .saturating_add(REASONING_ANSWER_RESERVE)
        .min(model_max);
    DEFAULT_COMPACTION_RESERVE_TOKENS
        .max(reasoning_floor)
        .min(model_max)
}

/// Headroom reserved between the per-request input estimate and the provider's
/// own input count.
///
/// The estimate is bytes/4 plus structural overhead, while a provider counts
/// with its own tokenizer and chat template. Sizing the request as exactly
/// `window - estimate` therefore sits on the boundary, where a one-token
/// difference is a hard rejection: a real vLLM deployment answered
/// "maximum context length is 131072 tokens ... you requested 30896 output
/// tokens and your prompt contains at least 100177 input tokens, for a total of
/// at least 131073 tokens". Reserving bounded slack keeps `input + output`
/// inside the window without meaningfully shrinking a decoded answer.
pub(super) const REQUEST_OUTPUT_HEADROOM_PERCENT: u64 = 1;

pub(super) const REQUEST_OUTPUT_HEADROOM_DIVISOR: u64 = 100;

pub(super) const REQUEST_OUTPUT_HEADROOM_MINIMUM: u64 = 256;

pub(super) const REQUEST_OUTPUT_HEADROOM_MAXIMUM: u64 = 4096;

pub(super) fn request_output_headroom(context_window: u64) -> u64 {
    ((context_window / REQUEST_OUTPUT_HEADROOM_DIVISOR) * REQUEST_OUTPUT_HEADROOM_PERCENT).clamp(
        REQUEST_OUTPUT_HEADROOM_MINIMUM,
        REQUEST_OUTPUT_HEADROOM_MAXIMUM,
    )
}

pub(super) fn resolve_request_max_output_tokens(
    context_window: u64,
    input_tokens: u64,
    provider_output_ceiling: u64,
) -> u64 {
    provider_output_ceiling.min(
        context_window
            .saturating_sub(input_tokens)
            .saturating_sub(request_output_headroom(context_window)),
    )
}

pub(super) fn add_usage(total: &mut Usage, turn: &Usage) {
    total.input_tokens = total.input_tokens.saturating_add(turn.input_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(turn.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(turn.cache_write_tokens);
    total.cache_write_1h_tokens = total
        .cache_write_1h_tokens
        .saturating_add(turn.cache_write_1h_tokens);
    total.output_tokens = total.output_tokens.saturating_add(turn.output_tokens);
    total.reasoning_tokens = total.reasoning_tokens.saturating_add(turn.reasoning_tokens);
    total.total_tokens = total.total_tokens.saturating_add(turn.total_tokens);
}

pub(super) fn usage_since(after: Usage, before: Usage) -> Usage {
    Usage {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        cache_read_tokens: after
            .cache_read_tokens
            .saturating_sub(before.cache_read_tokens),
        cache_write_tokens: after
            .cache_write_tokens
            .saturating_sub(before.cache_write_tokens),
        cache_write_1h_tokens: after
            .cache_write_1h_tokens
            .saturating_sub(before.cache_write_1h_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        reasoning_tokens: after
            .reasoning_tokens
            .saturating_sub(before.reasoning_tokens),
        total_tokens: after.total_tokens.saturating_sub(before.total_tokens),
    }
}

#[derive(Default)]
pub(super) struct CostAccumulator {
    pub(super) microdollars: u64,
    pub(super) picodollars_remainder: u32,
    pub(super) unpriced_operations: u64,
}

impl CostAccumulator {
    /// Aggregate a request after its usage record durably updates the session.
    /// Missing prices remain unpriced; the numeric amount is a known subtotal.
    pub(super) fn add(&mut self, cost: Option<Cost>) {
        let Some(cost) = cost else {
            self.unpriced_operations = self.unpriced_operations.saturating_add(1);
            return;
        };
        let remainder = u64::from(self.picodollars_remainder)
            .saturating_add(u64::from(cost.total_picodollars_remainder));
        let carry = remainder / u64::from(PICODOLLARS_PER_MICRODOLLAR);
        self.microdollars = self
            .microdollars
            .saturating_add(cost.total)
            .saturating_add(carry);
        self.picodollars_remainder = (remainder % u64::from(PICODOLLARS_PER_MICRODOLLAR)) as u32;
    }
}

pub(super) fn worst_case_request_cost(
    model: &Model,
    input_tokens: u64,
    output_tokens: u64,
    service_tier: Option<ServiceTier>,
) -> Option<u64> {
    let pricing = model.spec.pricing.as_ref()?;
    let cache_write_1h_rate = match pricing.cache_write_1h {
        Some(rate) => rate.0,
        None => pricing.input.0.checked_mul(2)?,
    };
    let mut input_rate = pricing
        .input
        .0
        .max(pricing.cache_read.0)
        .max(pricing.cache_write_5m.0)
        .max(cache_write_1h_rate);
    let mut output_rate = pricing
        .output
        .0
        .max(pricing.reasoning.map(|rate| rate.0).unwrap_or_default());
    if model.spec.protocol == Protocol::AnthropicMessages {
        for fallback in model
            .spec
            .preset
            .anthropic_compat
            .as_ref()
            .into_iter()
            .flat_map(|compat| &compat.allowed_fallback_models)
        {
            // The server may select any declared fallback, including one more
            // expensive than the requested model. An unpriced target cannot be
            // admitted under a hard cost ceiling.
            let pricing = fallback.cost?.pricing()?;
            input_rate = input_rate
                .max(pricing.input.0)
                .max(pricing.cache_read.0)
                .max(pricing.cache_write_5m.0)
                .max(pricing.input.0.checked_mul(2)?);
            output_rate = output_rate.max(pricing.output.0);
        }
    }
    for tier in &pricing.tiers {
        // The implicit one-hour write price follows the active input tier,
        // not the base catalog input rate. Never reserve below that bucket.
        if pricing.cache_write_1h.is_none() {
            if let Some(rate) = tier.input {
                input_rate = input_rate.max(rate.0.checked_mul(2)?);
            }
        }
        for rate in [
            tier.input,
            tier.cache_read,
            tier.cache_write_5m,
            tier.cache_write_1h,
        ]
        .into_iter()
        .flatten()
        {
            input_rate = input_rate.max(rate.0);
        }
        for rate in [tier.output, tier.reasoning].into_iter().flatten() {
            output_rate = output_rate.max(rate.0);
        }
    }
    let conservative = octet_ai::Pricing {
        input: octet_ai::TokenRate(input_rate),
        output: octet_ai::TokenRate(output_rate),
        cache_read: octet_ai::TokenRate(input_rate),
        cache_write_5m: octet_ai::TokenRate(input_rate),
        cache_write_1h: Some(octet_ai::TokenRate(input_rate)),
        reasoning: Some(octet_ai::TokenRate(output_rate)),
        tiers: Vec::new(),
    };
    let usage = Usage {
        input_tokens,
        output_tokens,
        ..Usage::default()
    };
    let cost = octet_ai::responses_cost_of(
        &conservative,
        &usage,
        model.endpoint.runtime.responses_profile,
        &model.spec.api_name,
        service_tier,
        None,
    )
    .ok()??;
    cost.total
        .checked_add(u64::from(cost.total_picodollars_remainder > 0))
}

pub(super) fn priced_session_subtotal(session: &Session, model: &Model) -> Option<u64> {
    (!session.has_unpriced_usage()
        && (session.total_cost_microdollars() > 0 || model.spec.pricing.is_some()))
    .then(|| session.total_cost_microdollars())
}

pub(super) fn usage_total_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage
            .input_tokens
            .saturating_add(usage.cache_read_tokens)
            .saturating_add(usage.cache_write_tokens)
            .saturating_add(usage.output_tokens)
    }
}

pub(super) fn session_total_tokens_for_own_context(session: &Session) -> u64 {
    session
        .usage_records()
        .iter()
        .filter(|record| !matches!(&record.kind, UsageRecordKind::DelegatedAgent { .. }))
        .fold(0u64, |total, record| {
            total.saturating_add(usage_total_tokens(&record.usage))
        })
}

pub(super) fn record_delegated_usage_once(
    session: &mut Session,
    mut delegated: DelegatedUsage,
) -> Result<(), SessionError> {
    use crate::delegation::{
        add_delegated_cost, add_delegated_usage, subtract_cost, subtract_usage,
    };

    // The root's committed ledger, not a process-local or fleet-file watermark,
    // is authoritative across failed appends, repeated snapshots and restarts.
    let mut mirrored_usage = Usage::default();
    let mut mirrored_cost = Cost::default();
    let mut mirrored_turns = 0;
    let mut mirrored_tools = 0;
    for record in session.usage_records() {
        if let UsageRecordKind::DelegatedAgent {
            agent_id,
            turn_count,
            tool_call_count,
        } = &record.kind
        {
            if *agent_id != delegated.agent_id {
                continue;
            }
            add_delegated_usage(&mut mirrored_usage, &record.usage);
            if let Some(cost) = record.cost {
                add_delegated_cost(&mut mirrored_cost, cost);
            }
            mirrored_turns = mirrored_turns.max(*turn_count);
            mirrored_tools = mirrored_tools.max(*tool_call_count);
        }
    }
    delegated.usage = subtract_usage(delegated.usage, mirrored_usage);
    delegated.cost = delegated
        .cost
        .map(|cost| subtract_cost(cost, mirrored_cost));
    if delegated.usage == Usage::default()
        && delegated.cost.unwrap_or_default() == Cost::default()
        && delegated.turn_count <= mirrored_turns
        && delegated.tool_call_count <= mirrored_tools
    {
        return Ok(());
    }
    session.record_delegated_agent_usage(delegated)
}

// Child records are cumulative snapshots. The root ledger, not a fleet-local
// watermark, is authoritative across repeated snapshots and process restarts.
pub(super) fn mirror_delegated_uncertainty(
    session: &mut Session,
    model: &Model,
    agent_id: &str,
    uncertain: bool,
    exposure: Option<UsageUncertaintyBound>,
) -> Result<bool, SessionError> {
    if !uncertain {
        return Ok(false);
    }
    let operation = format!("delegated_agent:{agent_id}");
    let mut mirrored = UsageUncertaintyBound {
        tokens: 0,
        cost_microdollars: Some(0),
    };
    let mut has_record = false;
    for (record, bound) in session
        .usage_uncertainty_records()
        .iter()
        .zip(session.usage_uncertainty_bounds())
    {
        if record.operation != operation {
            continue;
        }
        has_record = true;
        let Some(bound) = bound else {
            return Ok(false); // Already mirrored as unbounded.
        };
        mirrored.tokens = mirrored.tokens.saturating_add(bound.tokens);
        mirrored.cost_microdollars = mirrored
            .cost_microdollars
            .zip(bound.cost_microdollars)
            .map(|(left, right)| left.saturating_add(right));
    }
    let delta = exposure.map(|exposure| UsageUncertaintyBound {
        tokens: exposure.tokens.saturating_sub(mirrored.tokens),
        cost_microdollars: exposure.cost_microdollars.and_then(|cost| {
            mirrored
                .cost_microdollars
                .map(|prior| cost.saturating_sub(prior))
        }),
    });
    if has_record
        && delta.is_some_and(|delta| {
            delta.tokens == 0 && delta.cost_microdollars.unwrap_or_default() == 0
        })
    {
        return Ok(false);
    }
    session.record_usage_uncertainty_with_bound(
        model.endpoint.id.clone(),
        model.spec.id.clone(),
        operation,
        delta,
    )?;
    Ok(true)
}

pub(super) fn request_uncertainty_bound(
    model: &Model,
    input_tokens: u64,
    requested_output_tokens: u64,
    service_tier: Option<ServiceTier>,
    retention: CacheRetention,
) -> Option<UsageUncertaintyBound> {
    let cap = octet_ai::effective_output_token_cap(model, Some(requested_output_tokens))?;
    let fallback_long_retention = retention == CacheRetention::Long
        && model.spec.protocol == Protocol::AnthropicMessages
        && model.spec.cache.supports_long_retention
        && model
            .spec
            .preset
            .anthropic_compat
            .as_ref()
            .is_some_and(|compat| !compat.allowed_fallback_models.is_empty());
    Some(UsageUncertaintyBound {
        tokens: input_tokens.saturating_add(cap),
        cost_microdollars: (!fallback_long_retention)
            .then(|| worst_case_request_cost(model, input_tokens, cap, service_tier))
            .flatten(),
    })
}

pub(super) fn uncertainty_blocks_ceiling(
    session: &Session,
    token_limit: Option<u64>,
    cost_limit: Option<u64>,
) -> bool {
    (token_limit.is_some() || cost_limit.is_some())
        && session
            .usage_uncertainty_exposure()
            .is_none_or(|exposure| cost_limit.is_some() && exposure.cost_microdollars.is_none())
}

pub(super) fn require_enforceable_output_cap(
    session: &Session,
    output_cap: Option<u64>,
    token_limit: Option<u64>,
    cost_limit: Option<u64>,
) -> Result<(), AgentError> {
    if token_limit.is_none() && cost_limit.is_none() {
        return Ok(());
    }
    // Existing exposure is the more specific reason a ceiling cannot work.
    if uncertainty_blocks_ceiling(session, token_limit, cost_limit) {
        return Err(AgentError::UsageUncertain);
    }
    if let Some(limit) = cost_limit {
        if session.has_unpriced_usage() {
            return Err(AgentError::CostUnavailable { limit });
        }
    }
    output_cap.ok_or(AgentError::OutputLimitUnavailable)?;
    Ok(())
}

pub(super) fn reservation_output_tokens(
    session: &Session,
    model: &Model,
    requested: u64,
    token_limit: Option<u64>,
    cost_limit: Option<u64>,
) -> Result<u64, AgentError> {
    let cap = octet_ai::effective_output_token_cap(model, Some(requested));
    require_enforceable_output_cap(session, cap, token_limit, cost_limit)?;
    // Only operations without hard ceilings may use an unenforced planning
    // estimate. It is never a fabricated bound for a hard reservation.
    Ok(cap.unwrap_or(requested))
}

pub(super) fn reserve_request_tokens(
    session: &Session,
    input_tokens: u64,
    output_tokens: u64,
    limit: Option<u64>,
) -> Result<(), AgentError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    let exposure = session
        .usage_uncertainty_exposure()
        .ok_or(AgentError::UsageUncertain)?;
    let current = session_total_tokens_for_own_context(session).saturating_add(exposure.tokens);
    let reserved = input_tokens.saturating_add(output_tokens);
    if current >= limit || current.saturating_add(reserved) > limit {
        return Err(AgentError::TokenLimit {
            current,
            reserved,
            limit,
        });
    }
    Ok(())
}

pub(super) fn reserve_request_cost(
    session: &Session,
    model: &Model,
    input_tokens: u64,
    output_tokens: u64,
    limit: Option<u64>,
    retention: CacheRetention,
) -> Result<(), AgentError> {
    reserve_request_cost_with_tier(
        session,
        model,
        input_tokens,
        output_tokens,
        limit,
        None,
        retention,
    )
}

pub(super) fn reserve_request_cost_with_tier(
    session: &Session,
    model: &Model,
    input_tokens: u64,
    output_tokens: u64,
    limit: Option<u64>,
    service_tier: Option<ServiceTier>,
    retention: CacheRetention,
) -> Result<(), AgentError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    let exposure = session
        .usage_uncertainty_exposure()
        .ok_or(AgentError::UsageUncertain)?;
    let current = session.total_cost_microdollars().saturating_add(
        exposure
            .cost_microdollars
            .ok_or(AgentError::UsageUncertain)?,
    );
    if session.has_unpriced_usage() {
        return Err(AgentError::CostUnavailable { limit });
    }
    // Fallback quotes have no one-hour cache-write tariff. A route that can
    // request one-hour writes cannot enforce a hard ceiling if the server
    // chooses a fallback; do not invent a price for that bucket.
    if retention == CacheRetention::Long
        && model.spec.protocol == Protocol::AnthropicMessages
        && model.spec.cache.supports_long_retention
        && model
            .spec
            .preset
            .anthropic_compat
            .as_ref()
            .is_some_and(|compat| !compat.allowed_fallback_models.is_empty())
    {
        return Err(AgentError::CostUnavailable { limit });
    }
    let reserved = worst_case_request_cost(model, input_tokens, output_tokens, service_tier)
        .ok_or(AgentError::CostUnavailable { limit })?;
    if current >= limit || current.saturating_add(reserved) > limit {
        return Err(AgentError::CostLimit {
            current,
            reserved,
            limit,
        });
    }
    Ok(())
}
