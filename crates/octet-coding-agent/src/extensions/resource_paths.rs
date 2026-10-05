//! Bounded native resource-path collection and real loader candidate construction.
//! Activation/call-site wiring is intentionally held for the coordinated freeze.
use super::*;
use octet_agent::extension_process::{
    ExtensionResourceDiscoveryReason, ExtensionResourcePaths, EXTENSION_FEATURE_RESOURCE_PATHS,
};
use octet_agent::SkillRegistry;
mod consumer;
pub(crate) mod pi_theme;
pub(crate) use consumer::{GuardedSkills, ResourceLease, ResourceProviderGuard};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Skill,
    Prompt,
    Theme,
}

const MAX_DISCOVERY_PROCESSES: usize = 16;
const MAX_PASS_PATHS: usize = 256;
const MAX_PASS_BYTES: usize = 512 * 1024;

struct Contribution {
    process: ExtensionProcess,
    generation: u64,
    paths: ExtensionResourcePaths,
}

/// Created only from real generation-pinned process replies. No wire-supplied
/// extension name, owner or generation can authenticate a contribution.
pub(crate) struct ResourceDiscoveryBatch {
    owner: String,
    contributions: Vec<Contribution>,
    diagnostics: Vec<String>,
}

impl ExecutableExtensions {
    /// Prepare borrow-free work so an interactive caller can keep servicing the
    /// shell and session-owned reverse requests while the process is awaited.
    /// Caller must settle session_start first, including deferred UI starts.
    pub(crate) fn prepare_resource_discovery(
        &self,
        reason: ExtensionResourceDiscoveryReason,
    ) -> anyhow::Result<impl Future<Output = ResourceDiscoveryBatch> + Send + 'static> {
        anyhow::ensure!(
            self.session_lifecycle_started,
            "session_start has not been dispatched"
        );
        anyhow::ensure!(
            self.pending_session_hook_starts.is_empty()
                && self
                    .session_hook_start_tasks
                    .iter()
                    .all(JoinHandle::is_finished),
            "resources_discover must wait for deferred session_start"
        );
        let owner = self
            .resource_owner
            .clone()
            .context("resource discovery has no active owner")?;
        let mut processes = self
            .processes
            .iter()
            .filter(|p| {
                p.contributions()
                    .hooks
                    .contains(&ExtensionHook::ResourcesDiscover)
                    && p.supports_feature(EXTENSION_FEATURE_RESOURCE_PATHS)
            })
            .cloned()
            .collect::<Vec<_>>();
        processes.sort_by(|a, b| {
            a.descriptor()
                .manifest
                .name
                .cmp(&b.descriptor().manifest.name)
        });
        anyhow::ensure!(
            processes.len() <= MAX_DISCOVERY_PROCESSES,
            "resource discovery exceeds the 16-process pass budget"
        );
        Ok(async move {
            let results = futures_util::future::join_all(processes.into_iter().map(|process| {
                let owner = owner.clone();
                async move {
                    let result = process.discover_resource_paths(&owner, reason).await;
                    (process, result)
                }
            }))
            .await;
            let mut batch = ResourceDiscoveryBatch {
                owner,
                contributions: Vec::new(),
                diagnostics: Vec::new(),
            };
            let mut count = 0;
            let mut bytes = 0;
            for (process, result) in results {
                match result {
                    Ok((generation, paths)) => {
                        let all = paths
                            .skill_paths
                            .iter()
                            .chain(&paths.prompt_paths)
                            .chain(&paths.theme_paths);
                        let (n, size) = all.fold((0, 0), |(n, size), p| (n + 1, size + p.len()));
                        if count + n > MAX_PASS_PATHS || bytes + size > MAX_PASS_BYTES {
                            batch.diagnostics.push(format!(
                                "warning: {}: resource discovery exceeds pass budget",
                                process.descriptor().manifest.name
                            ));
                            continue;
                        }
                        count += n;
                        bytes += size;
                        batch.contributions.push(Contribution {
                            process,
                            generation,
                            paths,
                        });
                    }
                    Err(_) => batch.diagnostics.push(format!(
                        "warning: {}: resources_discover failed; no paths admitted",
                        process.descriptor().manifest.name
                    )),
                }
            }
            batch
        })
    }

    pub(crate) fn resource_loader(&self) -> ResourceLoader {
        ResourceLoader {
            resource_owner: self.resource_owner.clone(),
            processes: self.processes.clone(),
        }
    }

    pub(crate) fn load_resource_discovery(
        &self,
        base: &Config,
        batch: ResourceDiscoveryBatch,
        background: crate::tui::theme::TerminalBackground,
    ) -> anyhow::Result<LoadedResourcePaths> {
        self.resource_loader()
            .load_resource_discovery(base, batch, background)
    }
}

