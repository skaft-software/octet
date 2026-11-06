//! A failing, stalled or dead extension never refuses the user's prompt.
//!
//! Pi reports a throwing handler and keeps going. Before this suite existed the
//! host turned any before_prompt failure into a refusal that repeated for every
//! later prompt of the session.

use super::*;

#[cfg(unix)]
#[tokio::test]
async fn a_failed_then_dead_extension_never_blocks_prompts() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("fails-then-dies.sh");
    // Answers initialize, fails the first before_prompt hook with the same RPC
    // error the S07 todo run produced, then exits so the process is gone.
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
set -eu
IFS= read -r request
id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.1","tools":[],"commands":[]}}\n' "$id"
IFS= read -r request
id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"invalid_request: missing field session_leaf"}}\n' "$id"
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"
name = "fails-then-dies"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "fails-then-dies.sh"

[contributes]
hooks = ["before_prompt"]
"#,
    )
    .unwrap();
    let manifest_path = temp.path().join(EXTENSION_MANIFEST_FILENAME);
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.request_timeout = Duration::from_secs(2);
    runtime.shutdown_timeout = Duration::from_secs(2);
    runtime.supervise = false;
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path,
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
    extensions.processes.push(process.clone());

    // One failing hook: the prompt goes through unchanged with one short notice.
    let first = extensions
        .compose_prompt("system", "first prompt".into())
        .await
        .expect("a failing before_prompt hook must not refuse the prompt");
    assert_eq!(first.system, "system");
    assert_eq!(first.prompt, "first prompt");
    let notices = first
        .notifications
        .iter()
        .filter(|notice| notice.contains("continuing without it"))
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1, "{:?}", first.notifications);
    assert!(notices[0].contains("fails-then-dies"));
    assert!(notices[0].split_whitespace().count() <= 10);
    assert!(extensions
        .diagnostics
        .iter()
        .any(|message| message.contains("before_prompt hook failed")
            && message.contains("missing field session_leaf")));

    // The process is now gone: every later prompt still goes through, without
    // a deadline wait and without repeating the notice.
    for _ in 0..100 {
        if !process.is_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!process.is_running(), "fixture process should have exited");
    let started = std::time::Instant::now();
    for index in 0..3 {
        let later = extensions
            .compose_prompt("system", format!("later prompt {index}"))
            .await
            .expect("a dead extension must not refuse later prompts");
        assert_eq!(later.prompt, format!("later prompt {index}"));
        assert!(
            later
                .notifications
                .iter()
                .all(|notice| !notice.contains("continuing without it")),
            "the notice is shown once: {:?}",
            later.notifications
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a dead extension must be skipped, not waited for"
    );
    assert!(extensions
        .diagnostics
        .iter()
        .any(|message| message.contains("is not running")));
    extensions.shutdown().await;
}
