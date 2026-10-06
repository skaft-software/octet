//! Idle-owned discovery -> blocking native loaders -> atomic App publication.
use super::*;
use crate::extensions::resource_paths::{
    GuardedSkills, LoadedResourcePaths, ResourceLease, ResourceProviderGuard,
};
use crate::tui::theme::{OctetTheme, TerminalBackground};
use anyhow::Context;
use octet_agent::extension_process::ExtensionResourceDiscoveryReason as Reason;
use octet_ai::ModelId;
use std::path::PathBuf;

/// Explicit host-side admission, never inferred from Config::mode or a manifest.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceConsumerCapability {
    Disabled,
    AppFrontend,
}

pub(crate) struct ResourcePathConsumer {
    pub(super) capability: ResourceConsumerCapability,
    skill_paths: Vec<PathBuf>,
    prompt_paths: Vec<PathBuf>,
    theme_paths: Vec<PathBuf>,
    // Saved appearance before any session-only resource preference is applied.
    theme: Option<String>,
    // Saved route before any session-only contribution is applied.
    model: Option<ModelId>,
    // Kept independently of revoked registry descriptors, which become empty.
    pub(super) catalog_suffix: String,
    baseline_skills: Arc<dyn octet_agent::SkillRegistry>,
    baseline_prompts: Arc<crate::prompts::PromptRegistry>,
    enabled: bool,
    pending: Option<Reason>,
    guard: ResourceProviderGuard,
}
impl ResourcePathConsumer {
    pub(crate) fn new(
        config: &Config,
        skills: &Arc<dyn octet_agent::SkillRegistry>,
        prompts: &Arc<crate::prompts::PromptRegistry>,
        extensions: &crate::extensions::ExecutableExtensions,
        host: &mut octet_agent::ExtensionHost,
        capability: ResourceConsumerCapability,
    ) -> Self {
        let enabled = capability == ResourceConsumerCapability::AppFrontend
            && extensions.has_resource_consumer_processes();
        let guard = ResourceProviderGuard::default();
        // This observes an already negotiated offer. It NEVER creates one, and
        // is appended after process hooks to reject retirement while they wait.
        if enabled {
            host.provider_context_hook(guard.clone());
        }
        Self {
            capability,
            skill_paths: config.skill_paths.clone(),
            prompt_paths: config.prompt_paths.clone(),
            theme_paths: config.theme_paths.clone(),
            theme: config.theme.clone(),
            model: config.model.clone(),
            catalog_suffix: dynamic_suffix(&[], skills.as_ref()),
            baseline_skills: skills.clone(),
            baseline_prompts: prompts.clone(),
            enabled,
            pending: Some(Reason::Startup),
            guard,
        }
    }
    pub(crate) fn restore_paths(&self, config: &mut Config) {
        config.skill_paths = self.skill_paths.clone();
        config.prompt_paths = self.prompt_paths.clone();
        config.theme_paths = self.theme_paths.clone();
        if !config.theme_explicit {
            config.theme = self.theme.clone();
        }
        if !config.model_explicit {
            config.model = self.model.clone();
        }
    }
}

/// The session-only system-prompt suffix: contributed context files followed by
/// the live skill catalog. Both are replaced atomically at publication.
fn dynamic_suffix(
    context: &[(PathBuf, String)],
    skills: &dyn octet_agent::SkillRegistry,
) -> String {
    let mut suffix = String::new();
    if !context.is_empty() {
        let blocks = context
            .iter()
            .map(|(path, contents)| crate::resources::format_context_file(path, contents))
            .collect::<Vec<_>>();
        suffix.push_str(&crate::resources::wrap_project_context(&blocks));
    }
    suffix.push_str(&crate::resources::format_skills_for_prompt(
        &skills.descriptors(),
    ));
    suffix
}

impl App {
    pub(crate) fn resource_paths_pending(&self) -> bool {
        self.resource_paths.enabled
            && (self.resource_paths.pending.is_some() || !self.resource_paths.guard.current())
    }