pub(crate) struct ResourceLoader {
    resource_owner: Option<String>,
    processes: Vec<ExtensionProcess>,
}

impl ResourceLoader {
    pub(crate) fn retained_baseline(
        &self,
        config: Config,
        skills: Arc<dyn SkillRegistry>,
        prompts: Arc<crate::prompts::PromptRegistry>,
        selected_theme: crate::tui::theme::OctetTheme,
        diagnostic: String,
    ) -> anyhow::Result<LoadedResourcePaths> {
        Ok(LoadedResourcePaths {
            owner: self
                .resource_owner
                .clone()
                .context("resource owner retired")?,
            fences: Vec::new(),
            config,
            skills,
            prompts,
            selected_theme,
            diagnostics: vec![diagnostic],
        })
    }

    /// Build from canonical paths on the blocking loader, never augmented grants.
    pub(crate) fn load_resource_discovery(
        &self,
        base: &Config,
        batch: ResourceDiscoveryBatch,
        background: crate::tui::theme::TerminalBackground,
    ) -> anyhow::Result<LoadedResourcePaths> {
        anyhow::ensure!(
            self.resource_owner.as_deref() == Some(batch.owner.as_str()),
            "stale resource discovery owner"
        );
        let mut config = base.clone();
        let mut diagnostics = batch.diagnostics;
        // Added roots precede the caller's explicit paths: explicit user
        // overrides remain authoritative under native later-wins precedence.
        config.skill_paths.clear();
        config.prompt_paths.clear();
        config.theme_paths.clear();
        let mut fences = Vec::new();
        for contribution in batch.contributions {
            let process = contribution.process;
            let current = self
                .processes
                .iter()
                .any(|p| p.extension_instance_id() == process.extension_instance_id());
            if !current
                || !process.is_running()
                || process.health_snapshot().generation != contribution.generation
            {
                diagnostics.push(format!(
                    "warning: {}: stale resource discovery discarded",
                    process.descriptor().manifest.name
                ));
                continue;
            }
            for (paths, target, explicit, kind) in [
                (
                    &contribution.paths.skill_paths,
                    &mut config.skill_paths,
                    &base.skill_paths,
                    PathKind::Skill,
                ),
                (
                    &contribution.paths.prompt_paths,
                    &mut config.prompt_paths,
                    &base.prompt_paths,
                    PathKind::Prompt,
                ),
                (
                    &contribution.paths.theme_paths,
                    &mut config.theme_paths,
                    &base.theme_paths,
                    PathKind::Theme,
                ),
            ] {
                for path in paths {
                    match admit_path(Path::new(path), base, explicit, kind) {
                        Ok(path) if !target.contains(&path) => target.push(path),
                        Ok(_) => {}
                        Err(error) => diagnostics.push(format!(
                            "warning: {}: rejected resource path: {error}",
                            process.descriptor().manifest.name
                        )),
                    }
                }
            }
            fences.push((process, contribution.generation));
        }
        config.skill_paths.extend(base.skill_paths.iter().cloned());
        config
            .prompt_paths
            .extend(base.prompt_paths.iter().cloned());
        config.theme_paths.extend(base.theme_paths.iter().cloned());
        let skills = Arc::new(
            crate::resources::FileSystemSkillRegistry::new_with_invocation(
                config.workspace.clone(),
                config.invocation_cwd.clone(),
                config.skill_paths.clone(),
                config.workspace_trusted,
            )?,
        );
        diagnostics.extend(
            skills
                .diagnostics()
                .iter()
                .map(|d| format!("warning: {}: {}", d.path.display(), d.message)),
        );
        let prompts = Arc::new(crate::prompts::PromptRegistry::discover(
            &config.workspace,
            &config.prompt_paths,
            config.workspace_trusted,
        ));
        diagnostics.extend(
            prompts
                .diagnostics()
                .iter()
                .map(|d| format!("warning: {d:?}")),
        );
        // Run the actual bounded parsers, not available_themes (names only).
        let file_themes = crate::tui::theme::selectable_file_themes(&config, background);
        let selected_theme = crate::tui::theme::load_theme_for_background(&config, background);
        for kind in [ResourceKind::Prompt, ResourceKind::Theme] {
            let roots = if kind == ResourceKind::Prompt {
                &config.prompt_paths
            } else {
                &config.theme_paths
            };
            let resolver =
                ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
            let snapshot = resolver.discover(kind, roots);
            diagnostics.extend(
                snapshot
                    .diagnostics()
                    .iter()
                    .map(|d| format!("warning: {}: {}", d.path.display(), d.message)),
            );
            if kind == ResourceKind::Theme {
                for resource in snapshot.resources() {
                    if !crate::tui::theme::is_reserved_theme_name(&resource.name)
                        && !file_themes.iter().any(|(name, _)| name == &resource.name)
                    {
                        diagnostics.push(format!(
                            "warning: {}: theme failed bounded native parsing",
                            resource.path.display()
                        ));
                    }
                }
            }
        }
        // Source retirement during filesystem work cannot publish stale roots.
        anyhow::ensure!(
            fences
                .iter()
                .all(|(p, generation)| p.is_running()
                    && p.health_snapshot().generation == *generation),
            "resource source retired while loading"
        );
        Ok(LoadedResourcePaths {
            owner: batch.owner,
            fences,
            config,
            skills,
            prompts,
            selected_theme,
            diagnostics,
        })
    }
}

