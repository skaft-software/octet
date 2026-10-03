//! Configuring V2 delegation and the delegated-session surface on an `Agent`.

use super::*;

impl Agent {
    /// Enables the bounded host-side V2 collaboration runtime.
    ///
    /// Child agents inherit the resolved model, sandbox, approved extension
    /// tools, reasoning, compaction, completion, and cost settings present at
    /// this idle boundary. Each child receives an isolated durable session.
    pub fn enable_v2_delegation(
        &mut self,
        config: DelegationConfig,
    ) -> Result<std::path::PathBuf, DelegationError> {
        self.enable_v2_delegation_with_surface(config, true)
    }

    /// Enables the bounded V2 runtime without exposing native root
    /// collaboration tools. Product layers use this when an extension owns the
    /// user-facing orchestration and observation surface.
    pub fn enable_v2_delegation_extension_only(
        &mut self,
        config: DelegationConfig,
    ) -> Result<std::path::PathBuf, DelegationError> {
        self.enable_v2_delegation_with_surface(config, false)
    }

    /// Installs or refreshes the host-owned configured-model routing service.
    pub fn set_delegation_model_resolver(
        &mut self,
        resolver: Arc<dyn crate::delegation::AgentModelResolver>,
    ) {
        if let Some(binding) = &self.delegation {
            binding.set_model_resolver(resolver.clone());
        }
        self.delegation_model_resolver = Some(resolver);
    }

    pub(super) fn enable_v2_delegation_with_surface(
        &mut self,
        config: DelegationConfig,
        root_tools: bool,
    ) -> Result<std::path::PathBuf, DelegationError> {
        if self.delegation.is_some() {
            return Err(DelegationError::AlreadyEnabled);
        }
        let template = DelegationTemplate {
            model_resolver: std::sync::RwLock::new(self.delegation_model_resolver.clone()),
            client: self.client.clone(),
            model: self.model.clone(),
            base_system: std::sync::RwLock::new(self.system.clone()),
            sandbox: self.sandbox.clone(),
            effect_broker: self.effect_broker.clone(),
            extensions: self.extensions.clone(),
            max_turns: self.max_turns,
            reasoning: std::sync::RwLock::new(self.reasoning.clone()),
            reasoning_mode: self.reasoning_mode,
            cache_retention: self.cache_retention,
            runtime: std::sync::RwLock::new(self.delegation_runtime_settings()),
        };
        let binding = enable_root_delegation(self, config, template, root_tools)?;
        let team_directory = binding.team_directory().to_path_buf();
        self.delegation = Some(binding);
        Ok(team_directory)
    }

    /// Returns the authoritative host count of active delegated workers.
    ///
    /// Returns zero when delegation is not enabled; frontend roster snapshots
    /// are not consulted.
    pub fn active_delegated_worker_count(&self) -> usize {
        self.delegation
            .as_ref()
            .map_or(0, DelegationBinding::active_worker_count)
    }

    /// Returns the private team directory when V2 delegation is enabled.
    pub fn delegation_team_directory(&self) -> Option<&std::path::Path> {
        self.delegation
            .as_ref()
            .map(DelegationBinding::team_directory)
    }

    /// Opens one exact child transcript from this agent's current delegation
    /// team as a read-only session. Opaque references from another parent are
    /// never resolved.
    pub fn open_delegated_session_reference(
        &self,
        extension_principal: &str,
        reference: &str,
    ) -> Result<Option<Session>, AgentError> {
        let Some(binding) = self.delegation.as_ref() else {
            return Ok(None);
        };
        binding.open_session_reference(extension_principal, reference)
    }

    /// Binds an executable extension's negotiated child-session service to
    /// this root agent's V2 delegation manager.
    pub fn bind_extension_agent_sessions(
        &self,
        process: &ExtensionProcess,
    ) -> Result<bool, AgentError> {
        if !process.supports_feature(EXTENSION_FEATURE_AGENT_SESSIONS) {
            return Ok(false);
        }
        let binding = self.delegation.as_ref().ok_or_else(|| {
            AgentError::Delegation(
                "an extension negotiated agent_sessions before V2 delegation was enabled".into(),
            )
        })?;
        let service = binding
            .extension_service(
                process.agent_session_principal(),
                self.session_id.clone(),
                self.resource_owner.clone(),
            )
            .map_err(AgentError::Delegation)?;
        process
            .bind_agent_session_service(service)
            .map_err(|error| AgentError::Delegation(error.to_string()))?;
        Ok(true)
    }

    pub(super) fn delegation_runtime_settings(&self) -> DelegationRuntimeSettings {
        DelegationRuntimeSettings {
            compaction_model: self.compaction_model.clone(),
            auto_compaction_mode: self.auto_compaction_mode,
            auto_compaction_threshold: self.compaction_threshold_fraction,
            compaction_keep_recent_tokens: self.compaction_keep_recent_tokens,
            completion_policy: self.completion_policy,
            output_modalities: self.output_modalities.clone(),
            max_output_tokens: self.max_output_tokens,
            max_session_tokens: self.max_session_tokens,
            max_session_cost_microdollars: self.max_session_cost_microdollars,
            cache_warming_mode: self.cache_warmer.mode_control(),
            provider_retries_enabled: self.provider_retries_enabled,
            max_network_wait: self.max_network_wait,
            tool_schema_budget_bytes: self.tool_schema_budget_bytes,
        }
    }

    pub(super) fn sync_delegation_runtime_settings(&self) {
        if let Some(binding) = &self.delegation {
            binding.update_runtime_settings(self.delegation_runtime_settings());
        }
    }

    pub(crate) fn install_delegation_tools(&mut self, tools: Vec<Arc<dyn Tool>>) {
        for tool in tools {
            self.extensions.tool_arc(tool);
        }
    }

    pub(crate) fn set_delegation_binding(
        &mut self,
        binding: DelegationBinding,
    ) -> Result<(), DelegationError> {
        if self.delegation.is_some() {
            return Err(DelegationError::AlreadyEnabled);
        }
        self.delegation = Some(binding);
        Ok(())
    }

    pub(crate) fn mark_ultra_observation_managed(&mut self) {
        self.ultra_observation_managed = true;
    }
}
