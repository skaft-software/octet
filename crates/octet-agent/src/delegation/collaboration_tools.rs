//! The collaboration tools a delegating agent calls, and the root and child instructions.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum CollaborationToolKind {
    Spawn,
    FollowUp,
    SendMessage,
    Wait,
    List,
    Interrupt,
}

impl CollaborationToolKind {
    pub(super) const ALL: [Self; 6] = [
        Self::Spawn,
        Self::FollowUp,
        Self::SendMessage,
        Self::Wait,
        Self::List,
        Self::Interrupt,
    ];

    pub(super) fn definition(self) -> ToolDef {
        match self {
            Self::Spawn => tool_def(
                "spawn_agent",
                "Spawn an isolated child agent for an independent task. Returns immediately; use wait_agent or list_agents for status.",
                json!({
                    "type": "object",
                    "properties": {
                        "task_name": {"type": "string", "description": "Unique lowercase task name under this agent (letters, digits, underscore, hyphen)."},
                        "message": {"type": "string", "description": "Complete task and relevant context for the child."}
                    },
                    "required": ["task_name", "message"],
                    "additionalProperties": false
                }),
            ),
            Self::FollowUp => tool_def(
                "followup_task",
                "Send additional work to a delegated agent. It is queued after an active run or starts a new run when idle.",
                target_message_schema(),
            ),
            Self::SendMessage => tool_def(
                "send_message",
                "Send information to another agent. Active agents receive steering; idle agents receive it with their next task.",
                target_message_schema(),
            ),
            Self::Wait => tool_def(
                "wait_agent",
                "Wait for delegated-agent messages or status changes. Returns immediately if this agent has messages or no descendants are running.",
                json!({
                    "type": "object",
                    "properties": {
                        "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TOOL_TIMEOUT_MS, "description": "Maximum wait in milliseconds (default 30000)."}
                    },
                    "additionalProperties": false
                }),
            ),
            Self::List => tool_def(
                "list_agents",
                "List every agent in this delegation team, including durable session paths and current status.",
                json!({"type": "object", "properties": {}, "additionalProperties": false}),
            ),
            Self::Interrupt => tool_def(
                "interrupt_agent",
                "Interrupt a running descendant agent and propagate cancellation to its descendants.",
                json!({
                    "type": "object",
                    "properties": {
                        "target": {"type": "string", "description": "Agent ID or absolute delegation path."}
                    },
                    "required": ["target"],
                    "additionalProperties": false
                }),
            ),
        }
    }
}

pub(super) struct CollaborationTool {
    pub(super) manager: Weak<DelegationManager>,
    pub(super) owner: AgentIdentity,
    pub(super) kind: CollaborationToolKind,
}

#[async_trait::async_trait]
impl Tool for CollaborationTool {
    fn definition(&self) -> ToolDef {
        self.kind.definition()
    }

    fn effect(&self, _args: &Value, _ctx: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Delegation)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<ToolOutput, ToolError> {
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| ToolError::new("delegation team is no longer available"))?;
        if matches!(self.kind, CollaborationToolKind::Wait) {
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(30_000)
                .clamp(1, MAX_TOOL_TIMEOUT_MS);
            let wait = manager
                .wait(
                    &self.owner,
                    Duration::from_millis(timeout_ms),
                    &ctx.cancellation,
                    ctx.sandbox.max_output_bytes,
                )
                .await
                .map_err(ToolError::new)?;
            let text = match serde_json::to_string(&wait.value) {
                Ok(text) => text,
                Err(error) => {
                    if let Some(delivery_id) = wait.delivery_id {
                        manager.resolve_mailbox_delivery(&self.owner.id, delivery_id, false);
                    }
                    return Err(ToolError::new(format!(
                        "could not encode collaboration result: {error}"
                    )));
                }
            };
            let mut output = ToolOutput::new(text);
            if let Some(delivery_id) = wait.delivery_id {
                let commit_manager = self.manager.clone();
                let rollback_manager = self.manager.clone();
                let commit_owner = self.owner.id.clone();
                let rollback_owner = self.owner.id.clone();
                output = output.with_delivery_commit(
                    move || {
                        if let Some(manager) = commit_manager.upgrade() {
                            manager.resolve_mailbox_delivery(&commit_owner, delivery_id, true);
                        }
                    },
                    move || {
                        if let Some(manager) = rollback_manager.upgrade() {
                            manager.resolve_mailbox_delivery(&rollback_owner, delivery_id, false);
                        }
                    },
                );
            }
            return Ok(output);
        }

