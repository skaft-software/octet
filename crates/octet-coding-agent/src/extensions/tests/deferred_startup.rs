//! Deferred extension startup: the first frame never waits, a submission waits
//! only for the hooks that own it.
//!
//! These drive real extension processes (shell fixtures that answer the
//! initialize handshake) through the same discovery and activation path the
//! interactive frontend uses with `ExtensionStartupTiming::AfterFirstFrame`.

use super::*;

/// A minimal protocol fixture: `initialize` is answered after `delay_seconds`
/// with exactly the contributions its manifest declares.
fn write_fixture(root: &Path, name: &str, hooks: &[&str], delay_seconds: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    let announced = hooks
        .iter()
        .map(|hook| format!("\"{hook}\""))
        .collect::<Vec<_>>()
        .join(",");
    let hook_line = if hooks.is_empty() {
        String::new()
    } else {
        format!("contributes = {{ hooks = [{announced}] }}\n")
    };
    std::fs::write(
        directory.join(EXTENSION_MANIFEST_FILENAME),
        format!(
            r#"name = "{name}"
version = "0.2.0"
api_version = "0.2"
{hook_line}
[entrypoint]
command = "fixture.sh"
"#
        ),
    )
    .unwrap();
    let script = directory.join("fixture.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
if [ {delay_seconds} -gt 0 ]; then sleep {delay_seconds}; fi
IFS= read -r initialize
id=$(printf '%s\n' "$initialize" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"api_version":"0.2","tools":[],"commands":[],"protocol":{{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{{"max_concurrent_requests":1}}}}}}}}\n' "$id"
while IFS= read -r request; do
  case "$request" in
    *'"method":"shutdown"'*)
      id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id"
      exit 0
      ;;
  esac
done
"#
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&script, permissions).unwrap();
    directory
}

fn deferred_fixture_config(workspace: &Path, extension_root: &Path, names: &[&str]) -> Config {
    let mut config = executable_extension_config(workspace, extension_root, names[0]);
    config.enabled_extensions = names.iter().map(|name| (*name).to_owned()).collect();
    config.invocation_trusted_extensions = names.iter().map(|name| (*name).to_owned()).collect();
    config
}

/// The deferred fleet starts every admitted extension but only records the
/// `before_prompt` owners as prompt blockers; a synchronous fleet is complete
/// before it is handed back.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn deferred_startup_blocks_prompts_only_on_their_hook_owners() {
    let temp = tempfile::tempdir().unwrap();
    let extension_root = temp.path().join(".octet/extensions");
    write_fixture(&extension_root, "p4-prompt-hook", &["before_prompt"], 0);
    write_fixture(&extension_root, "p4-unrelated", &[], 5);
    // Discovered but not enabled: it never starts, and it must not disappear
    // from the fleet's status summaries when a late attach rebuilds them.
    write_fixture(&extension_root, "p4-disabled", &[], 0);
    let config = deferred_fixture_config(
        temp.path(),
        &extension_root,
        &["p4-prompt-hook", "p4-unrelated"],
    );
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();

    let mut host = ExtensionHost::new();
    let mut extensions = ExecutableExtensions::discover_and_start_with_provider_runtime_and_reason(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
        None,
        ExtensionProviderRuntime::default(),
        crate::app::resource_paths::ResourceConsumerCapability::Disabled,
        None,
        "startup",
        ExtensionStartupTiming::AfterFirstFrame,
    );

    assert!(extensions.startup_pending());
    assert_eq!(
        extensions.pending_startup_names(),
        vec!["p4-prompt-hook".to_owned(), "p4-unrelated".to_owned()]
    );
    // The unrelated 5 s extension is starting, but nothing waits on it.
    assert_eq!(
        extensions.pending_prompt_hook_names(),
        vec!["p4-prompt-hook".to_owned()]
    );

    let started = Instant::now();
    extensions
        .await_pending_prompt_hooks(&mut host, &session)
        .await
        .expect("the hook owner answers its handshake");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a submission waited for an extension that owns no prompt hook"
    );
    assert!(extensions
        .processes
        .iter()
        .any(|process| process.descriptor().manifest.name == "p4-prompt-hook"));
    // The unrelated extension is still attaching in the background.
    assert!(extensions.startup_pending());
    assert_eq!(
        extensions.pending_startup_names(),
        vec!["p4-unrelated".to_owned()]
    );

    // Status summaries follow each attach, and a descriptor that is discovered
    // but not enabled keeps its entry across the deferred installs.
    let hook_owner = extensions
        .summaries
        .iter()
        .find(|summary| summary.name == "p4-prompt-hook")
        .expect("attached summary");
    assert!(hook_owner.running, "an attached extension is running");
    assert!(hook_owner.hooks.contains(&ExtensionHook::BeforePrompt));
    let unrelated = extensions
        .summaries
        .iter()
        .find(|summary| summary.name == "p4-unrelated")
        .expect("pending summary");
    assert!(!unrelated.running, "a pending extension is not running yet");
    let disabled = extensions
        .summaries
        .iter()
        .find(|summary| summary.name == "p4-disabled")
        .expect("a discovered but disabled extension keeps its summary");
    assert!(!disabled.enabled && !disabled.running);

    // And its attach is still observable through the ordinary pump: the pump
    // reports what it installed, and `startup_pending` stays the settled test.
    let progress = extensions.pump_deferred_startup(&mut host, &session).await;
    assert!(progress.changed || progress.attached.is_empty());
}

