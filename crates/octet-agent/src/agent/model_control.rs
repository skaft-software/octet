//! In-place, durable idle selection. Does not retire executable extensions.
use super::*;

impl Agent {
    /// Select a resolved host route between runs, preserving the session and tools.
    /// Delegation's model template is currently immutable; refuse rather than
    /// leave children silently inheriting the previous route.
    pub fn select_model_at_idle(
        &mut self,
        model: Model,
        reasoning: ReasoningConfig,
        reasoning_label: String,
    ) -> Result<(), AgentError> {
        if self.delegation.is_some()
            && (model.spec.id != self.model.spec.id || model.endpoint.id != self.model.endpoint.id)
        {
            return Err(AgentError::Delegation(
                "model selection requires an updated delegation model template".into(),
            ));
        }
        require_ultra_observation(
            &reasoning,
            self.delegation.is_some() || self.ultra_observation_managed,
        )?;
        octet_ai::responses::validate_responses_input(
            &model,
            &ResponsesInput::default(),
            &reasoning,
            false,
        )?;
        resolve_service_tier(&model, self.service_tier)?;
        if self.auto_compaction_mode == AgentCompactionMode::NativeResponses
            && (model.spec.protocol != Protocol::OpenAiResponses
                || self
                    .session
                    .responses_replay_snapshot(&model.endpoint.id, &model.spec.id)?
                    .is_none())
        {
            return Err(AgentError::InvalidCompactionPolicy(
                "selected model has no complete route-affine native Responses replay".into(),
            ));
        }
        self.cache_warmer
            .cancel(&mut self.session, "model selection changed")?;
        if model.responses_features().reasoning_effort_updates {
            persist_reasoning_selection(&mut self.session, &model, &reasoning)?;
        }
        // Commit before publishing selection. A failed append is not success.
        self.session.append(EntryValue::Config {
            model: Some(model.spec.id.0.clone()),
            reasoning: Some(reasoning_label),
            reasoning_mode: Some(
                match self.reasoning_mode {
                    ReasoningMode::Standard => "standard",
                    ReasoningMode::Pro => "pro",
                }
                .into(),
            ),
        })?;
        self.max_output_tokens = model.spec.limits.max_output_tokens;
        self.model = model;
        self.reasoning = reasoning;
        if let Some(binding) = &self.delegation {
            binding.update_reasoning(self.reasoning.clone());
        }
        self.sync_delegation_runtime_settings();
        Ok(())
    }
}