    /// Resolve one contributed Pi provider/model pair. A narrowed launch plan
    /// may not have registered that route yet, so complete the catalog once
    /// exactly as a model-selection surface does; an explicit invocation choice
    /// never reaches this path.
    fn resolve_contributed_model(&mut self, provider: &str, model: &str) -> Option<ModelId> {
        let resolve = |catalog: &octet_ai::ModelCatalog| {
            crate::extensions::model_control::resolve_model(catalog, provider, model)
                .map(|model| model.spec.id.clone())
        };
        if let Some(id) = resolve(&self.catalog) {
            return Some(id);
        }
        self.enrich_catalog().ok()?;
        resolve(&self.catalog)
    }
    pub(crate) fn mark_resource_paths_reload(&mut self) {
        self.resource_paths.pending = Some(Reason::Reload);
        self.resource_paths.guard.clear();
    }
    pub(crate) fn original_resource_config(&self) -> Config {
        let mut config = self.config.clone();
        self.resource_paths.restore_paths(&mut config);
        config
    }
    /// Borrow-free work: frontend continues using its sole input/event owner.
    /// The synchronous half revokes old snapshots before any await.
    pub(crate) fn prepare_resource_paths(
        &mut self,
        background: TerminalBackground,
        lease: ResourceLease,
    ) -> anyhow::Result<
        impl std::future::Future<Output = anyhow::Result<(LoadedResourcePaths, ResourceLease)>>
            + Send
            + 'static,
    > {
        self.resource_paths.guard.clear();
        let reason = self.resource_paths.pending.unwrap_or(Reason::Reload);
        anyhow::ensure!(
            lease.matches(&self.executable_extensions),
            "resource session binding changed before discovery"
        );
        let discover = self
            .executable_extensions
            .prepare_resource_discovery(reason)?;
        let loader = self.executable_extensions.resource_loader();
        let base = self.original_resource_config();
        Ok(async move {
            let batch = discover.await;
            let loaded = tokio::task::spawn_blocking(move || {
                loader.load_resource_discovery(&base, batch, background)
            })
            .await
            .context("resource loader worker failed")??;
            anyhow::ensure!(
                lease.is_current(),
                "resource binding changed during discovery/loading"
            );
            Ok((loaded, lease))
        })
    }
    pub(crate) fn apply_extension_resource_paths(
        &mut self,
        loaded: LoadedResourcePaths,
        lease: ResourceLease,
    ) -> anyhow::Result<(OctetTheme, Vec<String>)> {
        anyhow::ensure!(
            loaded.is_current(&self.executable_extensions)
                && lease.matches(&self.executable_extensions),
            "stale resource candidate"
        );
        let prefix = self
            .system
            .strip_suffix(&self.resource_paths.catalog_suffix)
            .context("resource publication requires the base skill-catalog prompt boundary")?;
        let suffix = dynamic_suffix(&loaded.context, loaded.skills.as_ref());
        let system = format!("{prefix}{suffix}");
        // All fallible work is above. Registry snapshots and provider admission
        // receive the same owner/generation fence in this synchronous boundary.
        self.system = system;
        self.agent.set_system_prompt(self.system.clone());
        self.system_tokens =
            crate::app::bootstrap::estimate_text_tokens(self.agent.system_prompt());
        self.skills = Arc::new(GuardedSkills {
            inner: loaded.skills,
            lease: lease.clone(),
        });
        self.prompts = Arc::new((*loaded.prompts).clone().with_resource_lease(lease.clone()));
        self.config.skill_paths = loaded.config.skill_paths;
        self.config.prompt_paths = loaded.config.prompt_paths;
        self.config.theme_paths = loaded.config.theme_paths;
        self.config.theme = loaded.config.theme;
        self.user_keybindings = loaded.keybindings;
        let mut diagnostics = loaded.diagnostics;
        // Session-only default route: honored only when the invocation did not
        // choose one explicitly, and restored to the saved route on withdrawal.
        if !self.config.model_explicit {
            let desired = match loaded.default_model.as_ref() {
                Some(selection) => {
                    self.resolve_contributed_model(&selection.provider, &selection.model)
                }
                None => self.resource_paths.model.clone(),
            };
            if let Some(id) = desired {
                if self.model.spec.id != id {
                    if let Ok(model) = self.catalog.resolve(&id) {
                        let reasoning = crate::app::default_reasoning_for_model(&model);
                        match self.agent.select_model_at_idle(
                            model.clone(),
                            reasoning.clone(),
                            crate::app::reasoning_label(&reasoning),
                        ) {
                            Ok(()) => {
                                self.model = model;
                                self.reasoning = reasoning;
                            }
                            Err(error) => diagnostics.push(format!(
                                "warning: contributed default model {} was not selected: {error}",
                                id.0
                            )),
                        }
                    }
                }
            }
        }
        self.resource_paths.catalog_suffix = suffix;
        self.resource_paths.pending = None;
        self.resource_paths.guard.publish(lease);
        Ok((loaded.selected_theme, diagnostics))
    }