/// A synchronous boot is never pending: print/rpc/headless frontends keep the
/// historical contract that the fleet they receive is complete.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn synchronous_startup_is_complete_when_handed_back() {
    let temp = tempfile::tempdir().unwrap();
    let extension_root = temp.path().join(".octet/extensions");
    write_fixture(&extension_root, "p4-sync", &["before_prompt"], 0);
    let config = deferred_fixture_config(temp.path(), &extension_root, &["p4-sync"]);
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();

    let mut host = ExtensionHost::new();
    let extensions = ExecutableExtensions::discover_and_start_with_provider_runtime_and_reason(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
        None,
        ExtensionProviderRuntime::default(),
        crate::app::resource_paths::ResourceConsumerCapability::Disabled,
        None,
        "startup",
        ExtensionStartupTiming::Synchronous,
    );

    assert!(!extensions.startup_pending());
    assert!(extensions.pending_prompt_hook_names().is_empty());
    assert!(extensions.processes.iter().any(|process| {
        process.descriptor().manifest.name == "p4-sync" && process.is_running()
    }));
}

fn write_pi_preparation_fixture(root: &Path, fail: bool) -> PathBuf {
    let directory = write_fixture(root, "octet-pi-compat", &[], 0);
    std::fs::write(
        directory.join(EXTENSION_MANIFEST_FILENAME),
        r#"name = "octet-pi-compat"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "/bin/sh"
env = { OCTET_PI_AUTO_MIRROR = "1" }
"#,
    )
    .unwrap();
    std::fs::write(
        directory.join("auto-configure.mjs"),
        format!(
            r#"#!/bin/sh
source=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
printf started > "$source/capture-started"
count=0
while [ ! -f "$source/release-capture" ]; do
  count=$((count + 1))
  if [ "$count" -ge 500 ]; then exit 1; fi
  sleep 0.02
done
if [ "{fail}" = true ]; then printf 'capture fixture failed' >&2; exit 1; fi
mkdir -p "$1"
cat > "$1/extension.toml" <<EOF
name = "octet-pi-compat"
version = "0.2.0"
api_version = "0.2"
contributes = {{ hooks = ["before_prompt"] }}
[entrypoint]
command = "$source/fixture.sh"
EOF
printf complete > "$source/capture-complete"
"#,
        ),
    )
    .unwrap();
    directory
}

async fn wait_for_pi_capture(directory: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.join("capture-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Pi capture task started");
}

fn start_pi_preparation_fixture(
    config: &Config,
    host: &mut ExtensionHost,
    session: &Session,
    sessions: &SessionStore,
) -> ExecutableExtensions {
    start_pi_preparation_fixture_with_timing(
        config,
        host,
        session,
        sessions,
        ExtensionStartupTiming::AfterFirstFrame,
    )
}

