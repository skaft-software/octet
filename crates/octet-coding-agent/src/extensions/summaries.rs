//! Lifecycle snapshots, reload and rescan reports, extension summaries and options.

use super::*;

/// One frontend-owned snapshot of the live extension processes, taken before a
/// host dialog borrows [`ExecutableExtensions`] mutably.
///
/// The confirmation and input handlers run under the same mutable borrow that
/// drives the extension runtime, so they cannot call the `*_all` fan-out
/// helpers. The snapshot holds the same process handles, and each host emitter
/// is a no-op unless `lifecycle_events_v2` was negotiated and the process is
/// live, so presenting a dialog never needs its own feature check.
#[derive(Default)]
pub struct ExtensionLifecycleSnapshot {
    pub(super) processes: Vec<ExtensionProcess>,
}

impl ExecutableExtensions {
    /// Resolve the live issuing process for a requested compaction callback.
    pub fn compaction_callback_process(
        &self,
        owner: &octet_agent::extension_process::ExtensionResourceOwner,
    ) -> Option<ExtensionProcess> {
        self.processes
            .iter()
            .find(|process| {
                process.extension_instance_id() == owner.extension_instance_id
                    && process.health_snapshot().generation == owner.process_generation
            })
            .cloned()
    }

    /// Real foreground mirror of the admitted setup process.
    pub fn session_setup_context(
        &self,
        owner: &octet_agent::extension_process::ExtensionResourceOwner,
    ) -> anyhow::Result<serde_json::Value> {
        let process = self
            .processes
            .iter()
            .find(|process| {
                process.extension_instance_id() == owner.extension_instance_id
                    && process.health_snapshot().generation == owner.process_generation
            })
            .ok_or_else(|| anyhow::anyhow!("session setup process retired"))?;
        Ok(process.replacement_context(owner)?)
    }

    /// Capture the current process handles for one frontend-owned broadcast.
    pub fn lifecycle_snapshot(&self) -> ExtensionLifecycleSnapshot {
        ExtensionLifecycleSnapshot {
            processes: self.processes.clone(),
        }
    }
}

impl ExtensionLifecycleSnapshot {
    /// Await Pi's cancellable replacement boundary on captured process handles.
    pub async fn before_session_change(
        &self,
        owner: &str,
        hook: ExtensionHook,
        payload: serde_json::Value,
    ) -> anyhow::Result<bool> {
        for process in &self.processes {
            if !process.contributions().hooks.contains(&hook) {
                continue;
            }
            let output = process
                .run_hook(
                    hook,
                    payload.clone(),
                    process.current_context_for_resource_owner(owner),
                )
                .await?;
            if matches!(output.disposition, ExtensionHookDisposition::Deny { .. }) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Open one host-owned dialog boundary on every captured process.
    pub fn dialog_started(&self, dialog: &str) {
        for process in &self.processes {
            let _ = process.notify_dialog_started(dialog);
        }
    }

    /// Close one host-owned dialog boundary on every captured process.
    pub fn dialog_settled(&self, dialog: &str) {
        for process in &self.processes {
            let _ = process.notify_dialog_settled(dialog);
        }
    }
}

/// One live secret-free API 0.3 provider declaration owned by an extension.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ExtensionProviderSummary {
    /// Extension-declared provider identifier.
    pub id: String,
    /// Extension-declared display label.
    pub label: String,
    /// Host-owned availability wire status (`ready`, `pending`, `denied`,
    /// `unavailable`, or `revoked`).
    pub authorization: String,
    /// Routable catalog model ids (`provider/model`).
    pub models: Vec<String>,
    /// True only after the owning generation completed its initial catalog; a
    /// declaration may be recorded while that batch is still incomplete, and
    /// then it is never callable.
    pub live: bool,
}

/// Results stay typed until the caller chooses automatic or explicit feedback.
#[derive(Debug, Default)]
pub(crate) struct ExtensionReloadReport {
    pub processes: Vec<(String, Result<String, String>)>,
    pub shortcuts: Vec<String>,
    pub rescans: ExtensionRescanReport,
    pub details: Vec<String>,
    /// Occurrences (including discarded requests), never persistent problems.
    pub events: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct ExtensionRescanReport {
    pub checked: Vec<(String, Vec<String>)>,
    pub details: Vec<String>,
    pub events: Vec<String>,
}

impl ExtensionRescanReport {
    pub(super) fn into_notices(self) -> Vec<String> {
        self.checked
            .into_iter()
            .flat_map(|(_, problems)| problems)
            .chain(self.details)
            .chain(self.events)
            .collect()
    }
}

#[cfg(all(test, unix))]
impl ExtensionReloadReport {
    pub(super) fn into_notices(self) -> Vec<String> {
        self.processes
            .into_iter()
            .map(|(_, result)| match result {
                Ok(detail) | Err(detail) => detail,
            })
            .chain(self.details)
            .chain(self.shortcuts)
            .chain(self.events)
            .chain(self.rescans.into_notices())
            .collect()
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ExtensionSummary {
    pub name: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub manifest_digest: String,
    pub bundle_digest: Option<String>,
    pub source: ExtensionSource,
    pub enabled: bool,
    pub trusted: bool,
    pub running: bool,
    pub api_version: String,
    pub negotiated_features: Vec<String>,
    pub telemetry_schema: Option<String>,
    pub compatibility: String,
    pub health: Option<ExtensionHealthSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<octet_agent::extension_runtime::ExtensionRuntimeStatus>,
    pub tools: Vec<String>,
    pub commands: Vec<String>,
    pub hooks: Vec<ExtensionHook>,
    pub ui: Vec<ExtensionUiSurface>,
    /// Live secret-free API 0.3 provider declarations owned by this extension.
    pub providers: Vec<ExtensionProviderSummary>,
}

/// The options menu shown for one extension under `/extensions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionOptions {
    /// The extension's own menu, or entries generated from its commands.
    pub menu: octet_agent::ExtensionMenu,
    /// Generated entries ask for command arguments before running.
    pub generated: bool,
}

pub(super) fn generated_options(process: &ExtensionProcess) -> ExtensionOptions {
    let items = process
        .contributions()
        .commands
        .iter()
        // A missing or failed menu must not move worker runtime controls back
        // into extension management. Preserve all unrelated configuration commands.
        .filter(|command| {
            process.descriptor().manifest.name != SUBAGENTS_EXTENSION_NAME
                || command.name != "subagents"
        })
        .map(|command| octet_agent::ExtensionMenuItem {
            id: format!("command:{}", command.name),
            label: command.name.clone(),
            description: Some(match &command.usage {
                Some(usage) => format!("{} · {usage}", command.description),
                None => command.description.clone(),
            }),
            command: Some(command.name.clone()),
            arguments: Vec::new(),
            destructive: false,
            recommended: false,
            items: None,
            detail: None,
        })
        .collect();
    ExtensionOptions {
        menu: octet_agent::ExtensionMenu {
            items,
            ..octet_agent::ExtensionMenu::default()
        },
        generated: true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ExtensionPresentationView {
    /// Manifest-bound extension that owns this state.
    pub extension: String,
    /// Active process generation; stale generations are discarded.
    pub generation: u64,
    /// Host-created process-instance fence. This prevents a replacement
    /// process whose generation counter restarted from accepting stale actions.
    pub extension_instance_id: String,
    /// Host-derived durable session owner for frontend isolation.
    pub resource_owner: Option<String>,
    /// Complete monotonic semantic snapshot.
    pub snapshot: ExtensionPresentationSnapshot,
}
