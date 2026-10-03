//! ExecutableExtensions notifications after mutations and configuration changes, and resource rescans.

use super::*;

impl ExecutableExtensions {
    /// Delivers one completed, host-owned mutation to declared API `0.2`
    /// post-mutation hooks and returns only subset-validated rescan requests.
    ///
    /// Call this only after durable commit or completed rollback. Duplicate
    /// mutation identities are ignored across process reloads/restarts owned by
    /// this `ExecutableExtensions` instance. The returned requests are queued
    /// for the product resource resolver; hooks never get paths, contents, or
    /// permission to perform the mutation themselves.
    pub async fn notify_post_mutation(
        &mut self,
        mutation: PostMutationContext,
    ) -> Vec<PostMutationRescan> {
        if self
            .seen_post_mutation_ids
            .iter()
            .any(|seen| seen == mutation.mutation_id())
        {
            return Vec::new();
        }
        while self.seen_post_mutation_ids.len() >= MAX_SEEN_POST_MUTATION_IDS {
            self.seen_post_mutation_ids.pop_front();
        }
        self.seen_post_mutation_ids
            .push_back(mutation.mutation_id().to_owned());
        if mutation.kind() != PostMutationKind::Resource {
            for resource in mutation
                .affected_resources()
                .iter()
                .filter(|resource| mutation_resources::known(resource))
            {
                let current = self
                    .mutation_family_generations
                    .entry(resource.clone())
                    .or_default();
                *current = (*current).max(mutation.generation());
            }
        }

        let resource_owner = self.resource_owner.clone();
        let calls = self
            .processes
            .iter()
            .filter(|process| {
                process
                    .contributions()
                    .hooks
                    .contains(&ExtensionHook::PostMutation)
            })
            .cloned()
            .map(|process| {
                let name = process.descriptor().manifest.name.clone();
                let process_generation = process.health_snapshot().generation;
                let mutation = mutation.clone();
                let resource_owner = resource_owner.clone();
                async move {
                    let result = tokio::time::timeout(
                        POST_MUTATION_RPC_DEADLINE,
                        process.post_mutation(&mutation, resource_owner.as_deref()),
                    )
                    .await;
                    (name, process_generation, result)
                }
            });
        let results = futures_util::future::join_all(calls).await;
        let mut accepted = Vec::new();
        for (extension, process_generation, result) in results {
            let disposition = match result {
                Ok(Ok(disposition)) => disposition,
                Ok(Err(error)) => {
                    self.diagnostics.push(format!(
                        "warning: extension {extension:?} post_mutation hook failed: {error}"
                    ));
                    continue;
                }
                Err(_) => {
                    self.diagnostics.push(format!(
                        "warning: extension {extension:?} post_mutation hook exceeded {POST_MUTATION_RPC_DEADLINE:?}"
                    ));
                    continue;
                }
            };
            let Some(resource_ids) = disposition.resource_ids() else {
                continue;
            };
            if resource_ids.iter().any(|resource| {
                mutation
                    .affected_resources()
                    .binary_search(resource)
                    .is_err()
            }) {
                self.diagnostics.push(format!(
                    "warning: extension {extension:?} requested a post_mutation rescan outside the affected resource set"
                ));
                continue;
            }
            let request = PostMutationRescan {
                extension,
                mutation_id: mutation.mutation_id().to_owned(),
                kind: mutation.kind(),
                process_generation,
                generation: mutation.generation(),
                resource_ids: resource_ids.to_vec(),
            };
            while self.pending_post_mutation_rescans.len() >= MAX_PENDING_POST_MUTATION_RESCANS {
                self.pending_post_mutation_rescans.pop_front();
            }
            self.pending_post_mutation_rescans
                .push_back(request.clone());
            accepted.push(request);
        }
        accepted
    }

    /// Observe a completed user-configuration transaction. Never call for a
    /// preview, failed/partial write, or an in-memory-only setting change.
    /// The caller owns the stable ID and increasing resource generation.
    pub async fn notify_configuration_changed(
        &mut self,
        mutation_id: impl Into<String>,
        generation: u64,
        state: PostMutationState,
    ) -> Vec<PostMutationRescan> {
        let Some(mutation) = PostMutationContext::new(
            mutation_id,
            PostMutationKind::Configuration,
            ["resource:settings".to_owned()],
            generation,
            state,
        ) else {
            self.diagnostics
                .push("warning: rejected invalid configuration post_mutation notification");
            return Vec::new();
        };
        self.notify_post_mutation(mutation).await
    }

    /// Convenience bridge for a host-owned committing migration integration.
    ///
    /// Dry-run scanners must never invoke this method. A committing ingestion
    /// path must have a safely bound extension owner, pass the same stable ID
    /// on retry, and call this only after commit or a completed rollback.
    ///
    /// The post-mutation hook suite that exercises it drives real extension
    /// processes, so this exists on unix test builds only.
    #[cfg(all(test, unix))]
    pub async fn notify_migration_ingested(
        &mut self,
        mutation_id: impl Into<String>,
        affected_resources: impl IntoIterator<Item = String>,
        generation: u64,
        state: PostMutationState,
    ) -> Vec<PostMutationRescan> {
        let Some(mutation) = PostMutationContext::new(
            mutation_id,
            PostMutationKind::MigrationIngestion,
            affected_resources,
            generation,
            state,
        ) else {
            self.diagnostics
                .push("warning: rejected invalid migration post_mutation notification");
            return Vec::new();
        };
        self.notify_post_mutation(mutation).await
    }

