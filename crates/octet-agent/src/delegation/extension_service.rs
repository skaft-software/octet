//! Delegation on behalf of extensions: session policy, the binding and the extension delegation service.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExtensionAgentSessionPolicy {
    #[serde(default)]
    pub(crate) model_selection: Option<AgentModelSelection>,
    #[serde(default)]
    pub(crate) resolved_model: Option<AgentModelSelection>,
    #[serde(default)]
    pub(crate) resolved_reasoning: Option<octet_ai::ReasoningConfig>,
    /// Child tool scope: a non-empty duplicate-free subset of the standard
    /// tools `read`, `search`, `edit`, `write`, and `bash`.
    pub(crate) tools: Vec<String>,
    /// Child depth relative to the root; extension children are exactly one.
    pub(crate) max_depth: usize,
    /// Maximum active children this owner may hold; host cap is eight.
    pub(crate) max_concurrent_children: usize,
    /// Child turn budget. `None` inherits the parent session limit exactly
    /// (unlimited parents stay unlimited).
    #[serde(default)]
    pub(crate) max_turns: Option<u64>,
    /// Optional child token ceiling. `None` inherits the parent session
    /// limit exactly (unlimited parents stay unlimited).
    #[serde(default)]
    pub(crate) max_tokens: Option<u64>,
    /// Optional hard whole-microdollar child cost ceiling. `None` imposes no
    /// child-specific ceiling; the parent session ceiling still applies.
    #[serde(default)]
    pub(crate) max_cost_microdollars: Option<u64>,
    /// Bounded worker result size in UTF-8 bytes.
    pub(crate) max_output_bytes: usize,
    /// Optional hard worker wall clock in milliseconds. `None` runs without a
    /// wall-clock kill; the worker can still be interrupted or stopped.
    #[serde(default)]
    pub(crate) timeout_ms: Option<u64>,
}

pub(super) fn resolved_model_json(policy: Option<&ExtensionAgentSessionPolicy>) -> Value {
    match policy.and_then(|p| p.resolved_model.as_ref().map(|m| (p, m))) {
        Some((policy, model)) => {
            json!({"provider": model.provider, "model": model.model, "reasoning": policy.resolved_reasoning})
        }
        None => Value::Null,
    }
}

/// Project effective policy without exposing the internal recovery encoding.
pub(super) fn public_policy_json(policy: Option<&ExtensionAgentSessionPolicy>) -> Value {
    let Some(policy) = policy else {
        return Value::Null;
    };
    let mut value = serde_json::to_value(policy).expect("policy is JSON serializable");
    let object = value.as_object_mut().expect("policy serializes as object");
    object.remove("resolved_reasoning");
    object.insert("resolved_model".into(), resolved_model_json(Some(policy)));
    value
}

pub(super) fn child_orchestration_provenance(
    extension_policy: Option<&ExtensionAgentSessionPolicy>,
) -> DelegationOrchestrationProvenance {
    let mut provenance =
        DelegationOrchestrationProvenance::all(DelegationPolicySource::ParentInherited);
    if extension_policy.is_some() {
        // Extension-owned children receive a host-validated standard-tool
        // snapshot and explicit child-run limits. The sandbox, broker,
        // environment, cwd, and executable trust stay parent-owned.
        provenance.tool_scope = DelegationPolicySource::ChildOverride;
        provenance.execution_limits = DelegationPolicySource::ChildOverride;
    }
    provenance
}

