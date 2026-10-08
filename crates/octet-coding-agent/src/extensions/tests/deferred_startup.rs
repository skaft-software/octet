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
        .await_pending_prompt_hooks(&mut host)
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
    let progress = extensions.pump_deferred_startup(&mut host).await;
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