    /// A failed phase withdraws its overlay once. It never reruns a timed-out
    /// handler or strands a usable App behind an unfinishable barrier.
    pub(crate) fn prepare_resource_withdrawal(
        &mut self,
        background: TerminalBackground,
        error: anyhow::Error,
    ) -> anyhow::Result<
        impl std::future::Future<Output = anyhow::Result<(LoadedResourcePaths, ResourceLease)>>
            + Send
            + 'static,
    > {
        self.resource_paths.guard.clear();
        let lease = self.executable_extensions.resource_lease()?;
        let loader = self.executable_extensions.resource_loader();
        let base = self.original_resource_config();
        let skills = self.resource_paths.baseline_skills.clone();
        let prompts = self.resource_paths.baseline_prompts.clone();
        let diagnostic =
            format!("warning: extension resource phase failed; overlay withdrawn: {error}")
                .chars()
                .take(4096)
                .collect::<String>();
        Ok(async move {
            let loaded = tokio::task::spawn_blocking(move || {
                let theme = crate::tui::theme::load_theme_for_background(&base, background);
                loader.retained_baseline(base, skills, prompts, theme, diagnostic)
            })
            .await
            .context("baseline theme worker failed")??;
            Ok((loaded, lease))
        })
    }
    pub(crate) async fn refresh_resource_paths_headless(&mut self) -> anyhow::Result<()> {
        let resources_pending = self.resource_paths_pending();
        if !resources_pending && !self.executable_extensions.has_pending_session_starts() {
            return Ok(());
        }
        let starts = match self.executable_extensions.resource_session_starts() {
            Ok(work) => headless_work(&mut self.executable_extensions, work).await?,
            Err(error) => Err(error),
        };
        // Startup must settle even without resource contributors. Keep the
        // headless reverse-request policy, but do not reload baseline resources.
        if !resources_pending {
            starts?;
            return Ok(());
        }
        let candidate = match starts {
            Ok(lease) => match self.prepare_resource_paths(TerminalBackground::Unknown, lease) {
                Ok(work) => headless_work(&mut self.executable_extensions, work).await?,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        let (_, diagnostics) = match candidate
            .and_then(|(loaded, lease)| self.apply_extension_resource_paths(loaded, lease))
        {
            Ok(published) => published,
            Err(error) => {
                // One baseline fallback for either phase failure or retirement
                // between loading and publication. Never retry the fallback.
                let work = self.prepare_resource_withdrawal(TerminalBackground::Unknown, error)?;
                let (baseline, lease) =
                    headless_work(&mut self.executable_extensions, work).await??;
                self.apply_extension_resource_paths(baseline, lease)?
            }
        };
        for diagnostic in diagnostics {
            crate::output::stderr_line(diagnostic);
        }
        Ok(())
    }
}

/// Distinguish real host cancellation from a contributor/phase failure.
async fn headless_work<T>(
    extensions: &mut crate::extensions::ExecutableExtensions,
    work: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<anyhow::Result<T>> {
    tokio::pin!(work);
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => anyhow::bail!("resource discovery cancelled by shutdown"),
            result = &mut work => return Ok(result),
            _ = tick.tick() => {
                // An interactive consumer queues these requests for its next
                // shell pump; a host without one answers with typed refusals
                // instead of stranding the caller.
                for notice in extensions.drain_events() {
                    // Notifications already have bounded, line-safe grouped
                    // layout. Retain it through the native diagnostic sink.
                    crate::output::stderr_multiline(notice);
                }
            }
        }
    }
}