impl ExtensionAgentSessionPolicy {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(selection) = &self.model_selection {
            if selection.provider != "inherit" && selection.model == "inherit" {
                return Err("unsupported_model: provider requires explicit model".into());
            }
            if [&selection.provider, &selection.model, &selection.reasoning]
                .iter()
                .any(|value| {
                    value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
                })
            {
                return Err("unsupported_model: invalid selection identifier".into());
            }
        }
        if self.tools.is_empty() || self.tools.len() > EXTENSION_CHILD_TOOLS.len() {
            return Err(
                "child tools must be a non-empty subset of the standard child tool names".into(),
            );
        }
        let tools = self.tools.iter().collect::<BTreeSet<_>>();
        if tools.len() != self.tools.len()
            || self
                .tools
                .iter()
                .any(|tool| !EXTENSION_CHILD_TOOLS.contains(&tool.as_str()))
        {
            return Err(
                "child tools must be a duplicate-free subset of read, search, edit, write, and bash"
                    .into(),
            );
        }
        if self.max_depth != 1 {
            return Err("extension child max_depth must be exactly 1".into());
        }
        if self.max_concurrent_children == 0
            || self.max_concurrent_children > MAX_EXTENSION_ACTIVE_CHILDREN
        {
            return Err(format!(
                "extension child concurrency must be between 1 and {MAX_EXTENSION_ACTIVE_CHILDREN}"
            ));
        }
        if self
            .max_turns
            .is_some_and(|max_turns| !(1..=MAX_EXTENSION_TURNS).contains(&max_turns))
        {
            return Err(format!(
                "extension child max_turns must be null or between 1 and {MAX_EXTENSION_TURNS}"
            ));
        }
        if self
            .max_tokens
            .is_some_and(|max_tokens| !(1_000..=64_000).contains(&max_tokens))
        {
            return Err("extension child max_tokens must be null or between 1000 and 64000".into());
        }
        if self
            .max_cost_microdollars
            .is_some_and(|max_cost| !(1..=MAX_EXTENSION_COST_MICRODOLLARS).contains(&max_cost))
        {
            return Err(format!(
                "extension child max_cost_microdollars must be null or between 1 and {}",
                MAX_EXTENSION_COST_MICRODOLLARS
            ));
        }
        if !(512..=16 * 1024).contains(&self.max_output_bytes) {
            return Err("extension child max_output_bytes must be between 512 and 16384".into());
        }
        if self
            .timeout_ms
            .is_some_and(|timeout| !(5_000..=MAX_EXTENSION_TIMEOUT_MS).contains(&timeout))
        {
            return Err(format!(
                "extension child timeout_ms must be null or between 5000 and {MAX_EXTENSION_TIMEOUT_MS}"
            ));
        }
        Ok(())
    }
}

pub(crate) struct ExtensionDelegationSpawnRequest {
    pub(crate) task_name: String,
    pub(crate) profile: Option<String>,
    pub(crate) fingerprint: Option<String>,
    pub(crate) message: String,
    pub(crate) idempotency_key: String,
    pub(crate) policy: ExtensionAgentSessionPolicy,
}

#[derive(Clone)]
pub(crate) struct ExtensionDelegationService {
    pub(super) manager: Weak<DelegationManager>,
    pub(super) principal: Arc<str>,
    pub(super) parent_session_id: Arc<str>,
    pub(super) state: Arc<Mutex<ExtensionDelegationState>>,
}

#[derive(Default)]
pub(super) struct ExtensionDelegationState {
    pub(super) owners: BTreeMap<String, ExtensionDelegationOwnerState>,
}

#[derive(Default)]
pub(super) struct ExtensionDelegationOwnerState {
    pub(super) owned_agents: BTreeSet<String>,
    pub(super) idempotent_spawns: BTreeMap<String, IdempotentExtensionSpawn>,
}

pub(super) struct IdempotentExtensionSpawn {
    pub(super) task_name: String,
    pub(super) profile: Option<String>,
    pub(super) fingerprint: Option<String>,
    pub(super) message_sha256: String,
    pub(super) policy: ExtensionAgentSessionPolicy,
    pub(super) result: Value,
}

/// A session-owned worker re-discovered by a durable extension spawn
/// idempotency key after the owning run (or process) ended.
pub(super) struct ExtensionDurableSpawn {
    pub(super) task_name: String,
    pub(super) profile: Option<String>,
    pub(super) fingerprint: Option<String>,
    pub(super) policy: Option<ExtensionAgentSessionPolicy>,
    pub(super) resource_owner: Option<String>,
    pub(super) message_sha256: Option<String>,
    pub(super) result: Value,
}

impl DelegationBinding {
    pub(crate) fn team_directory(&self) -> &Path {
        &self.manager.team_directory
    }