fn start_pi_preparation_fixture_with_timing(
    config: &Config,
    host: &mut ExtensionHost,
    session: &Session,
    sessions: &SessionStore,
    timing: ExtensionStartupTiming,
) -> ExecutableExtensions {
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    ExecutableExtensions::discover_and_start_with_provider_runtime_and_reason(
        config,
        session,
        &model,
        &ReasoningConfig::Off,
        sessions,
        host,
        None,
        ExtensionProviderRuntime::default(),
        crate::app::resource_paths::ResourceConsumerCapability::Disabled,
        None,
        "startup",
        timing,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn synchronous_pi_startup_prepares_and_attaches_before_returning() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("extensions");
    let directory = write_pi_preparation_fixture(&root, false);
    std::fs::write(directory.join("release-capture"), "release").unwrap();
    let config = deferred_fixture_config(temp.path(), &root, &["octet-pi-compat"]);
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let mut host = ExtensionHost::new();
    let extensions = start_pi_preparation_fixture_with_timing(
        &config,
        &mut host,
        &session,
        &sessions,
        ExtensionStartupTiming::Synchronous,
    );
    assert!(!extensions.startup_pending());
    assert!(extensions.pi_mirror_directory.is_some());
    assert_eq!(extensions.processes.len(), 1);
    assert!(extensions.processes[0].is_running());
    assert!(directory.join("capture-complete").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn pi_capture_defers_past_fleet_return_and_gates_unknown_prompt_hooks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("extensions");
    let directory = write_pi_preparation_fixture(&root, false);
    let original = std::fs::read(directory.join(EXTENSION_MANIFEST_FILENAME)).unwrap();
    let config = deferred_fixture_config(temp.path(), &root, &["octet-pi-compat"]);
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let mut host = ExtensionHost::new();
    let started = Instant::now();
    let mut extensions = start_pi_preparation_fixture(&config, &mut host, &session, &sessions);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "fleet return awaited Pi capture"
    );
    assert!(extensions.startup_pending());
    assert_eq!(
        extensions.pending_prompt_hook_names(),
        vec!["octet-pi-compat"]
    );
    assert!(extensions.pi_mirror_directory.is_none());
    assert!(extensions.runtime_binding.is_none());
    assert!(extensions.processes.is_empty());
    wait_for_pi_capture(&directory).await;
    let progress = extensions.pump_deferred_startup(&mut host, &session).await;
    assert!(progress.attached.is_empty());
    assert!(extensions.startup_pending());
    assert!(tokio::time::timeout(
        Duration::from_millis(50),
        extensions.await_pending_prompt_hooks(&mut host, &session),
    )
    .await
    .is_err());
    assert!(extensions.startup_pending());
    assert!(!directory.join("capture-complete").exists());
    std::fs::write(directory.join("release-capture"), "release").unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        extensions.await_pending_prompt_hooks(&mut host, &session),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!extensions.startup_pending());
    assert!(extensions.pi_mirror_directory.is_some());
    assert_eq!(extensions.processes.len(), 1);
    let descriptor = extensions.processes[0].descriptor();
    assert_eq!(
        descriptor.manifest_path,
        directory
            .join(EXTENSION_MANIFEST_FILENAME)
            .canonicalize()
            .unwrap()
    );
    assert_eq!(descriptor.source, ExtensionSource::Explicit);
    assert_eq!(descriptor.activation.trust, ExtensionTrust::Trusted);
    assert!(descriptor
        .manifest
        .contributes
        .hooks
        .contains(&ExtensionHook::BeforePrompt));
    assert_eq!(
        std::fs::read(directory.join(EXTENSION_MANIFEST_FILENAME)).unwrap(),
        original
    );
    assert!(extensions
        .summaries
        .iter()
        .any(|summary| summary.name == "octet-pi-compat" && summary.running));
}