/// An unpublished complete candidate. Applying this uses the real skill registry,
/// prompt parser and native theme loader; no registration-only success is possible.
pub(crate) struct LoadedResourcePaths {
    pub(super) owner: String,
    pub(super) fences: Vec<(ExtensionProcess, u64)>,
    pub(crate) config: Config,
    pub(crate) skills: Arc<dyn SkillRegistry>,
    pub(crate) prompts: Arc<crate::prompts::PromptRegistry>,
    pub(crate) selected_theme: crate::tui::theme::OctetTheme,
    pub(crate) diagnostics: Vec<String>,
}

impl LoadedResourcePaths {
    pub(crate) fn is_current(&self, extensions: &ExecutableExtensions) -> bool {
        extensions.resource_owner.as_deref() == Some(self.owner.as_str())
            && self.fences.iter().all(|(p, generation)| {
                p.is_running()
                    && p.health_snapshot().generation == *generation
                    && extensions
                        .processes
                        .iter()
                        .any(|current| current.extension_instance_id() == p.extension_instance_id())
            })
    }
}

fn admit_path(
    path: &Path,
    config: &Config,
    explicit: &[PathBuf],
    kind: PathKind,
) -> anyhow::Result<PathBuf> {
    // Do not canonicalize a selected symlink into an apparently safe input.
    // The final loader still performs its descriptor-bound, no-follow read.
    for ancestor in path.ancestors() {
        anyhow::ensure!(
            !std::fs::symlink_metadata(ancestor)?
                .file_type()
                .is_symlink(),
            "symlinked roots and ancestors are not supported"
        );
    }
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir() || metadata.is_file(),
        "resource is not a regular file or directory"
    );
    let canonical = path.canonicalize()?;
    let workspace = config.workspace.canonicalize()?;
    let explicit_grant = explicit.iter().any(|p| {
        let p = if p.is_absolute() {
            p.clone()
        } else {
            config.workspace.join(p)
        };
        p.canonicalize()
            .is_ok_and(|root| canonical.starts_with(root))
    });
    anyhow::ensure!(
        config.workspace_trusted || !canonical.starts_with(workspace) || explicit_grant,
        "untrusted workspace resource has no explicit path grant"
    );
    if metadata.is_file() {
        let extension = path.extension().and_then(|e| e.to_str());
        anyhow::ensure!(
            match kind {
                PathKind::Skill => extension == Some("md"),
                PathKind::Prompt => matches!(extension, Some("md" | "toml")),
                PathKind::Theme => matches!(extension, Some("toml" | "json")),
            },
            "unsupported resource format"
        );
    }
    // The shared resolver bounds directory scans and the native theme loader
    // validates both TOML and Pi JSON. Admission is not a parse-success claim.
    Ok(path.to_owned())
}

#[cfg(all(test, unix))]
pub(crate) mod consumer_tests;
#[cfg(all(test, unix))]
#[path = "resource_paths/pi_app_tests.rs"]
mod pi_app_tests;
#[cfg(all(test, unix))]
mod tests;