    /// Drains host-validated rescan requests for the product resource owner.
    ///
    /// The queue contains no raw paths or contents and is bounded independently
    /// of extension event/progress channels.
    pub fn take_post_mutation_rescans(&mut self) -> Vec<PostMutationRescan> {
        self.pending_post_mutation_rescans.drain(..).collect()
    }

    /// Drains and re-resolves post-mutation rescan requests through the
    /// discovery configuration bound at construction.
    ///
    /// This is the product drain for mutations that have no reload path of their
    /// own (for example a configuration commit). It fails closed: when no
    /// discovery configuration is bound the queue is discarded with a bounded
    /// diagnostic rather than resolved against guessed roots, and a request for a
    /// stale generation or stopped process is dropped without re-entering it.
    pub fn drain_post_mutation_rescans(&mut self) -> Vec<String> {
        self.drain_post_mutation_report().into_notices()
    }

    pub(super) fn drain_post_mutation_report(&mut self) -> ExtensionRescanReport {
        let Some(config) = self.rescan_config.clone() else {
            let dropped = self
                .take_post_mutation_rescans()
                .into_iter()
                .map(|request| request.resource_ids.len())
                .sum::<usize>();
            return ExtensionRescanReport {
                events: if dropped == 0 {
                    Vec::new()
                } else {
                    vec![format!(
                    "warning: discarded {dropped} post_mutation rescan request(s); no discovery configuration is bound"
                )]
                },
                ..Default::default()
            };
        };
        self.rescan_post_mutation_report(&config)
    }

    /// Re-resolves selected extension resources through the same trust,
    /// precedence, no-follow and byte bounds as initial discovery. This is
    /// read-only: changed sources require an explicit product rebuild, never
    /// implicit activation or a recursive reload from an observational hook.
    pub(crate) fn rescan_post_mutation_resources(&mut self, config: &Config) -> Vec<String> {
        self.rescan_post_mutation_report(config).into_notices()
    }

    pub(super) fn rescan_post_mutation_report(&mut self, config: &Config) -> ExtensionRescanReport {
        let requests = self.take_post_mutation_rescans();
        let mut output = ExtensionRescanReport::default();
        let mut selected = BTreeMap::new();
        let mut families = BTreeMap::new();
        for request in requests {
            let requester_current = self.processes.iter().any(|process| {
                process.descriptor().manifest.name == request.extension
                    && process.is_running()
                    && process.health_snapshot().generation == request.process_generation
            });
            if !requester_current {
                output
                    .events
                    .push("warning: discarded stale post_mutation requesting process".into());
                continue;
            }
            for resource_id in request.resource_ids {
                if request.kind != PostMutationKind::Resource
                    && mutation_resources::known(&resource_id)
                {
                    if self.mutation_family_generations.get(&resource_id)
                        == Some(&request.generation)
                    {
                        families.insert(resource_id, request.generation);
                    } else {
                        output.events.push(
                            "warning: discarded stale post_mutation resource generation".into(),
                        );
                    }
                    continue;
                }
                let process = self.processes.iter().find(|process| {
                    opaque_extension_resource_id(&process.descriptor().manifest.name) == resource_id
                });
                let Some(process) = process.filter(|process| {
                    process.is_running()
                        && process.health_snapshot().generation == request.generation
                }) else {
                    output.events.push(
                        "warning: discarded stale or unavailable post_mutation resource rescan"
                            .into(),
                    );
                    continue;
                };
                selected.insert(
                    resource_id,
                    (process.descriptor().clone(), request.generation),
                );
            }
        }
        for (resource, generation) in families {
            output.events.push(mutation_resources::rescan(
                &resource,
                generation,
                config,
                self.rescan_global_config.as_deref(),
            ));
        }
        if selected.is_empty() {
            return output;
        }
        let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
        let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
        let mut problems = Vec::new();
        let (policy, _) = extension_policy(config, &mut problems);
        output.checked.push(("policy".into(), problems));
        for (_, (previous, generation)) in selected {
            let name = &previous.manifest.name;
            let key = format!("resource:{name}");
            let mut problems = Vec::new();
            let Some(resource) = snapshot
                .resources()
                .iter()
                .find(|resource| &resource.name == name)
            else {
                output.checked.push((
                    key,
                    vec![format!(
                        "warning: rescanned extension {name:?} is unavailable; run /reload"
                    )],
                ));
                continue;
            };
            let Some(mut current) =
                load_extension_descriptor(&resolver, resource, &policy, &mut problems)
            else {
                output.checked.push((key, problems));
                continue;
            };
            apply_experimental_streamable_http_mcp_gate(
                &mut current,
                config.experimental_streamable_http_mcp,
            );
            if current != previous {
                problems.push(format!("warning: rescanned extension {name:?} changed; run /reload before using the new resource"));
                output.checked.push((key, problems));
                continue;
            }
            output.checked.push((key, problems));
            output.details.push(format!(
                "rescanned extension {name:?} (generation {generation})"
            ));
        }
        output
    }
}
