//! Status text, JSON values, validation and usage arithmetic shared by the delegation modules.

use super::*;

/// Restore the remaining absolute wall budget, including time spent detached.
pub(super) fn wall_deadline_instant(deadline_ms: u64) -> tokio::time::Instant {
    let now = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
    tokio::time::Instant::now() + Duration::from_millis(deadline_ms.saturating_sub(now))
}

pub(super) fn classify_delegation_failure(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("persist") || lower.contains("session descriptor") {
        "persistence_failure"
    } else if lower.contains("provider") || lower.contains("model response") {
        "provider_failure"
    } else if lower.contains("tool")
        && (lower.contains("policy")
            || lower.contains("scope")
            || lower.contains("unavailable")
            || lower.contains("denied"))
    {
        "tool_policy_failure"
    } else if lower.contains("token")
        || lower.contains("turn")
        || lower.contains("cost")
        || lower.contains("limit")
    {
        "limit"
    } else if lower.contains("start") || lower.contains("construct") {
        "child_construction_failure"
    } else if lower.contains("interrupt") || lower.contains("abort") || lower.contains("cancel") {
        "cancellation"
    } else {
        "child_failure"
    }
}

pub(super) fn status_message(path: &str, status: &DelegatedAgentStatus) -> String {
    match status {
        DelegatedAgentStatus::Completed { output } => {
            format!("{path} completed:\n{output}")
        }
        DelegatedAgentStatus::LimitReached {
            output,
            turn_count,
            turn_limit,
        } => {
            let output = if output.is_empty() {
                "no final answer was produced".to_owned()
            } else {
                format!("partial output:\n{output}")
            };
            format!("{path} reached its turn limit ({turn_count}/{turn_limit} turns); {output}")
        }
        DelegatedAgentStatus::Failed { error } => format!("{path} failed: {error}"),
        DelegatedAgentStatus::Interrupted => format!("{path} was interrupted"),
        DelegatedAgentStatus::TimedOut => format!("{path} timed out"),
        DelegatedAgentStatus::Detached => {
            format!("{path} is detached: it survived its owning run and can be reattached")
        }
        DelegatedAgentStatus::AwaitingApproval { reason } => {
            format!("{path} is awaiting approval and has not acted: {reason}")
        }
        DelegatedAgentStatus::Shutdown => format!("{path} was shut down"),
        DelegatedAgentStatus::Pending
        | DelegatedAgentStatus::Idle
        | DelegatedAgentStatus::Running => {
            format!("{path} is {}", status.label())
        }
    }
}

/// One bounded durable snapshot of a session-owned worker.
pub(super) fn durable_fleet_record(record: &AgentRecord) -> DurableFleetRecord {
    DurableFleetRecord {
        agent_id: record.identity.id.clone(),
        agent_path: record.identity.path.clone(),
        parent_id: record.parent_id.clone(),
        depth: record.identity.depth,
        task_name: record.task_name.clone(),
        display_task_name: record.display_task_name.clone(),
        session_path: record.session_path.clone(),
        status: record.status.clone(),
        detached: record.detached,
        created_at_ms: record.created_at_ms,
        started_at_ms: record.started_at_ms,
        completed_at_ms: record.completed_at_ms,
        turn_count: record.turn_count,
        tool_call_count: record.tool_call_count,
        usage: record.usage,
        usage_uncertain: record.usage_uncertain,
        usage_exposure: record.usage_exposure,
        cost: record.cost,
        cost_microdollars: record.cost_microdollars,
        deadline_at_ms: record.deadline_at_ms,
        turn_limit: record.turn_limit,
        extension_principal: record.extension_principal.clone(),
        extension_profile: record.extension_profile.clone(),
        extension_idempotency_key: record.extension_idempotency_key.clone(),
        extension_resource_owner: record.extension_resource_owner.clone(),
        extension_message_sha256: record.extension_message_sha256.clone(),
        extension_requested_policy: record.extension_requested_policy.clone(),
        extension_fingerprint: record.extension_fingerprint.clone(),
        extension_policy: record.extension_policy.clone(),
        resource_owner: record.resource_owner.clone(),
        durable_diagnostic: record.durable_diagnostic.clone(),
        claim: record.claim.clone(),
        pending_messages: record.pending_messages.clone(),
        queued_follow_ups: record.pending_follow_ups.clone(),
        pending_initial_task: record.pending_initial_task.clone(),
        mailbox: record.mailbox.iter().map(Into::into).collect(),
        mailbox_delivery: record.mailbox_delivery,
    }
}

