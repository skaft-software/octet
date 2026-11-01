//! Activation-only preparation of the shipped Pi adapter. Discovery/install
//! remain inert. The generated catalog lives outside the integrity-checked bundle
//! and is retained for this fleet only; no Pi or saved Octet configuration changes.
use super::lifecycle::PI_BRIDGE_EXTENSION_NAME;
use super::*;
use octet_agent::extension_process::{sanitized_subprocess_environment, ProcessGroupGuard};
use std::process::Stdio;
use tokio::io::AsyncReadExt;

pub(super) fn settings_binding(
    descriptor: &DiscoveredExtension,
    workspace: &Path,
    project_trusted: bool,
) -> Option<octet_agent::extension_process::PiSettingsConfig> {
    if descriptor.manifest.name != PI_BRIDGE_EXTENSION_NAME || !workspace.is_absolute() {
        return None;
    }
    let selected = ["OCTET_PI_AGENT_DIR", "PI_CODING_AGENT_DIR"]
        .into_iter()
        .find_map(|name| {
            descriptor
                .manifest
                .entrypoint
                .env
                .get(name)
                .map(std::ffi::OsString::from)
                .or_else(|| std::env::var_os(name))
        });
    let selected = match selected {
        Some(value) if value.is_empty() => return None,
        Some(value) => PathBuf::from(value),
        None => dirs::home_dir()?.join(".pi/agent"),
    };
    let absolute = if selected.is_absolute() {
        selected
    } else {
        workspace.join(selected)
    };
    let mut agent_dir = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                agent_dir.pop();
            }
            component => agent_dir.push(component.as_os_str()),
        }
    }
    Some(octet_agent::extension_process::PiSettingsConfig {
        workspace: workspace.to_path_buf(),
        agent_dir,
        project_trusted,
    })
}

pub(super) fn prepare(
    descriptors: &mut [DiscoveredExtension],
    config: &Config,
    diagnostics: &mut Vec<String>,
) -> Option<tempfile::TempDir> {
    let descriptor = descriptors
        .iter_mut()
        .find(|descriptor| needs_preparation(descriptor, config))?;
    let result = block_on_runtime(prepare_admitted(descriptor, config)).and_then(|result| result);
    finish_preparation(descriptor, result, diagnostics)
}

pub(super) fn needs_preparation(descriptor: &DiscoveredExtension, config: &Config) -> bool {
    descriptor.manifest.name == PI_BRIDGE_EXTENSION_NAME
        && descriptor
            .manifest
            .entrypoint
            .env
            .get("OCTET_PI_AUTO_MIRROR")
            .is_some_and(|value| value == "1")
        && config.start_extension_processes
        && config.sandbox.process_execution_allowed()
        && descriptor
            .activation
            .start_decision(descriptor.source, config.workspace_trusted)
            == ExtensionStartDecision::Allowed
}

pub(super) fn finish_preparation(
    descriptor: &mut DiscoveredExtension,
    result: anyhow::Result<(tempfile::TempDir, ExtensionManifest)>,
    diagnostics: &mut Vec<String>,
) -> Option<tempfile::TempDir> {
    match result {
        Ok((directory, manifest)) => {
            // Keep the selected source and trust identity. Only the session's
            // captured entrypoint/catalog changes, never the installed manifest.
            let mut entrypoint = manifest.entrypoint;
            for name in ["OCTET_PI_AGENT_DIR", "PI_CODING_AGENT_DIR"] {
                if let Some(value) = descriptor.manifest.entrypoint.env.get(name) {
                    entrypoint.env.insert(name.into(), value.clone());
                }
            }
            descriptor.manifest.entrypoint = entrypoint;
            descriptor.manifest.contributes = manifest.contributes;
            descriptor.manifest.capabilities = manifest.capabilities;
            Some(directory)
        }
        Err(error) => {
            diagnostics.push(format!("error: Pi setup mirror could not start: {error}"));
            // Do not start an empty bridge and falsely report a working mirror.
            descriptor.activation.enabled = false;
            None
        }
    }
}