#[tokio::test(flavor = "multi_thread")]
async fn pi_capture_failure_is_reported_without_starting_an_empty_bridge() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("extensions");
    let directory = write_pi_preparation_fixture(&root, true);
    let config = deferred_fixture_config(temp.path(), &root, &["octet-pi-compat"]);
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let mut host = ExtensionHost::new();
    let mut extensions = start_pi_preparation_fixture(&config, &mut host, &session, &sessions);
    wait_for_pi_capture(&directory).await;
    std::fs::write(directory.join("release-capture"), "release").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while extensions.startup_pending() {
            let progress = extensions.pump_deferred_startup(&mut host, &session).await;
            if !extensions.startup_pending() {
                assert!(progress.changed);
                assert!(progress
                    .notices
                    .iter()
                    .any(|notice| notice.contains("capture fixture failed")));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(extensions.processes.is_empty());
    assert!(extensions.pi_mirror_directory.is_none());
    assert!(extensions
        .summaries
        .iter()
        .any(|summary| summary.name == "octet-pi-compat" && !summary.enabled && !summary.running));
    assert!(extensions
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("Pi setup mirror could not start")));
}

#[tokio::test(flavor = "multi_thread")]
async fn retired_pi_capture_cannot_publish_or_keep_running() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("extensions");
    let directory = write_pi_preparation_fixture(&root, false);
    let config = deferred_fixture_config(temp.path(), &root, &["octet-pi-compat"]);
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let mut host = ExtensionHost::new();
    let mut extensions = start_pi_preparation_fixture(&config, &mut host, &session, &sessions);
    wait_for_pi_capture(&directory).await;
    extensions.release_binding().await;
    assert!(!extensions.startup_pending());
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(directory.join("release-capture"), "release").unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!directory.join("capture-complete").exists());
    let progress = extensions.pump_deferred_startup(&mut host, &session).await;
    assert!(progress.attached.is_empty());
    assert!(extensions.runtime_binding.is_none());
    assert!(extensions.pi_mirror_directory.is_none());
    assert!(extensions.processes.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn deferred_pi_capture_still_requires_trust_and_process_admission() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".octet/extensions");
    let directory = write_pi_preparation_fixture(&root, false);
    let mut config = deferred_fixture_config(temp.path(), &root, &["octet-pi-compat"]);
    config.extension_paths.clear();
    config.workspace_trusted = true;
    for no_process in [false, true] {
        let mut config = config.clone();
        if no_process {
            config.start_extension_processes = false;
        } else {
            config.invocation_trusted_extensions.clear();
        }
        let sessions = SessionStore::new(&config.session_dir, temp.path());
        let session =
            Session::create(temp.path().join(format!("session-{no_process}.jsonl"))).unwrap();
        let mut host = ExtensionHost::new();
        let extensions = start_pi_preparation_fixture(&config, &mut host, &session, &sessions);
        assert!(!extensions.startup_pending());
        assert!(extensions.processes.is_empty());
        assert!(!directory.join("capture-started").exists());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deferred_pi_catalog_keeps_preexisting_tool_clashes_ineligible() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("extensions");
    let directory = write_pi_preparation_fixture(&root, false);
    for name in ["octet-web-search", "external-search"] {
        let fixture = write_fixture(&root, name, &[], 0);
        let path = fixture.join(EXTENSION_MANIFEST_FILENAME);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            path,
            text.replace(
                "[entrypoint]",
                "contributes = { tools = [\"fixture_search\"] }\n[entrypoint]",
            ),
        )
        .unwrap();
    }
    let config = deferred_fixture_config(
        temp.path(),
        &root,
        &["octet-pi-compat", "octet-web-search", "external-search"],
    );
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let mut host = ExtensionHost::new();
    let mut extensions = start_pi_preparation_fixture(&config, &mut host, &session, &sessions);
    assert!(extensions
        .deferred_startup
        .as_ref()
        .unwrap()
        .tool_clashes
        .contains_key("external-search"));
    std::fs::write(directory.join("release-capture"), "release").unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        extensions.await_pending_prompt_hooks(&mut host, &session),
    )
    .await
    .unwrap()
    .unwrap();
    let catalog = extensions.runtime_manager.as_ref().unwrap().catalog();
    assert!(
        !catalog
            .get("external-search")
            .unwrap()
            .descriptor
            .activation
            .enabled
    );
    assert!(extensions
        .start_failures
        .get("external-search")
        .unwrap()
        .contains("clashes"));
    assert!(!extensions
        .processes
        .iter()
        .any(|process| process.descriptor().manifest.name == "external-search"));
}