    pub(crate) fn open_session_reference(
        &self,
        extension_principal: &str,
        reference: &str,
    ) -> Result<Option<Session>, AgentError> {
        if !reference.starts_with("agent-session:") {
            return Ok(None);
        }
        let path = {
            let state = self
                .manager
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .records
                .values()
                .find(|record| {
                    record.extension_principal.as_deref() == Some(extension_principal)
                        && delegated_session_reference(&record.session_path).as_deref()
                            == Some(reference)
                })
                .map(|record| record.session_path.clone())
        };
        let Some(path) = path else {
            return Ok(None);
        };
        let file = secure_fs::open_private_file_for_read(&path)
            .map_err(|error| AgentError::Delegation(error.to_string()))?;
        Session::open_read_only_with_file(path, file)
            .map(Some)
            .map_err(AgentError::Session)
    }

    pub(crate) fn system_instructions(&self) -> &str {
        &self.system_instructions
    }

    pub(crate) fn is_root(&self) -> bool {
        self.identity.id == ROOT_AGENT_ID
    }

    /// Attach the owning root run to the manager's loss-tolerant latest
    /// telemetry stream. Child runs deliberately do not receive this stream.
    pub(crate) fn telemetry_receiver(
        &self,
    ) -> Option<watch::Receiver<Option<DelegationTelemetrySnapshot>>> {
        self.is_root().then(|| self.manager.attach_telemetry())
    }

    pub(crate) fn detach_telemetry(&self) {
        self.manager.detach_telemetry();
    }

    /// Count worker tasks that still own host state, independent of telemetry
    /// or extension presentation (including idle and unwinding workers).
    pub(crate) fn active_worker_count(&self) -> usize {
        self.manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .values()
            .filter(|record| record.live_task)
            .count()
    }

    pub(crate) fn request_shutdown(&self) {
        self.manager.request_shutdown_descendants(&self.identity.id);
    }

    /// Session-scoped detachment at the owning run boundary.
    ///
    /// Replaces run-scoped retirement: the fleet survives the turn, the durable
    /// roster is refreshed, and an explicit `run_detached` provenance record is
    /// written instead of a silent vanish.
    pub(crate) fn detach_run(&self) {
        self.manager.detach_run(&self.identity);
    }

    /// Session-owned launchable-handle resolver for the root owner.
    pub(crate) fn session_handle(&self) -> SessionDelegationHandle {
        SessionDelegationHandle {
            manager: Arc::clone(&self.manager),
        }
    }

    pub(crate) fn delegated_usage_records(&self) -> Vec<DelegatedUsageRecord> {
        self.manager.extension_usage_records(&self.identity.id)
    }

    pub(crate) fn prepare_owning_run(&self) -> Result<(), AgentError> {
        self.manager
            .prepare_owning_run(&self.identity)
            .map_err(AgentError::Delegation)
    }

    pub(crate) fn set_model_resolver(&self, resolver: Arc<dyn AgentModelResolver>) {
        *self
            .manager
            .template
            .model_resolver
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(resolver);
    }

    pub(crate) fn update_base_system(&self, system: String) {
        if self.identity.id == ROOT_AGENT_ID {
            *self
                .manager
                .template
                .base_system
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = system;
        }
    }
    pub(crate) fn update_reasoning(&self, reasoning: octet_ai::ReasoningConfig) {
        if self.identity.id == ROOT_AGENT_ID {
            *self
                .manager
                .template
                .reasoning
                .write()
                .unwrap_or_else(|p| p.into_inner()) = reasoning;
        }
    }

    pub(crate) fn update_runtime_settings(&self, settings: DelegationRuntimeSettings) {
        if self.identity.id == ROOT_AGENT_ID {
            *self
                .manager
                .template
                .runtime
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings;
        }
    }

