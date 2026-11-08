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
            model: std::sync::RwLock::new(self.model.clone()),
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

    /// Subscribes to the current root session's latest delegation telemetry.
    ///
    /// The independent, coalescing stream remains live between runs and can be
    /// used alongside run events. A fresh snapshot is published on subscription;
    /// intermediate snapshots may be skipped. It is observational only and
    /// does not claim or acknowledge a worker result. Child agents and agents
    /// without delegation return `None`.
    ///
    /// A receiver is bound to this agent's delegation manager, not a frontend's
    /// currently selected session. Frontends must discard it and subscribe to
    /// the replacement agent when the owner or runtime changes; events from an
    /// old receiver must not be applied to the replacement session.
    pub fn delegation_telemetry(
        &self,
    ) -> Option<watch::Receiver<Option<DelegationTelemetrySnapshot>>> {
        self.delegation
            .as_ref()
            .and_then(DelegationBinding::telemetry_receiver)
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

    /// Returns how many delegated workers are executing or hold accepted work
    /// they have not started.
    ///
    /// Settled workers (completed, limit reached, failed, timed out or
    /// interrupted) still have a live task waiting for a follow-up, but
    /// replacing this agent parks them with their outcome and durable session
    /// retained, so they are not counted here.
    pub fn busy_delegated_worker_count(&self) -> usize {
        self.delegation
            .as_ref()
            .map_or(0, DelegationBinding::busy_worker_count)
    }

    /// Parks every delegated worker before this agent's owner is replaced.
    ///
    /// Workers stop through their cancellation path and keep their durable
    /// child sessions, outcomes and accepted work: undelivered tasks reattach
    /// at the next owning run, delivered work is never replayed, and a settled
    /// or interrupted worker resumes through an explicit follow-up. Returns
    /// how many worker tasks are still live when `timeout` elapses.
    pub async fn detach_delegated_workers(&self, timeout: std::time::Duration) -> usize {
        match &self.delegation {
            Some(binding) => binding.detach_workers(timeout).await,
            None => 0,
        }
    }

    /// Reactivates delegation after [`Self::detach_delegated_workers`] when
    /// this agent stays the owner (the replacement did not happen), so parked
    /// workers are reachable again before the next run.
    pub fn reattach_delegated_workers(&self) -> Result<(), AgentError> {
        match &self.delegation {
            Some(binding) => binding.reattach_workers(),
            None => Ok(()),
        }
    }

    /// Replaces an idle root session with a freshly owned delegation runtime.
    ///
    /// The caller must first park workers with [`Self::detach_delegated_workers`].
    /// All final child usage is persisted to the old session before replacement.
    /// `rebind` installs the new negotiated services without restarting extension
    /// processes; if it fails, the old session/runtime is restored and `rebind`
    /// is called again for that owner. The caller reattaches parked workers on
    /// failure. The ordinary pointer-retargeting API continues to refuse V2.
    pub fn replace_session_rebuilding_delegation_at_idle(
        &mut self,
        session: Session,
        mut rebind: impl FnMut(&Self) -> Result<(), Box<AgentError>>,
    ) -> Result<(), AgentError> {
        let Some(binding) = self.delegation.as_ref() else {
            return self.replace_session_at_idle(session);
        };
        let delegated = binding.delegated_usage_records();
        // Everything that can fail while writing only to the old owner runs
        // before a replacement runtime exists. Creating the runtime first meant
        // a refused cache warmer or a failed usage record stranded a team
        // directory, provenance journal and fleet artifact for a session that
        // never became current.
        self.prepare_session_replacement()?;
        for delegated in delegated {
            mirror_delegated_uncertainty(
                &mut self.session,
                &self.model,
                &delegated.agent_id,
                delegated.usage_uncertain,
                delegated.usage_exposure,
            )?;
            record_delegated_usage_once(
                &mut self.session,
                DelegatedUsage {
                    agent_id: delegated.agent_id,
                    turn_count: delegated.turn_count,
                    tool_call_count: delegated.tool_call_count,
                    endpoint: self.model.endpoint.id.clone(),
                    model: self.model.spec.id.clone(),
                    usage: delegated.usage,
                    cost: delegated.cost,
                },
            )?;
        }
        let replacement = self
            .delegation
            .as_ref()
            .ok_or_else(|| {
                AgentError::Delegation("delegation was disabled during session replacement".into())
            })?
            .rebuild_for_session(session.path())
            .map_err(|error| AgentError::Delegation(error.to_string()))?;
        replacement.set_span_context(self.telemetry_context().clone());
        // Keep the previous owner and Bash lease alive until every negotiated
        // service has rebound. No await or setup callback runs during this swap.
        let owner = session.resource_owner_key();
        let old_session = std::mem::replace(&mut self.session, session);
        let old_session_id = std::mem::replace(&mut self.session_id, owner.clone());
        let old_owner = std::mem::replace(&mut self.resource_owner, owner.clone());
        let old_bash = std::mem::replace(&mut self.bash_owner, BashOwnerLease::acquire(&owner));
        let old_lifecycle = self.last_run_lifecycle.take();
        let old_display = self.prompt_display_text.take();
        let old_binding = self.delegation.replace(replacement);
        self.refresh_delegation_tool_bindings();
        if let Err(error) = rebind(self) {
            self.session = old_session;
            self.session_id = old_session_id;
            self.resource_owner = old_owner;
            self.bash_owner = old_bash;
            self.last_run_lifecycle = old_lifecycle;
            self.prompt_display_text = old_display;
            let discarded = self
                .delegation
                .replace(old_binding.expect("a delegation binding was installed before the swap"));
            self.refresh_delegation_tool_bindings();
            // A replacement runtime that never became current must not leave a
            // team directory, provenance journal or fleet artifact behind.
            let discard_error =
                discarded.and_then(|binding| binding.discard_activation_artifacts().err());
            if let Err(rollback) = rebind(self) {
                return Err(AgentError::Delegation(format!(
                    "session rebind failed ({error}); old-owner rebind failed ({rollback})"
                )));
            }
            return Err(match discard_error {
                Some(discard) => AgentError::Delegation(format!(
                    "{error}; the unused replacement runtime could not be removed ({discard})"
                )),
                None => *error,
            });
        }
        Ok(())
    }

    fn refresh_delegation_tool_bindings(&mut self) {
        if let Some(binding) = &self.delegation {
            for replacement in binding.root_tools() {
                let name = replacement.definition().name;
                // Replace only existing static root tools; policy-filtered
                // tools stay absent, with the catalog and active set unchanged.
                if let Some(tool) = self
                    .extensions
                    .tools
                    .iter_mut()
                    .find(|tool| tool.definition().name == name)
                {
                    *tool = replacement;
                }
            }
        }
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
