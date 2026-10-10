//! ExecutableExtensions status, summaries, presentation views and subagent controls.

use super::*;

pub(super) fn private_python_runtime_config(
    setup: Option<octet_agent::extension_process::PythonRuntimeSetup>,
) -> anyhow::Result<octet_agent::extension_process::PythonRuntimeConfig> {
    private_python_runtime_config_for_home(dirs::home_dir(), setup)
}

fn private_python_runtime_config_for_home(
    home: Option<PathBuf>,
    setup: Option<octet_agent::extension_process::PythonRuntimeSetup>,
) -> anyhow::Result<octet_agent::extension_process::PythonRuntimeConfig> {
    let home = home.ok_or_else(|| anyhow::anyhow!("the authoritative user home is unavailable; private Python setup cannot use a workspace or '.' fallback"))?;
    anyhow::ensure!(
        home.is_absolute(),
        "the authoritative user home must be absolute"
    );
    Ok(octet_agent::extension_process::PythonRuntimeConfig {
        root: home.join(".octet/runtimes/python"),
        setup,
    })
}

impl ExecutableExtensions {
    /// Explicit selected-source setup, usable even when Python-free startup
    /// failed. Factory validation reads bounded extension source only; the
    /// returned owned future holds no App/Agent or extension-process borrow.
    pub(crate) fn python_runtime_setup(
        &self,
        extension: &str,
        config: &Config,
        setup: octet_agent::extension_process::PythonRuntimeSetup,
    ) -> anyhow::Result<Pin<Box<dyn Future<Output = anyhow::Result<PathBuf>> + Send>>> {
        use std::io::Read as _;
        anyhow::ensure!(
            self.workspace == config.workspace,
            "runtime setup belongs to another workspace; reload /extensions before retrying"
        );
        anyhow::ensure!(
            config.start_extension_processes && config.sandbox.process_execution_allowed(),
            "extension runtime setup is disabled by native-host/process execution policy"
        );
        anyhow::ensure!(!config.offline, "private Python setup requires a connection; restart without --offline to approve its verified download");
        let manager = self.runtime_manager.as_ref().ok_or_else(|| {
            anyhow::anyhow!("the selected extension has no native runtime catalogue")
        })?;
        let selected = manager.catalog();
        let expected = selected
            .get(extension)
            .ok_or_else(|| anyhow::anyhow!("the selected extension was not discovered"))?;
        anyhow::ensure!(
            expected.source_verified,
            "enable and trust the selected extension source before runtime setup"
        );
        let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
        let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
        let mut diagnostics = Vec::new();
        let (policy, _) = extension_policy(config, &mut diagnostics);
        let fresh = snapshot.resources().iter().filter(|resource| resource.name == extension).filter_map(|resource| load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics))
            .find(|descriptor| descriptor.manifest.name == extension)
            .ok_or_else(|| anyhow::anyhow!("the selected extension source is no longer discoverable; reload /extensions and retry"))?;
        anyhow::ensure!(fresh.activation.start_decision(fresh.source, config.workspace_trusted) == ExtensionStartDecision::Allowed,
            "the selected extension must be enabled, source-trusted and workspace-admitted before runtime setup");
        anyhow::ensure!(fresh.source == expected.descriptor.source
            && normalize_trusted_manifest_path(&fresh.manifest_path)? == normalize_trusted_manifest_path(&expected.descriptor.manifest_path)?,
            "the selected extension source changed; reload /extensions and review source trust before retrying");
        let current = ExtensionRuntimeCatalog::from_descriptors([fresh.clone()]);
        let current = current.get(extension).ok_or_else(|| {
            anyhow::anyhow!("the selected extension source could not be verified")
        })?;
        anyhow::ensure!(current.source_verified && current.content_digest == expected.content_digest,
            "the selected extension content changed; reload /extensions and review it before retrying");
        let directory = fresh
            .manifest_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("the selected extension manifest has no directory"))?;
        let entrypoint = directory.join(&fresh.manifest.entrypoint.command);
        let mut first_line = Vec::new();
        octet_agent::secure_fs::open_regular_file_for_read(&entrypoint)?
            .take(256)
            .read_to_end(&mut first_line)?;
        first_line.truncate(
            first_line
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(first_line.len()),
        );
        anyhow::ensure!(
            entrypoint
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
                || (first_line.starts_with(b"#!")
                    && first_line.windows(6).any(|part| part == b"python")),
            "the selected source is not a Python extension entrypoint"
        );
        let runtime = private_python_runtime_config(Some(setup))?;
        Ok(Box::pin(async move {
            octet_agent::extension_process::provision_python_runtime(&runtime)
                .await
                .map_err(Into::into)
        }))
    }

    pub fn status_summary(&self) -> String {
        let ready = self
            .summaries()
            .into_iter()
            .filter(|extension| {
                extension
                    .health
                    .as_ref()
                    .is_some_and(|health| health.state == ExtensionHealthState::Ready)
            })
            .map(|extension| extension.name.clone())
            .collect::<Vec<_>>();
        if self.summaries.is_empty() {
            "0 ready / 0 discovered".to_owned()
        } else if ready.is_empty() {
            format!("0 ready / {} discovered", self.summaries.len())
        } else {
            format!(
                "{} ready / {} discovered ({})",
                ready.len(),
                self.summaries.len(),
                ready.join(", ")
            )
        }
    }

    /// Returns discovery metadata with live protocol-health snapshots overlaid.
    pub fn summaries(&self) -> Vec<ExtensionSummary> {
        let runtime_statuses = self
            .runtime_manager
            .as_ref()
            .map(ExtensionRuntimeManager::statuses)
            .unwrap_or_default()
            .into_iter()
            .map(|status| (status.provenance.extension.clone(), status))
            .collect::<BTreeMap<_, _>>();
        let processes_by_name = self
            .processes
            .iter()
            .map(|process| (process.descriptor().manifest.name.as_str(), process))
            .collect::<BTreeMap<_, _>>();
        let mut summaries = self.summaries.clone();
        for summary in &mut summaries {
            summary.runtime = runtime_statuses.get(&summary.name).cloned();
            let Some(process) = processes_by_name.get(summary.name.as_str()).copied() else {
                continue;
            };
            let health = process.health_snapshot();
            summary.running = process.is_running();
            summary.health = Some(health);
            summary.api_version = process.api_version().to_owned();
            summary.negotiated_features = process.negotiated_features().iter().cloned().collect();
            let (telemetry_schema, compatibility) = extension_compatibility(
                &summary.name,
                summary.running,
                &summary.negotiated_features,
                summary.health.as_ref(),
            );
            summary.telemetry_schema = telemetry_schema;
            summary.compatibility = compatibility;
            summary.tools = process
                .tool_definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect();
            summary.providers = self
                .provider_runtime
                .recorded_providers_for(process.extension_instance_id())
                .into_iter()
                .map(|(entry, complete)| ExtensionProviderSummary {
                    id: entry.provider.id.clone(),
                    label: entry.provider.label.clone(),
                    authorization: entry.authorization.as_wire().to_owned(),
                    models: entry
                        .models
                        .iter()
                        .map(|model| extension_provider_model_id(&entry.provider.id, &model.id).0)
                        .collect(),
                    live: complete,
                })
                .collect();
        }
        summaries
    }

    /// Returns whether the observing first-party subagents extension has a
    /// live, negotiated child-session service.
    pub fn has_agent_session_service(&self) -> bool {
        self.processes.iter().any(|process| {
            process.descriptor().manifest.name == SUBAGENTS_EXTENSION_NAME
                && process.is_running()
                && process.supports_feature(EXTENSION_FEATURE_AGENT_SESSIONS)
                && process.supports_feature(EXTENSION_FEATURE_DELEGATION_TELEMETRY)
        })
    }

    /// Count nonterminal workers in the current owner-fenced subagents roster.
    /// This must be observed before releasing the App binding, not on its replacement.
    pub(crate) fn active_subagent_worker_count(&self) -> usize {
        self.presentation_views()
            .into_iter()
            .filter(|view| view.extension == SUBAGENTS_EXTENSION_NAME)
            .filter_map(|view| view.snapshot.collection)
            .flat_map(|collection| collection.nodes)
            .filter(|node| {
                matches!(
                    node.state,
                    octet_agent::ExtensionPresentationState::Pending
                        | octet_agent::ExtensionPresentationState::Active
                        | octet_agent::ExtensionPresentationState::Running
                        | octet_agent::ExtensionPresentationState::Degraded
                )
            })
            .count()
    }

    /// Returns the latest accepted semantic state for each running extension.
    pub fn presentation_views(&self) -> Vec<ExtensionPresentationView> {
        self.presentations
            .values()
            .filter(|view| {
                (view.resource_owner.is_none()
                    || view.resource_owner.as_deref() == self.resource_owner.as_deref())
                    && self.processes.iter().any(|process| {
                        process.descriptor().manifest.name == view.extension
                            && process.is_running()
                            && process.health_snapshot().generation == view.generation
                            && process.extension_instance_id() == view.extension_instance_id
                    })
            })
            .cloned()
            .collect()
    }

    /// Returns the exact path-free extension principal that issued a current
    /// owner-scoped delegated-session reference. Resolution remains separately
    /// parent- and principal-bound in `Agent`.
    pub fn presentation_session_reference_principal(&self, reference: &str) -> Option<String> {
        let matches = |references: &[octet_agent::ExtensionPresentationReference]| {
            references.iter().any(|candidate| {
                candidate.kind == octet_agent::ExtensionPresentationReferenceKind::Session
                    && candidate.id == reference
            })
        };
        let view = self.presentation_views().into_iter().find(|view| {
            view.snapshot
                .activities
                .iter()
                .any(|activity| matches(&activity.references))
                || view.snapshot.collection.as_ref().is_some_and(|collection| {
                    collection
                        .nodes
                        .iter()
                        .any(|node| matches(&node.references))
                        || collection
                            .detail
                            .as_ref()
                            .is_some_and(|detail| matches(&detail.references))
                })
        })?;
        self.processes
            .iter()
            .find(|process| {
                process.descriptor().manifest.name == view.extension
                    && process.extension_instance_id() == view.extension_instance_id
                    && process.health_snapshot().generation == view.generation
            })
            .map(ExtensionProcess::agent_session_principal)
    }

    /// Drains pending extension events and renders the generic headless fallback.
    pub fn presentation_text(&mut self) -> String {
        let _ = self.drain_events();
        format_presentation_views(&self.presentation_views())
    }

    pub fn inspect_text(&mut self) -> String {
        self.drain_events();
        let mut lines = Vec::new();
        if self.summaries.is_empty() {
            lines.push("No executable extensions discovered.".to_owned());
        } else {
            lines.push("Executable extensions".to_owned());
            for extension in self.summaries() {
                let state = match (extension.enabled, extension.trusted, extension.running) {
                    (_, _, true) => "running",
                    (true, false, false) => "enabled, untrusted",
                    (false, true, false) => "trusted, disabled",
                    (true, true, false) => "launch failed",
                    (false, false, false) => "disabled, untrusted",
                };
                lines.push(format!(
                    "- {} {} · API {} · {} · {:?} · {}",
                    extension.name,
                    extension.version,
                    extension.api_version,
                    state,
                    extension.source,
                    extension.manifest_path.display()
                ));
                lines.push(format!(
                    "  manifest sha256: {} · bundle sha256: {}",
                    extension.manifest_digest,
                    extension.bundle_digest.as_deref().unwrap_or("unpackaged"),
                ));
                lines.push(format!(
                    "  compatibility: {} · telemetry schema: {}",
                    extension.compatibility,
                    extension
                        .telemetry_schema
                        .as_deref()
                        .unwrap_or("not negotiated"),
                ));
                if let Some(health) = &extension.health {
                    lines.push(format!(
                        "  health: {:?} · generation {} · {} pending{}",
                        health.state,
                        health.generation,
                        health.pending_requests,
                        health
                            .last_error
                            .as_ref()
                            .map(|error| format!(" · last error: {error}"))
                            .unwrap_or_default()
                    ));
                }
                if !extension.negotiated_features.is_empty() {
                    lines.push(format!(
                        "  features: {}",
                        extension.negotiated_features.join(", ")
                    ));
                }
                if !extension.tools.is_empty() {
                    lines.push(format!("  tools: {}", extension.tools.join(", ")));
                }
                if !extension.providers.is_empty() {
                    lines.push(format!(
                        "  providers: {}",
                        extension
                            .providers
                            .iter()
                            .map(|provider| {
                                format!(
                                    "{} [{}] {} · models: {}",
                                    provider.id,
                                    provider.authorization,
                                    if provider.live { "live" } else { "pending" },
                                    provider.models.join(", ")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
                if !extension.commands.is_empty() {
                    lines.push(format!("  commands: /{}", extension.commands.join(", /")));
                }
                if !extension.hooks.is_empty() {
                    lines.push(format!("  hooks: {:?}", extension.hooks));
                }
                if !extension.ui.is_empty() {
                    lines.push(format!("  ui: {:?}", extension.ui));
                }
            }
        }
        let presentation = self.presentation_text();
        if !presentation.is_empty() {
            lines.push(String::new());
            lines.push("Extension activity".to_owned());
            lines.push(presentation);
        }
        if !self.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Diagnostics".to_owned());
            if self.diagnostics.dropped > 0 {
                lines.push(format!(
                    "- warning: {} older extension diagnostic(s) were dropped to enforce the {} byte / {} entry history limit",
                    self.diagnostics.dropped, MAX_DIAGNOSTIC_BYTES, MAX_DIAGNOSTIC_ENTRIES
                ));
            }
            lines.extend(
                self.diagnostics
                    .iter()
                    .map(|diagnostic| format!("- {diagnostic}")),
            );
        }
        lines.join("\n")
    }

    /// Own the first-party stop request independently of the active Agent borrow.
    /// No confirmation can be silently approved and no extension context may be
    /// injected. The extension and host remain responsible for owner validation
    /// and for reporting terminal settlement after the stop acknowledgement.
    pub(crate) fn subagent_stop_control(
        &self,
        target: String,
        expected_owner: &str,
    ) -> anyhow::Result<Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>> {
        anyhow::ensure!(
            !expected_owner.is_empty() && self.resource_owner.as_deref() == Some(expected_owner),
            "subagent stop requires the active session owner"
        );
        anyhow::ensure!(
            self.command_owner("subagents").as_deref() == Some(SUBAGENTS_EXTENSION_NAME),
            "first-party subagent command is unavailable"
        );
        let process = self
            .processes
            .iter()
            .find(|process| {
                process.descriptor().manifest.name == SUBAGENTS_EXTENSION_NAME
                    && process.is_running()
                    && process
                        .contributions()
                        .commands
                        .iter()
                        .any(|command| command.name == "subagents")
            })
            .ok_or_else(|| anyhow::anyhow!("first-party subagent command is not running"))?
            .clone();
        let context = extension_execution_context(&process, self.resource_owner.as_deref());
        Ok(Box::pin(async move {
            let mut diagnostics = BoundedDiagnostics::default();
            // Use the extension runtime's normal command deadline, just like
            // idle dispatch. A shorter UI timeout can cancel stop-all halfway
            // through its owner-checked sequence of interrupts.
            let output = execute_headless_command(
                &process,
                "subagents",
                vec!["stop".into(), target],
                context,
                0,
                &mut diagnostics,
            )
            .await?;
            anyhow::ensure!(
                output.context.is_empty(),
                "subagent stop attempted context injection"
            );
            anyhow::ensure!(!output.text.contains("failed closed"), "{}", output.text);
            Ok(output.text)
        }))
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn test_subagent_stop_fixture(
        workspace: &Path,
        owner: Option<&str>,
        name: &str,
        delay_seconds: u64,
    ) -> (Self, ExtensionProcess, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let script = workspace.join("subagent-stop-fixture.sh");
        let log = workspace.join("subagent-stop-fixture.jsonl");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
request_id() {{ sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }}
IFS= read -r initialize
id=$(printf '%s' "$initialize" | request_id)
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"api_version":"0.4","tools":[],"commands":[{{"name":"subagents","description":"Test owner-bound stop"}}],"protocol":{{"version":"0.4","features":["request_cancellation","content_parts","terminal_handoff"],"limits":{{"max_concurrent_requests":1}}}}}}}}\n' "$id"
while IFS= read -r request; do
  case "$request" in
    *'"method":"command/execute"'*)
      printf '%s\n' "$request" >> "$OCTET_WORKSPACE/subagent-stop-fixture.jsonl"
      id=$(printf '%s' "$request" | request_id)
      case "$request" in *'"stop"'*) sleep {} ;; esac
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"text":"interrupt requested; state stopping (not settled)","notifications":[],"context":[]}}}}\n' "$id"
      ;;
    *'"method":"shutdown"'*)
      id=$(printf '%s' "$request" | request_id)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id"
      exit 0
      ;;
  esac