/// Parse the numeric suffix of a stable `agent-{n}` identity.
pub(super) fn agent_number_from_id(id: &str) -> Option<u64> {
    id.strip_prefix("agent-").and_then(|rest| rest.parse().ok())
}

/// Reconstruct the stable `spawn_agent` result for a session-owned record.
///
/// Used to honour an extension spawn idempotency key across a turn or process
/// boundary without spawning a duplicate worker.
pub(super) fn extension_spawn_result_value(record: &AgentRecord) -> Value {
    json!({
        "agent_id": record.identity.id,
        "agent_path": record.identity.path,
        "task_name": record
            .display_task_name
            .as_deref()
            .unwrap_or(record.task_name.as_str()),
        "profile": record.extension_profile,
        "idempotency_key": record.extension_idempotency_key,
        "fingerprint": record.extension_fingerprint,
        "status": record.status,
        "resolved_model": resolved_model_json(record.extension_policy.as_ref()),
        "policy": public_policy_json(record.extension_policy.as_ref()),
        "effective_tool_policy": record.effective_tool_policy,
        "orchestration_provenance": record.orchestration_provenance,
        "created_at_ms": record.created_at_ms,
        "started_at_ms": record.started_at_ms,
        "completed_at_ms": record.completed_at_ms,
        "turn_limit": record.turn_limit,
        "deadline_at_ms": record.deadline_at_ms,
    })
}

/// Whether a child failure means the effect required an approval authority
/// that was not attached to this run.
///
/// This is the authority-to-act gate for unattended mutation, not the
/// credential/OAuth or persisted-trust gate.
pub(super) fn is_missing_approval_authority(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("approval is unavailable")
        || lower.contains("approval was not granted")
        || lower.contains("approval_unavailable")
        || lower.contains("approval_denied")
}

pub(super) fn list_value_locked(state: &ManagerState) -> Value {
    let agents = state
        .records
        .values()
        .map(agent_record_value)
        .collect::<Vec<_>>();
    json!({"agents": agents, "persistence_error": state.persistence_error})
}

pub(super) fn agent_record_value(record: &AgentRecord) -> Value {
    // Session-owned launchable handle. The token is the same opaque,
    // path-free, argv-safe reference the extension already receives; only the
    // verdict travels with it, never the transcript path.
    let blocked = launchability(record).err();
    let handle = delegated_session_reference(&record.session_path);
    let phase = if !record.active_tools.is_empty() {
        "using_tool"
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
    };
    json!({
        "agent_id": record.identity.id,
        "agent_path": record.identity.path,
        "parent_id": record.parent_id,
        "task_name": record.display_task_name.as_deref().unwrap_or(record.task_name.as_str()),
        "depth": record.identity.depth,
        "session": record.session_path,
        "status": record.status,
        "resolved_model": resolved_model_json(record.extension_policy.as_ref()),
        "policy": public_policy_json(record.extension_policy.as_ref()),
        "effective_tool_policy": record.effective_tool_policy,
        "orchestration_provenance": record.orchestration_provenance,
        "profile": record.extension_profile,
        "idempotency_key": record.extension_idempotency_key,
        "fingerprint": record.extension_fingerprint,
        "created_at_ms": record.created_at_ms,
        "started_at_ms": record.started_at_ms,
        "completed_at_ms": record.completed_at_ms,
        "detached": record.detached,
        "live_task": record.live_task,
        "handle": handle,
        "launchable": blocked.is_none(),
        "launch_blocked": blocked,
        "diagnostic": record.durable_diagnostic,
        "turn_count": record.turn_count,
        "turn_limit": record.turn_limit,
        "tool_call_count": record.tool_call_count,
        "phase": phase,
        "tool_name": record.active_tools.values().next_back(),
        "recent_tools": record
            .recent_tools
            .iter()
            .map(|activity| {
                json!({
                    "name": activity.name,
                    "args": activity.args_summary,
                    "started_at_ms": activity.started_at_ms,
                    "finished_at_ms": activity.finished_at_ms,
                    "error": activity.error,
                })
            })
            .collect::<Vec<_>>(),
        "usage": record.usage,
        "usage_uncertain": record.usage_uncertain,
        "cost_microdollars": record.cost_microdollars,
        "deadline_at_ms": record.deadline_at_ms,
    })
}