    pub(crate) fn extension_service(
        &self,
        principal: impl Into<String>,
        parent_session_id: impl Into<String>,
        root_resource_owner: impl Into<String>,
    ) -> Result<ExtensionDelegationService, String> {
        if self.identity.id != ROOT_AGENT_ID {
            return Err("extension delegation service requires the root binding".into());
        }
        let principal = principal.into();
        if principal.trim().is_empty() || principal.len() > 256 {
            return Err("extension delegation principal must be 1..=256 bytes".into());
        }
        let parent_session_id = parent_session_id.into();
        if parent_session_id.trim().is_empty()
            || parent_session_id.len() > 256
            || parent_session_id.chars().any(char::is_whitespace)
        {
            return Err(
                "extension delegation parent session must be a bounded stable identifier".into(),
            );
        }
        let root_resource_owner = root_resource_owner.into();
        ExtensionDelegationService::validate_resource_owner(&root_resource_owner)?;
        let mut service_state = ExtensionDelegationState::default();
        {
            let mut state = self
                .manager
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.root_resource_owner = Some(root_resource_owner.clone());
            // Durable records, not a replay of spawn requests, reconstruct the
            // inspection/control index. Both the extension principal and the
            // parent resource-owner fence must match the restored tree.
            for record in state.records.values() {
                if record.extension_principal.as_deref() != Some(principal.as_str()) {
                    continue;
                }
                let Some(owner) = record.extension_resource_owner.as_deref() else {
                    continue;
                };
                if ExtensionDelegationService::validate_resource_owner(owner).is_err() {
                    continue;
                }
                let parent_matches = if owner == root_resource_owner {
                    record.parent_id == ROOT_AGENT_ID
                } else {
                    state
                        .records
                        .get(&record.parent_id)
                        .is_some_and(|parent| parent.resource_owner.as_deref() == Some(owner))
                };
                if parent_matches {
                    service_state
                        .owners
                        .entry(owner.to_owned())
                        .or_default()
                        .owned_agents
                        .insert(record.identity.id.clone());
                }
            }
        }
        Ok(ExtensionDelegationService {
            manager: Arc::downgrade(&self.manager),
            principal: Arc::from(principal),
            parent_session_id: Arc::from(parent_session_id),
            state: Arc::new(Mutex::new(service_state)),
        })
    }
}

impl ExtensionDelegationService {
    pub(super) fn manager(&self) -> Result<Arc<DelegationManager>, String> {
        self.manager
            .upgrade()
            .ok_or_else(|| "delegation service is no longer available".to_owned())
    }

    pub(super) fn root_identity() -> AgentIdentity {
        AgentIdentity {
            id: ROOT_AGENT_ID.into(),
            path: ROOT_AGENT_PATH.into(),
            depth: 0,
        }
    }

    pub(super) fn owner_identity(
        &self,
        manager: &DelegationManager,
        resource_owner: &str,
    ) -> Result<AgentIdentity, String> {
        Self::validate_resource_owner(resource_owner)?;
        let state = manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.root_resource_owner.as_deref() == Some(resource_owner) {
            return Ok(Self::root_identity());
        }
        state
            .records
            .values()
            .find(|record| record.resource_owner.as_deref() == Some(resource_owner))
            .map(|record| record.identity.clone())
            .ok_or_else(|| "extension resource owner is not an active model session".to_owned())
    }

    pub(super) fn resolve_owned_target(
        &self,
        manager: &DelegationManager,
        resource_owner: &str,
        target: &str,
    ) -> Result<String, String> {
        let owned = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owners
            .get(resource_owner)
            .map(|owner| owner.owned_agents.clone())
            .unwrap_or_default();
        if owned.is_empty() {
            return Err("extension resource owner has no child sessions".into());
        }
        let state = manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owned_paths = owned
            .iter()
            .filter_map(|id| {
                state
                    .records
                    .get(id)
                    .map(|record| record.identity.path.clone())
            })
            .collect::<Vec<_>>();
        let target_id = DelegationManager::resolve_id_locked(&state, target)
            .ok_or_else(|| format!("unknown extension delegation target: {target}"))?;
        let target_path = state
            .records
            .get(&target_id)
            .map(|record| record.identity.path.as_str())
            .ok_or_else(|| format!("unknown extension delegation target: {target}"))?;
        if !owned.contains(&target_id)
            && !owned_paths
                .iter()
                .any(|root| is_descendant_path(target_path, root))
        {
            return Err("extension principal may access only its own child-session trees".into());
        }
        Ok(target_id)
    }