pub(super) async fn prepare_admitted(
    descriptor: &DiscoveredExtension,
    config: &Config,
) -> anyhow::Result<(tempfile::TempDir, ExtensionManifest)> {
    let source = descriptor
        .manifest_path
        .parent()
        .context("Pi bundle has no directory")?;
    let directory = tempfile::Builder::new()
        .prefix("octet-pi-mirror-")
        .tempdir()?;
    let output = directory.path().join(PI_BRIDGE_EXTENSION_NAME);
    let mut command = tokio::process::Command::new(&descriptor.manifest.entrypoint.command);
    command
        .arg(source.join("auto-configure.mjs"))
        .arg(&output)
        .arg(config.workspace_trusted.to_string())
        .current_dir(&config.workspace)
        .env_clear()
        .envs(sanitized_subprocess_environment())
        .envs(&descriptor.manifest.entrypoint.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // The Pi location is configuration, not credentials. Pass only these named
    // selectors; do not inherit API keys or the complete parent environment.
    for name in ["OCTET_PI_AGENT_DIR", "PI_CODING_AGENT_DIR"] {
        if !descriptor.manifest.entrypoint.env.contains_key(name) {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
    }
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    let launch = octet_agent::extension_process::WindowsProcessLaunch::extension(&mut command)?;
    let mut child = command
        .spawn()
        .context("Pi compatibility requires Node 22.19+ on PATH")?;
    #[cfg(not(windows))]
    let _group = ProcessGroupGuard::bash(child.id());
    #[cfg(windows)]
    let _group: ProcessGroupGuard = launch.register(&child)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    async fn capture(stream: impl tokio::io::AsyncRead + Unpin) -> anyhow::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stream.take(65537).read_to_end(&mut bytes).await?;
        anyhow::ensure!(
            bytes.len() <= 65536,
            "Pi mirror preparation output exceeded 64 KiB"
        );
        Ok(bytes)
    }
    let (status, _, errors) = tokio::time::timeout(Duration::from_secs(180), async {
        tokio::try_join!(
            async { Ok::<_, anyhow::Error>(child.wait().await?) },
            capture(stdout),
            capture(stderr)
        )
    })
    .await
    .context("Pi mirror preparation exceeded 180 seconds")??;
    anyhow::ensure!(status.success(), "{}", String::from_utf8_lossy(&errors));
    let text =
        octet_agent::secure_fs::read_regular_file_bounded(&output.join("extension.toml"), 65536)?;
    let manifest = ExtensionManifest::parse(std::str::from_utf8(&text)?)?;
    anyhow::ensure!(
        manifest.name == PI_BRIDGE_EXTENSION_NAME,
        "Pi mirror returned a different extension"
    );
    // The shipped manifest declares the envelope accepted by enabling it. A
    // capture may narrow it, but cannot acquire provider secrets or new grants.
    anyhow::ensure!(
        manifest
            .capabilities
            .builtin_tool_overrides
            .iter()
            .all(|name| descriptor
                .manifest
                .capabilities
                .builtin_tool_overrides
                .contains(name))
            && !manifest.capabilities.provider_credentials,
        "Pi mirror exceeded its declared capability envelope"
    );
    Ok((directory, manifest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> DiscoveredExtension {
        DiscoveredExtension {
            manifest: ExtensionManifest::parse(
                "name = \"octet-pi-compat\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"node\"\n",
            ).unwrap(),
            manifest_path: PathBuf::from("/reviewed/extension.toml"),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        }
    }

    #[test]
    fn pi_settings_binding_uses_native_named_selector_and_exact_workspace_trust() {
        let mut descriptor = descriptor();
        descriptor
            .manifest
            .entrypoint
            .env
            .insert("OCTET_PI_AGENT_DIR".into(), "../selected".into());
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        descriptor.manifest.entrypoint.env.insert(
            "PI_CODING_AGENT_DIR".into(),
            root.join("secondary").to_string_lossy().into_owned(),
        );
        let workspace = root.join("workspace");
        let binding = settings_binding(&descriptor, &workspace, false).unwrap();
        assert_eq!(binding.agent_dir, root.join("selected"));
        assert_eq!(binding.workspace, workspace);
        assert!(!binding.project_trusted);
        assert!(
            settings_binding(&descriptor, &workspace, true)
                .unwrap()
                .project_trusted
        );
        descriptor.manifest.name = "foreign-extension".into();
        assert!(settings_binding(&descriptor, &workspace, true).is_none());
    }

    #[test]
    fn pi_settings_binding_survives_native_mirror_preparation_without_extra_environment() {
        let mut descriptor = descriptor();
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().canonicalize().unwrap().join("selected");
        descriptor.manifest.entrypoint.env.insert(
            "OCTET_PI_AGENT_DIR".into(),
            selected.to_string_lossy().into_owned(),
        );
        descriptor
            .manifest
            .entrypoint
            .env
            .insert("UNRELATED".into(), "not-forwarded".into());
        let mut prepared = descriptor.manifest.clone();
        prepared.entrypoint.env.clear();
        prepared
            .entrypoint
            .env
            .insert("OCTET_PI_COMPAT_CACHE".into(), "/cache".into());
        let mut diagnostics = Vec::new();
        let directory = finish_preparation(
            &mut descriptor,
            Ok((tempfile::tempdir().unwrap(), prepared)),
            &mut diagnostics,
        );
        assert!(directory.is_some());
        assert!(diagnostics.is_empty());
        assert_eq!(
            descriptor
                .manifest
                .entrypoint
                .env
                .get("OCTET_PI_AGENT_DIR")
                .unwrap(),
            &selected.to_string_lossy()
        );
        assert!(!descriptor.manifest.entrypoint.env.contains_key("UNRELATED"));
        assert_eq!(
            settings_binding(&descriptor, selected.parent().unwrap(), false)
                .unwrap()
                .agent_dir,
            selected
        );
    }
}