done
"#,
                delay_seconds
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = ExtensionManifest::parse(&format!(
            r#"name = {name:?}
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "subagent-stop-fixture.sh"
[contributes]
commands = ["subagents"]
"#
        ))
        .unwrap();
        let process = ExtensionProcess::start(
            DiscoveredExtension {
                manifest,
                manifest_path: workspace.join("extension.toml"),
                source: ExtensionSource::Explicit,
                activation: octet_agent::extension_process::ExtensionActivation {
                    enabled: true,
                    trust: ExtensionTrust::Trusted,
                },
            },
            ExtensionRuntimeConfig::new(workspace),
        )
        .await
        .unwrap();
        let mut extensions = Self::default();
        extensions.resource_owner = owner.map(str::to_owned);
        extensions.receivers.push(process.subscribe());
        extensions.processes.push(process.clone());
        (extensions, process, log)
    }

    /// Publishes a worker roster for `process` as its current semantic state.
    /// Each worker is `(node id, label, stop target)`; one without a target
    /// carries no stop action, like a settled worker.
    #[cfg(all(test, unix))]
    pub(crate) fn test_publish_worker_roster(
        &mut self,
        process: &ExtensionProcess,
        workers: &[(&str, &str, Option<&str>)],
    ) {
        let mut nodes = Vec::new();
        let mut actions = Vec::new();
        for (node_id, label, stop) in workers {
            let mut action_ids = Vec::new();
            if let Some(target) = stop {
                let id = format!("stop:{node_id}");
                actions.push(octet_agent::ExtensionPresentationAction {
                    id: id.clone(),
                    label: format!("Stop {label}"),
                    command: "subagents".into(),
                    arguments: vec!["stop".into(), (*target).to_owned()],
                    destructive: true,
                });
                action_ids.push(id);
            }
            nodes.push(octet_agent::ExtensionPresentationNode {
                id: (*node_id).to_owned(),
                parent_id: None,
                state: if stop.is_some() {
                    octet_agent::ExtensionPresentationState::Running
                } else {
                    octet_agent::ExtensionPresentationState::Succeeded
                },
                label: (*label).to_owned(),
                secondary: None,
                action_ids,
                references: Vec::new(),
            });
        }
        let name = process.descriptor().manifest.name.clone();
        self.presentations.insert(
            name.clone(),
            ExtensionPresentationView {
                extension: name,
                generation: process.health_snapshot().generation,
                extension_instance_id: process.extension_instance_id().to_owned(),
                resource_owner: None,
                snapshot: ExtensionPresentationSnapshot {
                    revision: 1,
                    status: None,
                    activities: Vec::new(),
                    collection: Some(octet_agent::ExtensionPresentationCollection {
                        kind: octet_agent::ExtensionPresentationCollectionKind::List,
                        title: "Subagents".into(),
                        nodes,
                        selected_node_id: None,
                        detail: None,
                    }),
                    actions,
                },
            },
        );
    }

    /// An owned, single-flight observation request for the active modal loop.
    /// Only the first-party status command is admitted, without consent or
    /// context-injection authority. Persistent receivers still own snapshots.
    pub(crate) fn subagent_status_check(
        &self,
    ) -> Option<Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>> {
        let process = self
            .processes
            .iter()
            .find(|process| {
                process.descriptor().manifest.name == SUBAGENTS_EXTENSION_NAME
                    && process.is_running()
                    && process
                        .contributions()
                        .commands
                        .iter()
                        .any(|command| command.name == "subagents")
            })?
            .clone();
        let context = extension_execution_context(&process, self.resource_owner.as_deref());
        Some(Box::pin(async move {
            let mut diagnostics = BoundedDiagnostics::default();
            let result = tokio::time::timeout(
                Duration::from_millis(750),
                execute_headless_command(
                    &process,
                    "subagents",
                    vec!["status".into()],
                    context,
                    0,
                    &mut diagnostics,
                ),
            )
            .await;
            for message in diagnostics.entries {
                crate::output::stderr_line(message);
            }
            if diagnostics.dropped > 0 {
                crate::output::stderr!(
                    "warning: {} extension diagnostics omitted",
                    diagnostics.dropped
                );
            }
            let output =
                result.map_err(|_| anyhow::anyhow!("subagent status refresh timed out"))??;
            anyhow::ensure!(
                output.context.is_empty(),
                "subagent status refresh attempted context injection"
            );
            if output.text.contains("failed closed") {
                anyhow::bail!("subagent status refresh failed closed");
            }
            Ok(output.text)
        }))
    }
}