pub(super) fn target_message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": {"type": "string", "description": "Agent ID or absolute delegation path."},
            "message": {"type": "string", "description": "Information or task text to deliver."}
        },
        "required": ["target", "message"],
        "additionalProperties": false
    })
}

pub(super) fn tool_def(name: &str, description: &str, input_schema: Value) -> ToolDef {
    ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: name.into(),
        description: description.into(),
        parameters: input_schema,
    }
}

pub(super) fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ToolError::new(format!("{key} must be a non-empty string")))
}

pub(super) fn validate_task_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 48 {
        return Err("task_name must contain 1 to 48 characters".into());
    }
    if !name.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
    }) {
        return Err(
            "task_name may contain only lowercase ASCII letters, digits, underscore, and hyphen"
                .into(),
        );
    }
    Ok(())
}

pub(super) fn is_descendant_path(candidate: &str, parent: &str) -> bool {
    candidate.len() > parent.len()
        && candidate.starts_with(parent)
        && candidate.as_bytes().get(parent.len()) == Some(&b'/')
}

pub(super) fn validate_durable_text(kind: &str, text: &str) -> Result<(), String> {
    if text.len() > MAX_PROVENANCE_TEXT_BYTES {
        return Err(format!(
            "{kind} exceeds the {}-byte delegation limit",
            MAX_PROVENANCE_TEXT_BYTES
        ));
    }
    Ok(())
}

