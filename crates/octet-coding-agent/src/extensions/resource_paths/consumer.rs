//! App-binding leases for loaded filesystem resources, not opaque handles.
use super::*;
use octet_agent::skills::*;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};

#[derive(Clone)]
pub(crate) struct ResourceLease {
    owner: String,
    live: Arc<AtomicBool>,
    clock: Arc<AtomicU64>,
    epoch: u64,
    revoked: Arc<AtomicBool>,
    // Include empty/failed responders. A crashed generation can publish baseline
    // roots, then a later restart invalidates that baseline and rediscovers.
    processes: Vec<(ExtensionProcess, u64, bool)>,
}
impl std::fmt::Debug for ResourceLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceLease")
            .field("current", &self.is_current())
            .finish()
    }
}
impl ResourceLease {
    pub(crate) fn is_current(&self) -> bool {
        self.live.load(Ordering::Acquire)
            && !self.revoked.load(Ordering::Acquire)
            && self.clock.load(Ordering::Acquire) == self.epoch
            && self.processes.iter().all(|(p, generation, running)| {
                p.health_snapshot().generation == *generation && p.is_running() == *running
            })
    }
    pub(crate) fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }
    pub(crate) fn matches(&self, extensions: &ExecutableExtensions) -> bool {
        self.is_current()
            && extensions.resource_owner.as_deref() == Some(self.owner.as_str())
            && Arc::ptr_eq(&self.live, &extensions.resource_paths_live)
            && self.processes.iter().all(|(p, _, _)| {
                extensions
                    .processes
                    .iter()
                    .any(|q| p.extension_instance_id() == q.extension_instance_id())
            })
    }
}

#[derive(Clone, Default)]
pub(crate) struct ResourceProviderGuard(Arc<Mutex<Option<ResourceLease>>>);
impl ResourceProviderGuard {
    pub(crate) fn current(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(ResourceLease::is_current)
    }
    pub(crate) fn clear(&self) {
        if let Some(old) = self.0.lock().unwrap().take() {
            old.revoke();
        }
    }
    pub(crate) fn publish(&self, lease: ResourceLease) {
        *self.0.lock().unwrap() = Some(lease);
    }
}
#[async_trait::async_trait]
impl octet_agent::ProviderContextHook for ResourceProviderGuard {
    async fn project_context(
        &self,
        _: &octet_ai::Request,
        context: &octet_agent::ProviderContextProjectionContext,
    ) -> Result<Option<octet_agent::ProviderContextProjection>, String> {
        let state = self.0.lock().unwrap();
        if !state
            .as_ref()
            .is_some_and(|lease| lease.owner == context.resource_owner && lease.is_current())
        {
            return Err("resource discovery must settle at an idle boundary".into());
        }
        Ok(None)
    }
}

impl ExecutableExtensions {
    pub(crate) fn has_resource_consumer_processes(&self) -> bool {
        self.processes.iter().any(|p| {
            p.supports_feature(EXTENSION_FEATURE_RESOURCE_PATHS)
                && p.contributions()
                    .hooks
                    .contains(&ExtensionHook::ResourcesDiscover)
        })
    }
    pub(crate) fn resource_lease(&self) -> anyhow::Result<ResourceLease> {
        Ok(ResourceLease {
            owner: self
                .resource_owner
                .clone()
                .context("resource binding has no owner")?,
            live: self.resource_paths_live.clone(),
            clock: self.resource_paths_epoch.clone(),
            epoch: self.resource_paths_epoch.load(Ordering::Acquire),
            revoked: Arc::new(AtomicBool::new(false)),
            processes: self
                .processes
                .iter()
                .filter(|p| {
                    p.supports_feature(EXTENSION_FEATURE_RESOURCE_PATHS)
                        && p.contributions()
                            .hooks
                            .contains(&ExtensionHook::ResourcesDiscover)
                })
                .map(|p| (p.clone(), p.health_snapshot().generation, p.is_running()))
                .collect(),
        })
    }
    pub(crate) fn resource_session_starts(
        &mut self,
    ) -> anyhow::Result<impl Future<Output = anyhow::Result<ResourceLease>> + Send + 'static> {
        let lease = self.resource_lease()?;
        self.schedule_session_hook_starts();
        // Dropping the frontend phase cancels owned waits, rather than detaching
        // session_start tasks and later treating an empty task list as success.
        struct Wait(Vec<JoinHandle<()>>);
        impl Drop for Wait {
            fn drop(&mut self) {
                for task in &self.0 {
                    task.abort();
                }
            }
        }
        let mut wait = Wait(std::mem::take(&mut self.session_hook_start_tasks));
        Ok(async move {
            tokio::time::timeout(Duration::from_secs(30), async {
                for task in &mut wait.0 {
                    task.await.context("session_start worker failed")?;
                }
                // No idempotent start call: only the core's retained terminal
                // outcome can admit a contributor. Failed starts are excluded
                // by discover_resource_paths and produce batch diagnostics.
                anyhow::ensure!(
                    lease.is_current(),
                    "resource generation changed while session_start settled"
                );
                Ok(lease)
            })
            .await
            .context("resource session_start barrier exceeded 30 seconds")?
        })
    }
}

pub(crate) struct GuardedSkills {
    pub(crate) inner: Arc<dyn SkillRegistry>,
    pub(crate) lease: ResourceLease,
}
impl SkillRegistry for GuardedSkills {
    fn descriptors(&self) -> Arc<[SkillDescriptor]> {
        if self.lease.is_current() {
            self.inner.descriptors()
        } else {
            Arc::from([])
        }
    }
    fn diagnostics(&self) -> Arc<[SkillDiagnostic]> {
        self.inner.diagnostics()
    }
    fn find(&self, query: &SkillQuery) -> Vec<SkillSearchResult> {
        if self.lease.is_current() {
            self.inner.find(query)
        } else {
            Vec::new()
        }
    }
    fn load(&self, id: &SkillId) -> Result<LoadedSkill, SkillLoadError> {
        if !self.lease.is_current() {
            return Err(SkillLoadError::SourceChanged);
        }
        let loaded = self.inner.load(id)?;
        if !self.lease.is_current() {
            return Err(SkillLoadError::SourceChanged);
        }
        Ok(loaded)
    }
    fn read_resource(&self, skill: &LoadedSkill, path: &str) -> Result<String, SkillLoadError> {
        if !self.lease.is_current() {
            return Err(SkillLoadError::SourceChanged);
        }
        let text = self.inner.read_resource(skill, path)?;
        if !self.lease.is_current() {
            return Err(SkillLoadError::SourceChanged);
        }
        Ok(text)
    }
}