#[cfg(all(test, unix))]
mod python_setup_tests {
    use super::*;

    fn consent() -> octet_agent::extension_process::PythonRuntimeSetup {
        octet_agent::extension_process::PythonRuntimeSetup {
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress: Arc::new(|_| {}),
        }
    }

    async fn fixture() -> (tempfile::TempDir, ExecutableExtensions, Config, PathBuf) {
        fixture_with_project_source(false).await
    }

    async fn fixture_with_project_source(
        project: bool,
    ) -> (tempfile::TempDir, ExecutableExtensions, Config, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let extensions_root = root.join(if project {
            ".octet/extensions"
        } else {
            "extensions"
        });
        let directory = extensions_root.join("private-python-fixture");
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("extension.py");
        std::fs::write(
            &source,
            "#!/usr/bin/env python3\n# offline fixture, never executed\n",
        )
        .unwrap();
        let manifest = "name = 'private-python-fixture'\nversion = '0.1.0'\napi_version = '0.4'\n[entrypoint]\ncommand = 'extension.py'\n";
        std::fs::write(directory.join(EXTENSION_MANIFEST_FILENAME), manifest).unwrap();
        let mut config = crate::extensions::tests::executable_extension_config(
            &root,
            &extensions_root,
            "private-python-fixture",
        );
        config.offline = false;
        if project {
            config.extension_paths.clear();
            config.workspace_trusted = true;
        }
        let resolver = ResourceResolver::new(root.clone(), config.workspace_trusted);
        let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
        let mut diagnostics = Vec::new();
        let (policy, _) = extension_policy(&config, &mut diagnostics);
        let descriptor = snapshot
            .resources()
            .iter()
            .filter(|resource| resource.name == "private-python-fixture")
            .find_map(|resource| {
                load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
            })
            .unwrap();
        let manager =
            ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(&root).unwrap());
        manager
            .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([descriptor]))
            .await;
        let mut extensions = ExecutableExtensions::default();
        extensions.workspace = root;
        extensions.runtime_manager = Some(manager);
        (home, extensions, config, source)
    }

    #[test]
    fn private_home_location_never_falls_back_to_workspace_or_current_directory() {
        assert!(private_python_runtime_config_for_home(None, None).is_err());
        assert!(
            private_python_runtime_config_for_home(Some(PathBuf::from("relative")), None).is_err()
        );
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let config = private_python_runtime_config_for_home(Some(root.clone()), None).unwrap();
        assert_eq!(config.root, root.join(".octet/runtimes/python"));
        assert!(config.setup.is_none());
        assert!(!config.root.exists());
    }

    #[tokio::test]
    async fn admitted_python_free_factory_requires_no_running_process_or_agent_borrow() {
        let (_home, extensions, config, source) = fixture().await;
        assert!(extensions.processes.is_empty());
        let before = std::fs::read(&source).unwrap();
        let action = extensions
            .python_runtime_setup("private-python-fixture", &config, consent())
            .unwrap();
        drop(action); // Never poll: no download, Python execution or install.
        assert_eq!(std::fs::read(source).unwrap(), before);
        assert!(extensions.processes.is_empty());
    }

    #[tokio::test]
    async fn setup_factory_refuses_current_policy_disabled_untrusted_and_changed_sources() {
        let (_home, extensions, config, source) = fixture().await;
        let mut denied = config.clone();
        denied.start_extension_processes = false;
        assert!(extensions
            .python_runtime_setup("private-python-fixture", &denied, consent())
            .is_err());
        denied = config.clone();
        denied.sandbox.allow_process = false;
        assert!(extensions
            .python_runtime_setup("private-python-fixture", &denied, consent())
            .is_err());
        denied = config.clone();
        denied.enabled_extensions.clear();
        assert!(extensions
            .python_runtime_setup("private-python-fixture", &denied, consent())
            .is_err());
        denied = config.clone();
        denied.invocation_trusted_extensions.clear();
        // An explicit CLI source is already a trusted invocation choice.
        assert!(extensions
            .python_runtime_setup("private-python-fixture", &denied, consent())
            .is_ok());
        let (_project_home, project_extensions, mut project_config, _) =
            fixture_with_project_source(true).await;
        project_config.invocation_trusted_extensions.clear();
        assert!(project_extensions
            .python_runtime_setup("private-python-fixture", &project_config, consent())
            .is_err());
        denied = config.clone();
        denied.offline = true;
        assert!(extensions
            .python_runtime_setup("private-python-fixture", &denied, consent())
            .is_err());
        std::fs::write(
            source,
            "#!/usr/bin/env python3\n# changed since admission\n",
        )
        .unwrap();
        assert!(extensions
            .python_runtime_setup("private-python-fixture", &config, consent())
            .is_err());
    }
}