pub(crate) fn add_delegated_usage(total: &mut Usage, next: &Usage) {
    total.input_tokens = total.input_tokens.saturating_add(next.input_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(next.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(next.cache_write_tokens);
    total.cache_write_1h_tokens = total
        .cache_write_1h_tokens
        .saturating_add(next.cache_write_1h_tokens);
    total.output_tokens = total.output_tokens.saturating_add(next.output_tokens);
    total.reasoning_tokens = total.reasoning_tokens.saturating_add(next.reasoning_tokens);
    total.total_tokens = total.total_tokens.saturating_add(next.total_tokens);
}

pub(crate) fn add_delegated_cost(total: &mut Cost, next: Cost) {
    total.input = total.input.saturating_add(next.input);
    total.output = total.output.saturating_add(next.output);
    total.reasoning = total.reasoning.saturating_add(next.reasoning);
    total.cache_read = total.cache_read.saturating_add(next.cache_read);
    total.cache_write = total.cache_write.saturating_add(next.cache_write);
    let remainder = u64::from(total.total_picodollars_remainder)
        .saturating_add(u64::from(next.total_picodollars_remainder));
    total.total = total
        .total
        .saturating_add(next.total)
        .saturating_add(remainder / u64::from(PICODOLLARS_PER_MICRODOLLAR));
    total.total_picodollars_remainder = (remainder % u64::from(PICODOLLARS_PER_MICRODOLLAR)) as u32;
}

/// Increment of a cumulative token snapshot. Saturating, so a record that was
/// somehow rolled back can never underflow the root ledger.
pub(crate) fn subtract_usage(total: Usage, mirrored: Usage) -> Usage {
    Usage {
        input_tokens: total.input_tokens.saturating_sub(mirrored.input_tokens),
        cache_read_tokens: total
            .cache_read_tokens
            .saturating_sub(mirrored.cache_read_tokens),
        cache_write_tokens: total
            .cache_write_tokens
            .saturating_sub(mirrored.cache_write_tokens),
        cache_write_1h_tokens: total
            .cache_write_1h_tokens
            .saturating_sub(mirrored.cache_write_1h_tokens),
        output_tokens: total.output_tokens.saturating_sub(mirrored.output_tokens),
        reasoning_tokens: total
            .reasoning_tokens
            .saturating_sub(mirrored.reasoning_tokens),
        total_tokens: total.total_tokens.saturating_sub(mirrored.total_tokens),
    }
}

/// Increment of a cumulative cost snapshot.
pub(crate) fn subtract_cost(total: Cost, mirrored: Cost) -> Cost {
    let scale = u128::from(PICODOLLARS_PER_MICRODOLLAR);
    let delta = (u128::from(total.total) * scale + u128::from(total.total_picodollars_remainder))
        .saturating_sub(
            u128::from(mirrored.total) * scale + u128::from(mirrored.total_picodollars_remainder),
        );
    Cost {
        input: total.input.saturating_sub(mirrored.input),
        output: total.output.saturating_sub(mirrored.output),
        reasoning: total.reasoning.saturating_sub(mirrored.reasoning),
        cache_read: total.cache_read.saturating_sub(mirrored.cache_read),
        cache_write: total.cache_write.saturating_sub(mirrored.cache_write),
        total: (delta / scale) as u64,
        total_picodollars_remainder: (delta % scale) as u32,
    }
}

pub(super) fn delegation_usage_tokens(usage: &Usage) -> u64 {
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

pub(super) fn bounded_text_to(text: &str, limit: usize) -> String {
    const SUFFIX: &str = "\n...[truncated]";
    if text.len() <= limit {
        return text.to_owned();
    }
    let suffix = if limit >= SUFFIX.len() { SUFFIX } else { "" };
    let mut end = limit.saturating_sub(suffix.len()).min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &text[..end], suffix)
}

pub(super) fn bounded_text(text: &str) -> String {
    bounded_text_to(text, MAX_PROVENANCE_TEXT_BYTES)
}

/// Reduce parsed tool arguments to a bounded single-line summary of
/// `key=value` scalar pairs, collapsing whitespace so the value renders on
/// one picker row. Non-scalar arguments are summarized as their type.
pub(super) fn tool_args_summary(args: &serde_json::Value) -> String {
    let flatten = |value: &str| {
        let mut collapsed = String::with_capacity(value.len());
        let mut previous_was_space = true;
        for character in value.chars() {
            if character.is_whitespace() {
                if !previous_was_space {
                    collapsed.push(' ');
                    previous_was_space = true;
                }
            } else {
                collapsed.push(character);
                previous_was_space = false;
            }
        }
        collapsed.trim_end().to_owned()
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(object) = args.as_object() {
        for (key, value) in object {
            let rendered = match value {
                serde_json::Value::String(text) => flatten(text),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                serde_json::Value::Null => continue,
                _ => flatten(&value.to_string()),
            };
            parts.push(format!("{key}={rendered}"));
            if parts.join(" ").len() > MAX_TOOL_ARGS_SUMMARY_BYTES {
                parts.pop();
                break;
            }
        }
    }
    let mut summary = parts.join(" ");
    if summary.len() > MAX_TOOL_ARGS_SUMMARY_BYTES {
        summary = bounded_text_to(&summary, MAX_TOOL_ARGS_SUMMARY_BYTES);
    }
    summary.replace('\n', " ")
}

pub(super) fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(super) fn create_private_team_directory(
    parent: &Path,
) -> Result<Arc<secure_fs::PrivateDirectory>, DelegationError> {
    let parent = std::path::absolute(parent)?;
    Ok(Arc::new(secure_fs::create_bound_private_directory(
        &parent, "team-",
    )?))
}

pub(super) fn cleanup_failed_team_activation(
    team_directory: &secure_fs::PrivateDirectory,
) -> Result<(), String> {
    let mut failures = Vec::new();
    let journal = team_directory.path().join("provenance.jsonl");
    if let Err(error) = team_directory.remove_regular_file_if_exists(&journal) {
        failures.push(format!("remove provenance journal: {error}"));
    }
    if let Err(error) = team_directory.remove_empty_if_exists() {
        failures.push(format!("remove team directory: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