    pub(super) fn owner_task_prefix(&self, resource_owner: &str) -> String {
        extension_owner_task_prefix(&self.principal, resource_owner)
    }

    pub(super) fn validate_resource_owner(resource_owner: &str) -> Result<(), String> {
        if resource_owner.trim().is_empty() || resource_owner.len() > 512 {
            return Err("extension resource owner must be 1..=512 bytes".into());
        }
        Ok(())
    }

    pub(crate) fn shutdown_owner(&self, resource_owner: &str) {
        let roots = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owners
            .get(resource_owner)
            .map(|owner| owner.owned_agents.clone())
            .unwrap_or_default();
        if let Some(manager) = self.manager.upgrade() {
            manager.request_shutdown_agent_trees(&roots);
        }
    }

    pub(crate) fn shutdown_owned(&self) {
        let roots = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owners
            .values()
            .flat_map(|owner| owner.owned_agents.iter().cloned())
            .collect::<BTreeSet<_>>();
        if let Some(manager) = self.manager.upgrade() {
            manager.request_shutdown_agent_trees(&roots);
        }
    }

    pub(crate) fn models(
        &self,
        resource_owner: &str,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Value, String> {
        if !(1..=100).contains(&limit)
            || query.is_some_and(|q| q.len() > 128 || q.chars().any(char::is_control))
        {
            return Err("invalid model discovery bounds".into());
        }
        let manager = self.manager()?;
        let owner = self.owner_identity(&manager, resource_owner)?;
        if owner.depth != 0 {
            return Err("model discovery is root-owner only".into());
        }
        let resolver = manager
            .template
            .model_resolver
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let mut models = if let Some(resolver) = resolver.as_ref() {
            resolver.models(query, limit + 1)?
        } else {
            let resolved = manager.template.resolve_model(None)?;
            let m = &resolved.model.spec;
            vec![AgentModelDescriptor {
                model: resolved.metadata.model,
                provider: resolved.metadata.provider,
                display_name: m.display_name.clone(),
                reasoning: vec![resolved.metadata.reasoning],
                context_window: m.limits.context_window,
                max_output_tokens: m.limits.max_output_tokens,
            }]
        };
        if models.len() > limit + 1
            || models.iter().any(|m| {
                [&m.model, &m.provider]
                    .iter()
                    .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
                    || m.display_name
                        .as_ref()
                        .is_some_and(|name| name.len() > 512 || name.chars().any(char::is_control))
                    || m.reasoning.len() > 32
                    || m.reasoning
                        .iter()
                        .any(|r| r.is_empty() || r.len() > 256 || r.chars().any(char::is_control))
            })
        {
            return Err("configured model discovery exceeded public metadata bounds".into());
        }
        if let Some(query) = query {
            let query = query.to_lowercase();
            models.retain(|m| {
                format!(
                    "{} {} {}",
                    m.model,
                    m.provider,
                    m.display_name.as_deref().unwrap_or("")
                )
                .to_lowercase()
                .contains(&query)
            });
        }
        let truncated = models.len() > limit;
        models.truncate(limit);
        Ok(json!({"models": models, "truncated": truncated}))
    }

    pub(crate) fn spawn(
        &self,
        resource_owner: &str,
        request: ExtensionDelegationSpawnRequest,
    ) -> Result<Value, String> {
        let ExtensionDelegationSpawnRequest {
            task_name,
            profile,
            fingerprint,
            message,
            idempotency_key,
            policy,
        } = request;
        Self::validate_resource_owner(resource_owner)?;
        validate_task_name(&task_name)?;
        if let Some(profile) = profile.as_deref() {
            validate_task_name(profile)
                .map_err(|_| "profile must be a bounded lowercase stable identifier".to_owned())?;
        }
        if fingerprint.as_deref().is_some_and(|fingerprint| {
            fingerprint.len() != 64
                || !fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err("fingerprint must be a lowercase SHA-256 digest".into());
        }
        policy.validate()?;
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 256 {
            return Err("spawn idempotency_key must be 1..=256 bytes".into());
        }
        let message_sha256 = format!("{:x}", Sha256::digest(message.as_bytes()));
        let manager = self.manager()?;
        let reject_spawn = |error: String| {
            manager.publish_external_failure("spawn_rejected", &error);
            error
        };
        let mut service_state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owner_state = service_state
            .owners
            .entry(resource_owner.to_owned())
            .or_default();
        {
            let manager_state = manager
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            owner_state
                .owned_agents
                .retain(|id| manager_state.records.contains_key(id));
            owner_state.idempotent_spawns.retain(|_, spawn| {
                spawn
                    .result
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| manager_state.records.contains_key(id))
            });
        }
        if let Some(existing) = owner_state.idempotent_spawns.get(&idempotency_key) {
            if existing.task_name != task_name
                || existing.profile != profile
                || existing.fingerprint != fingerprint
                || existing.message_sha256 != message_sha256
                || existing.policy != policy
            {
                return Err(reject_spawn(
                    "spawn idempotency_key was reused with different input".into(),
                ));
            }
            return Ok(existing.result.clone());
        }
        // Durable idempotency: the worker survived the owning run (or a
        // restart) as a session-owned record. Re-issue its original result
        // instead of spawning a duplicate worker, and re-arm the fast path.
        if let Some(durable) =
            manager.extension_owned_record(&self.principal, resource_owner, &idempotency_key)
        {
            if durable.task_name != task_name
                || durable.profile != profile
                || durable.fingerprint != fingerprint
                || durable.policy.as_ref() != Some(&policy)
                || durable.resource_owner.as_deref() != Some(resource_owner)
                || durable.message_sha256.as_deref() != Some(message_sha256.as_str())
            {
                return Err(reject_spawn(
                    "spawn idempotency_key was reused with different input".into(),
                ));
            }
            let mut result = durable.result;
            result["task_name"] = Value::String(task_name.clone());
            result["principal"] = Value::String(self.principal.to_string());
            result["resource_owner"] = Value::String(resource_owner.to_owned());
            if let Some(agent_id) = result.get("agent_id").and_then(Value::as_str) {
                owner_state.owned_agents.insert(agent_id.to_owned());
            }
            owner_state.idempotent_spawns.insert(
                idempotency_key,
                IdempotentExtensionSpawn {
                    task_name,
                    profile,
                    fingerprint,
                    message_sha256,
                    policy,
                    result: result.clone(),
                },
            );
            return Ok(result);
        }
        let internal_digest = Sha256::digest(format!("{task_name}\0{idempotency_key}").as_bytes());
        let internal_task_name = format!(
            "{}-task-{}",
            self.owner_task_prefix(resource_owner),
            internal_digest[..6]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let owner = self.owner_identity(&manager, resource_owner)?;
        let rejection = {
            let manager_state = manager
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let active_count = owner_state
                .owned_agents
                .iter()
                .filter_map(|id| manager_state.records.get(id))
                .filter(|record| record.status.is_running())
                .count();
            if active_count >= policy.max_concurrent_children {
                Some(format!(
                    "extension child concurrency limit reached ({})",
                    policy.max_concurrent_children
                ))
            } else if owner_state.owned_agents.len() >= MAX_EXTENSION_OWNED_CHILDREN {
                Some(format!(
                    "extension child total limit reached ({MAX_EXTENSION_OWNED_CHILDREN})"
                ))
            } else {
                None
            }
        };
        if let Some(error) = rejection {
            return Err(reject_spawn(error));
        }
        let mut result = match manager.spawn(
            &owner,
            SpawnRequest {
                task_name: internal_task_name,
                display_task_name: Some(task_name.clone()),
                message,
                extension_policy: Some(policy.clone()),
                extension_provenance: Some(ExtensionSpawnProvenance {
                    parent_session_id: self.parent_session_id.to_string(),
                    principal: self.principal.to_string(),
                    resource_owner: resource_owner.to_owned(),
                    profile: profile.clone(),
                    idempotency_key: idempotency_key.clone(),
                    fingerprint: fingerprint.clone(),
                }),
            },
        ) {
            Ok(result) => result,
            Err(error) => {
                manager.publish_external_failure("spawn_rejected", &error);
                return Err(error);
            }
        };
        let agent_id = result
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "delegation spawn omitted agent_id".to_owned())?;
        let agent_id = agent_id.to_owned();
        result["task_name"] = Value::String(task_name.clone());
        result["principal"] = Value::String(self.principal.to_string());
        result["resource_owner"] = Value::String(resource_owner.to_owned());
        owner_state.owned_agents.insert(agent_id);
        owner_state.idempotent_spawns.insert(
            idempotency_key,
            IdempotentExtensionSpawn {
                task_name,
                profile,
                fingerprint,
                message_sha256,
                policy,
                result: result.clone(),
            },
        );
        Ok(result)
    }

    pub(crate) async fn send_message(
        &self,
        resource_owner: &str,
        target: &str,
        message: String,
    ) -> Result<Value, String> {
        let manager = self.manager()?;
        let owner = self.owner_identity(&manager, resource_owner)?;
        let target = self.resolve_owned_target(&manager, resource_owner, target)?;
        manager.send_message(&owner, &target, message).await
    }

    pub(crate) async fn follow_up(
        &self,
        resource_owner: &str,
        target: &str,
        message: String,
    ) -> Result<Value, String> {
        let manager = self.manager()?;
        let owner = self.owner_identity(&manager, resource_owner)?;
        let target = self.resolve_owned_target(&manager, resource_owner, target)?;
        manager
            .follow_up(&owner, FollowUpRequest { target, message })
            .await
    }

    pub(crate) async fn interrupt(
        &self,
        resource_owner: &str,
        target: &str,
    ) -> Result<Value, String> {
        let manager = self.manager()?;
        let owner = self.owner_identity(&manager, resource_owner)?;
        let target = self.resolve_owned_target(&manager, resource_owner, target)?;
        manager.interrupt(&owner, &target).await
    }

    pub(crate) fn list(&self, resource_owner: &str) -> Result<Value, String> {
        Self::validate_resource_owner(resource_owner)?;
        let manager = self.manager()?;
        self.owner_identity(&manager, resource_owner)?;
        let owned = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owners
            .get(resource_owner)
            .map(|owner| owner.owned_agents.clone())
            .unwrap_or_default();
        let state = manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owned_paths = owned
            .iter()
            .filter_map(|id| {
                state
                    .records
                    .get(id)
                    .map(|record| record.identity.path.clone())
            })
            .collect::<Vec<_>>();
        let agents = state
            .records
            .values()
            .filter(|record| {
                owned.contains(&record.identity.id)
                    || owned_paths
                        .iter()
                        .any(|root| is_descendant_path(&record.identity.path, root))
            })
            .map(|record| {
                let mut value = agent_record_value(record);
                value["session"] = delegated_session_reference(&record.session_path)
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                value["provenance"] = json!({
                    "kind": "extension_agent_session",
                    "principal": self.principal.as_ref(),
                    "resource_owner": resource_owner,
                });
                value
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "principal": self.principal.as_ref(),
            "resource_owner": resource_owner,
            "agents": agents,
            "persistence_error": state.persistence_error,
        }))
    }

    pub(crate) async fn wait(
        &self,
        resource_owner: &str,
        timeout: Duration,
        cancellation: &crate::CancellationToken,
    ) -> Result<Value, String> {
        let manager = self.manager()?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = manager.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let snapshot = self.list(resource_owner)?;
            let any_running = snapshot["agents"].as_array().is_some_and(|agents| {
                agents.iter().any(|agent| {
                    matches!(
                        agent["status"]["state"].as_str(),
                        Some("pending" | "running")
                    )
                })
            });
            if !any_running {
                return Ok(json!({"timed_out": false, "snapshot": snapshot}));
            }
            tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err("extension delegation wait cancelled".into())
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Ok(json!({"timed_out": true, "snapshot": self.list(resource_owner)?}))
                }
                _ = &mut changed => {}
            }
        }
    }
}
