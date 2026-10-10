//! Pi registration routing to the already admitted resident MCP process.
//! No MCP transport or launch engine belongs in this product binding.
use super::*;
use octet_agent::extension_process::ExtensionMcpRequest;

pub(super) struct PiMcpBinding {
    process: ExtensionProcess,
    bridge: ExtensionProcess,
    owner: ExtensionResourceOwner,
}

async fn release(binding: &PiMcpBinding) -> anyhow::Result<()> {
    let mut context = binding.bridge.current_context();
    // Cleanup is host-owned, so it does not fabricate a still-live JS parent or
    // revive a retired bridge owner. The exact original marker narrows removal.
    context.mcp_registration_owner = Some(binding.owner.clone());
    binding
        .bridge
        .execute_command("mcp", vec!["__pi_release".into()], context)
        .await
        .map_err(|_| anyhow::anyhow!("resident MCP registration cleanup failed"))?;
    Ok(())
}

impl ExecutableExtensions {
    pub(super) fn start_mcp_request(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        owner: ExtensionResourceOwner,
        request: ExtensionMcpRequest,
    ) {
        let bridge = self
            .processes
            .iter()
            .find(|candidate| {
                candidate.descriptor().manifest.name == "octet-mcp"
                    && candidate.is_running()
                    && candidate.api_version() == "0.4"
            })
            .cloned();
        let registrations = Arc::clone(&self.pi_mcp_binding);
        let live = Arc::clone(&self.resource_paths_live);
        let policy = self.effect_policy;
        self.renderer_tasks.retain(|task| !task.is_finished());
        self.renderer_tasks.push(tokio::spawn(async move {
            let result: anyhow::Result<Value> = async {
                anyhow::ensure!(
                    policy == octet_agent::EffectPolicy::UnsafeHost,
                    "transient MCP process startup requires unsafe_host"
                );
                let bridge = bridge.ok_or_else(|| {
                    anyhow::anyhow!(
                        "resident octet-mcp is not enabled; registration did not start a server"
                    )
                })?;
                let mut current = registrations.lock().await;
                anyhow::ensure!(
                    live.load(Ordering::Acquire)
                        && process.resource_owner_is_live(&owner)
                        && !process.exec_request_is_cancelled(&request_id, generation),
                    "MCP registration owner or parent is no longer active"
                );
                if let Some(binding) = current.as_ref() {
                    if binding.owner != owner {
                        anyhow::ensure!(
                            !binding.process.resource_owner_is_live(&binding.owner),
                            "resident MCP registrations belong to another active owner"
                        );
                        release(binding).await?;
                        *current = None;
                    }
                }
                let watch = current.is_none();
                // Claim cleanup ownership before dispatch: a lost acknowledgement
                // is an unknown outcome, not proof that no server was launched.
                *current = Some(PiMcpBinding {
                    process: process.clone(),
                    bridge: bridge.clone(),
                    owner: owner.clone(),
                });
                if watch {
                    let registrations = Arc::clone(&registrations);
                    let watched_owner = owner.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            let mut current = registrations.lock().await;
                            let Some(binding) = current.as_ref() else {
                                break;
                            };
                            if binding.owner != watched_owner {
                                break;
                            }
                            if !live.load(Ordering::Acquire)
                                || !binding.process.resource_owner_is_live(&binding.owner)
                            {
                                if release(binding).await.is_ok() {
                                    *current = None;
                                }
                                // Keep failed cleanup for release_binding; never retarget.
                                break;
                            }
                        }
                    });
                }
                let mut context =
                    bridge.current_context_for_resource_owner(owner.session_id.clone());
                context.mcp_registration_owner = Some(owner.clone());
                let encoded = serde_json::to_string(&request.servers)?;
                // Both the target command and its privilege marker are chosen
                // by this host. User /mcp arguments cannot obtain this marker.
                let output = bridge
                    .execute_command("mcp", vec!["__pi_replace".into(), encoded], context)
                    .await
                    .map_err(|_| anyhow::anyhow!("resident MCP registration failed"))?;
                let result: Value = serde_json::from_str(&output.text)
                    .map_err(|_| anyhow::anyhow!("invalid resident MCP acknowledgement"))?;
                anyhow::ensure!(
                    result.get("changes").is_some_and(Value::is_object)
                        && result.get("errors").is_some_and(Value::is_array)
                        && result.get("shadowed").is_some_and(Value::is_array),
                    "invalid resident MCP acknowledgement"
                );
                drop(current);
                Ok(result)
            }
            .await;
            let outcome = match result {
                Ok(result) => ExtensionRequestOutcome::Ok(result),
                Err(error) => ExtensionRequestOutcome::Failed(
                    ExtensionRequestFailure::InvalidRequest,
                    error.to_string(),
                ),
            };
            let _ = process
                .respond_to_extension_request(request_id, generation, outcome)
                .await;
        }));
    }

    pub(super) async fn clear_pi_mcp_registrations(&mut self) {
        let mut current = self.pi_mcp_binding.lock().await;
        if let Some(binding) = current.as_ref() {
            if let Err(error) = release(binding).await {
                self.diagnostics.push(error.to_string());
                return;
            }
        }
        *current = None;
    }
}
