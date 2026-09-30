//! Fixtures shared by more than one group of extension-host tests.
//!
//! Each item here builds a *running* piece of the extension host — a full
//! [`Config`], a live extension process, or a recording confirmation handler —
//! so that the tests in the sibling group modules can exercise the host against
//! something real instead of a mock. They live in one place rather than being
//! copied per group so a change to the private `Config` / host shapes is one
//! compile error in one file rather than drift across a dozen.

use super::*;

#[cfg(unix)]
pub(in crate::extensions) fn executable_extension_config(
    workspace: &Path,
    extension_root: &Path,
    name: &str,
) -> Config {
    Config {
        workspace: workspace.to_owned(),
        invocation_cwd: workspace.to_owned(),
        model: Some(octet_ai::ModelId("gpt-4o-mini".into())),
        model_explicit: false,
        reasoning: None,
        reasoning_explicit: false,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        reasoning_mode_explicit: false,
        cache_retention: octet_ai::CacheRetention::Short,
        effect_policy: octet_agent::EffectPolicy::Controlled,
        sandbox: crate::config::SandboxPolicy {
            allow_external_paths: false,
            ..crate::config::SandboxPolicy::default()
        },
        theme: None,
        system_prompt: None,
        theme_paths: vec![],
        color: crate::config::ColorMode::Auto,
        mouse: crate::config::MouseMode::Auto,
        plain: false,
        show_images: false,
        session_dir: workspace.join("sessions"),
        compaction: crate::config::CompactionPolicy::default(),
        max_cost_microdollars: None,
        cost_warning_microdollars: None,
        max_turns: Some(1),
        show_reasoning_in_print: false,
        initial_prompt: None,
        prompt_template: None,
        debug_prompt: false,
        prompt_paths: vec![],
        mode: crate::config::Mode::Interactive,
        resume: crate::config::ResumeSelector::New,
        skill_paths: vec![],
        extension_paths: vec![extension_root.to_owned()],
        enabled_extensions: vec![name.to_owned()],
        extension_activation_overridden: false,
        trusted_extensions: vec![],
        invocation_trusted_extensions: vec![name.to_owned()],
        start_extension_processes: true,
        experimental_streamable_http_mcp: false,
        extension_flag_values: BTreeMap::new(),
        tools: crate::config::ToolPolicy::default(),
        telemetry: None,
        context_files: false,
        offline: true,
        workspace_trusted: false,
    }
}

#[cfg(unix)]
pub(super) async fn lifecycle_fixture(temp: &tempfile::TempDir) -> (ExecutableExtensions, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = temp.path().join("lifecycle-fixture.sh");
    let wire_log = temp.path().join("lifecycle-wire.jsonl");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
IFS= read -r initialize
id=$(printf '%s\n' "$initialize" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.2","tools":[],"commands":[],"protocol":{"version":"0.2","features":["request_cancellation","content_parts","lifecycle_events"],"limits":{"max_concurrent_requests":1},"lifecycle_events":["turn/started","turn/settled"]}}}\n' "$id"
while IFS= read -r request; do
  printf '%s\n' "$request" >> "$OCTET_WORKSPACE/lifecycle-wire.jsonl"
  case "$request" in
    *'"method":"shutdown"'*)
      id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      exit 0
      ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"
name = "lifecycle-fixture"
version = "0.2.0"
api_version = "0.2"

[entrypoint]
command = "lifecycle-fixture.sh"
"#,
    )
    .unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.host_state.session_id = Some("lifecycle-test-session".into());
    runtime.request_timeout = Duration::from_secs(2);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join("extension.toml"),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        runtime,
    )
    .await
    .unwrap();

    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process);
    extensions.session_id = Some("lifecycle-test-session".into());
    (extensions, wire_log)
}

#[cfg(unix)]
#[derive(Default)]
pub(super) struct RecordingConfirmationHandler {
    pub(super) calls: Vec<(String, String)>,
    pub(super) input_calls: Vec<(String, String, bool)>,
    pub(super) input_value: Option<String>,
}

#[cfg(unix)]
impl ExtensionConfirmationHandler for RecordingConfirmationHandler {
    fn confirm<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        Box::pin(async move {
            self.calls
                .push((extension.to_owned(), request.prompt.clone()));
            Ok(true)
        })
    }

    fn input<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a ExtensionInputRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + 'a>> {
        Box::pin(async move {
            self.input_calls
                .push((extension.to_owned(), request.prompt.clone(), request.secret));
            Ok(self.input_value.take())
        })
    }
}

#[cfg(unix)]
pub(super) struct ImmediateCancellationHandler;

#[cfg(unix)]
impl ExtensionConfirmationHandler for ImmediateCancellationHandler {
    fn wait_for_cancel<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn confirm<'a>(
        &'a mut self,
        _extension: &'a str,
        _request: &'a ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        Box::pin(async { anyhow::bail!("unexpected confirmation") })
    }
}
