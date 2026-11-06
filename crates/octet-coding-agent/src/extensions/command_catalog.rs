//! Read-only projection of the native slash registry and admitted resource registries.
use super::*;
use octet_agent::extension_process::ExtensionSourceInfo;
use octet_agent::{SkillRegistry, SkillSource, SkillTrust};
use std::path::{Path, PathBuf};

pub(super) struct CommandResources {
    skills: Arc<dyn SkillRegistry>,
    prompts: Arc<crate::prompts::PromptRegistry>,
    sources: BTreeMap<PathBuf, ExtensionSourceInfo>,
    lease: Option<resource_paths::ResourceLease>,
}

fn local_source(path: &Path, scope: &str) -> ExtensionSourceInfo {
    ExtensionSourceInfo {
        path: path.to_string_lossy().into_owned(),
        source: "local".into(),
        scope: scope.into(),
        origin: "top-level".into(),
        base_dir: path.parent().map(|p| p.to_string_lossy().into_owned()),
    }
}

impl CommandResources {
    fn source(&self, path: &Path, scope: &str) -> ExtensionSourceInfo {
        let mut source = self
            .sources
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map(|(_, info)| info.clone())
            .unwrap_or_else(|| local_source(path, scope));
        source.path = path.to_string_lossy().into_owned();
        source
    }
}

impl ExecutableExtensions {
    pub(crate) fn bind_command_resources(
        &mut self,
        skills: Arc<dyn SkillRegistry>,
        prompts: Arc<crate::prompts::PromptRegistry>,
        sources: BTreeMap<PathBuf, ExtensionSourceInfo>,
        lease: Option<resource_paths::ResourceLease>,
    ) {
        self.command_resources = Some(CommandResources {
            skills,
            prompts,
            sources,
            lease,
        });
    }

    pub(super) fn command_catalog_outcome(&self) -> ExtensionRequestOutcome {
        let Some(resources) = &self.command_resources else {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::UnsupportedFeature,
                "native command resources are not bound".into(),
            );
        };
        if resources
            .lease
            .as_ref()
            .is_some_and(|lease| !lease.matches(self))
        {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::NotForegroundOwner,
                "command resources must settle at an idle boundary".into(),
            );
        }
        let mut commands = Vec::new();
        for command in self.command_registry() {
            let Some(process) = self
                .processes
                .iter()
                .find(|p| p.descriptor().manifest.name == command.owner && p.is_running())
            else {
                continue;
            };
            let descriptor = process.descriptor();
            commands.push(serde_json::json!({
                "name": command.public_name, "description": command.description,
                "source": "extension", "extension": command.owner,
                "registeredName": command.registered_name,
                "sourceInfo": local_source(&descriptor.manifest_path, "temporary"),
            }));
        }
        for prompt in resources.prompts.descriptors().iter() {
            let scope = match prompt.trust {
                crate::prompts::PromptTrust::UserInstalled => "user",
                crate::prompts::PromptTrust::Workspace => "project",
                crate::prompts::PromptTrust::ExplicitExternal => "temporary",
            };
            commands.push(
                serde_json::json!({"name": prompt.name, "description": prompt.description,
                "source": "prompt", "sourceInfo": resources.source(&prompt.path, scope)}),
            );
        }
        for skill in resources.skills.descriptors().iter() {
            let source = match &skill.source {
                SkillSource::FileSystem { entrypoint, .. } => {
                    let scope = match skill.trust {
                        SkillTrust::UserInstalled => "user",
                        SkillTrust::Workspace => "project",
                        SkillTrust::ExplicitExternal | SkillTrust::BuiltIn => "temporary",
                    };
                    resources.source(entrypoint, scope)
                }
                SkillSource::BuiltIn => ExtensionSourceInfo {
                    path: format!("builtin:{}", skill.id),
                    source: "builtin".into(),
                    scope: "temporary".into(),
                    origin: "top-level".into(),
                    base_dir: None,
                },
            };
            commands.push(serde_json::json!({"name": format!("skill:{}", skill.id),
                "description": skill.description, "source": "skill", "sourceInfo": source}));
        }
        let result = serde_json::json!({"commands": commands});
        // Refuse the complete projection, never silently truncate availability.
        if resources
            .lease
            .as_ref()
            .is_some_and(|lease| !lease.matches(self))
        {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::NotForegroundOwner,
                "command resource lease retired during projection".into(),
            );
        }
        if serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() <= 512 * 1024) {
            ExtensionRequestOutcome::Ok(result)
        } else {
            ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "command catalog exceeds 512 KiB".into(),
            )
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_command_catalog_rejects_revoked_resource_lease() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        app.executable_extensions.bind_command_resources(
            app.skills.clone(),
            app.prompts.clone(),
            Default::default(),
            None,
        );
        assert!(matches!(
            app.executable_extensions.command_catalog_outcome(),
            ExtensionRequestOutcome::Ok(_)
        ));
        let lease = app.executable_extensions.resource_lease().unwrap();
        app.executable_extensions.bind_command_resources(
            app.skills.clone(),
            app.prompts.clone(),
            Default::default(),
            Some(lease.clone()),
        );
        assert!(matches!(
            app.executable_extensions.command_catalog_outcome(),
            ExtensionRequestOutcome::Ok(_)
        ));
        lease.revoke();
        assert!(matches!(
            app.executable_extensions.command_catalog_outcome(),
            ExtensionRequestOutcome::Failed(ExtensionRequestFailure::NotForegroundOwner, _)
        ));
    }
}