        let value = match self.kind {
            CollaborationToolKind::Spawn => {
                let request = SpawnRequest {
                    task_name: required_string(&args, "task_name")?,
                    display_task_name: None,
                    message: required_string(&args, "message")?,
                    extension_policy: None,
                    extension_provenance: None,
                };
                manager.spawn(&self.owner, request)
            }
            CollaborationToolKind::FollowUp => {
                let request = FollowUpRequest {
                    target: required_string(&args, "target")?,
                    message: required_string(&args, "message")?,
                };
                manager.follow_up(&self.owner, request).await
            }
            CollaborationToolKind::SendMessage => {
                let target = required_string(&args, "target")?;
                let message = required_string(&args, "message")?;
                manager.send_message(&self.owner, &target, message).await
            }
            CollaborationToolKind::Wait => unreachable!("wait returned above"),
            CollaborationToolKind::List => manager.list_value_for(&self.owner),
            CollaborationToolKind::Interrupt => {
                let target = required_string(&args, "target")?;
                manager.interrupt(&self.owner, &target).await
            }
        }
        .map_err(ToolError::new)?;
        serde_json::to_string(&value)
            .map(ToolOutput::new)
            .map_err(|error| {
                ToolError::new(format!("could not encode collaboration result: {error}"))
            })
    }
}

pub(crate) fn enable_root_delegation(
    agent: &mut Agent,
    config: DelegationConfig,
    template: DelegationTemplate,
    root_tools: bool,
) -> Result<DelegationBinding, DelegationError> {
    if root_tools {
        for name in &COLLABORATION_TOOL_NAMES {
            if agent
                .registered_tool_names()
                .iter()
                .any(|registered| registered == name)
            {
                return Err(DelegationError::DuplicateTool((*name).into()));
            }
        }
    }
    let manager = DelegationManager::create(config, template, agent.session().path(), root_tools)?;
    // Row 3.5: delegated child runs are observed with the owner's explicit
    // context. The observer is inert unless the host installed one.
    manager.set_span_context(agent.telemetry_context().clone());
    let binding = manager.root_binding();
    if manager.root_tools {
        agent.append_system_instructions(binding.system_instructions().to_owned());
        agent.install_delegation_tools(manager.tools(&binding.identity));
    }
    Ok(binding)
}

pub(super) fn root_instructions(config: &DelegationConfig) -> String {
    let proactive = match config.mode {
        DelegationMode::Available => {
            "Delegation is available when the user or task explicitly benefits from separate agents."
        }
        DelegationMode::Proactive => {
            "Use sub-agents proactively when parallel work would materially improve speed or quality."
        }
    };
    format!(
        "<octet_multi_agent_v2>\nYou are {ROOT_AGENT_PATH}, the root of a bounded agent team. {proactive}\nUse spawn_agent for independent work, send_message for timely context, followup_task for additional work, wait_agent/list_agents to coordinate, and interrupt_agent to stop obsolete work. Integrate and verify child results yourself; do not present unverified child output as fact. Delegation is bounded to {} concurrent agents including you, depth {}, and {} total agents.\n</octet_multi_agent_v2>",
        config.limits.max_concurrent_agents,
        config.limits.max_depth,
        config.limits.max_total_agents
    )
}

pub(super) fn child_instructions(
    identity: &AgentIdentity,
    parent_path: &str,
    limits: &DelegationLimits,
) -> String {
    format!(
        "<octet_multi_agent_v2>\nYou are {}, delegated by {}. Complete the assigned task independently and return a concise, evidence-based result. Use send_message for information your parent needs before completion. You may spawn useful independent sub-agents within the remaining bounds (max depth {}, max {} concurrent including root). Coordinate with wait_agent/list_agents and interrupt obsolete descendants. Your final response is delivered automatically to your parent.\n</octet_multi_agent_v2>",
        identity.path, parent_path, limits.max_depth, limits.max_concurrent_agents
    )
}
