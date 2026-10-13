//! Activation-only preparation of the shipped Pi adapter. Discovery/install
//! remain inert. The generated catalog lives outside the integrity-checked bundle
//! and is retained for this fleet only; no Pi or saved Octet configuration changes.
use super::*;
use super::lifecycle::PI_BRIDGE_EXTENSION_NAME;
use octet_agent::extension_process::{sanitized_subprocess_environment, ProcessGroupGuard};
use std::process::Stdio;
use tokio::io::AsyncReadExt;

pub(super) fn prepare(
    descriptors: &mut [DiscoveredExtension],
    config: &Config,
    diagnostics: &mut Vec<String>,
) -> Option<tempfile::TempDir> {
    let descriptor = descriptors.iter_mut().find(|descriptor| {
        descriptor.manifest.name == PI_BRIDGE_EXTENSION_NAME
            && descriptor.manifest.entrypoint.env.get("OCTET_PI_AUTO_MIRROR")
                == Some(&"1".to_owned())
    })?;
    if !config.start_extension_processes
        || !config.sandbox.process_execution_allowed()
        || descriptor.activation.start_decision(descriptor.source, config.workspace_trusted)
            != ExtensionStartDecision::Allowed
    {
        return None;
    }
    let result = block_on_runtime(prepare_admitted(descriptor, config)).and_then(|result| result);
    match result {
        Ok((directory, manifest)) => {
            // Keep the selected source and trust identity. Only the session's
            // captured entrypoint/catalog changes, never the installed manifest.
            descriptor.manifest.entrypoint = manifest.entrypoint;
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

async fn prepare_admitted(
    descriptor: &DiscoveredExtension,
    config: &Config,
) -> anyhow::Result<(tempfile::TempDir, ExtensionManifest)> {
    let source = descriptor.manifest_path.parent().context("Pi bundle has no directory")?;
    let directory = tempfile::Builder::new().prefix("octet-pi-mirror-").tempdir()?;
    let output = directory.path().join(PI_BRIDGE_EXTENSION_NAME);
    let mut command = tokio::process::Command::new(&descriptor.manifest.entrypoint.command);
    command.arg(source.join("auto-configure.mjs"))
        .arg(&output).arg(config.workspace_trusted.to_string())
        .current_dir(&config.workspace)
        .env_clear().envs(sanitized_subprocess_environment())
        .envs(&descriptor.manifest.entrypoint.env)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true);
    // The Pi location is configuration, not credentials. Pass only these named
    // selectors; do not inherit API keys or the complete parent environment.
    for name in ["OCTET_PI_AGENT_DIR", "PI_CODING_AGENT_DIR"] {
        if !descriptor.manifest.entrypoint.env.contains_key(name) {
            if let Some(value) = std::env::var_os(name) { command.env(name, value); }
        }
    }
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    let launch = octet_agent::extension_process::WindowsProcessLaunch::extension(&mut command)?;
    let mut child = command.spawn().context("Pi compatibility requires Node 22.19+ on PATH")?;
    #[cfg(not(windows))]
    let _group = ProcessGroupGuard::bash(child.id());
    #[cfg(windows)]
    let _group: ProcessGroupGuard = launch.register(&child)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    async fn capture(stream: impl tokio::io::AsyncRead + Unpin) -> anyhow::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stream.take(65537).read_to_end(&mut bytes).await?;
        anyhow::ensure!(bytes.len() <= 65536, "Pi mirror preparation output exceeded 64 KiB");
        Ok(bytes)
    }
    let (status, _, errors) = tokio::time::timeout(Duration::from_secs(180), async {
        tokio::try_join!(async { Ok::<_, anyhow::Error>(child.wait().await?) }, capture(stdout), capture(stderr))
    }).await.context("Pi mirror preparation exceeded 180 seconds")??;
    anyhow::ensure!(status.success(), "{}", String::from_utf8_lossy(&errors));
    let text = octet_agent::secure_fs::read_regular_file_bounded(&output.join("extension.toml"), 65536)?;
    let manifest = ExtensionManifest::parse(std::str::from_utf8(&text)?)?;
    anyhow::ensure!(manifest.name == PI_BRIDGE_EXTENSION_NAME, "Pi mirror returned a different extension");
    // The shipped manifest declares the envelope accepted by enabling it. A
    // capture may narrow it, but cannot acquire provider secrets or new grants.
    anyhow::ensure!(
        manifest.capabilities.builtin_tool_overrides.iter().all(|name| descriptor.manifest.capabilities.builtin_tool_overrides.contains(name))
            && !manifest.capabilities.provider_credentials,
        "Pi mirror exceeded its declared capability envelope"
    );
    Ok((directory, manifest))
}
