//! Confirmation and interactive-input prompts owned by the interactive shell.
//!
//! Covers a `confirmation/request` being subscribed, answered, and unblocking the
//! extension, an `input/request` being routed through the interactive owner, and
//! a non-interactive frontend denying a confirmation instead of blocking.

use super::support::*;
use super::*;

#[cfg(unix)]
#[tokio::test]
async fn command_confirmation_is_subscribed_answered_and_unblocks_the_extension() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("confirmation-fixture.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[{"name":"guarded","description":"Wait for an explicit confirmation","usage":"/guarded"}]}}'
IFS= read -r command
printf '%s\n' '{"jsonrpc":"2.0","id":"fixture-confirmation","method":"confirmation/request","params":{"prompt":"Allow fixture command?","detail":"The fixture will not finish until octet answers.","destructive":false,"default":false}}'
IFS= read -r confirmation
case "$confirmation" in
  *'"id":"fixture-confirmation"'*'"confirmed":true'*) ;;
  *) exit 41 ;;
esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"text":"fixture completed","notifications":[],"context":[]}}'
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"
name = "confirmation-fixture"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "confirmation-fixture.sh"

[contributes]
commands = ["guarded"]
confirmations = true
"#,
    )
    .unwrap();
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
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();

    // Mirror product startup: retain the process's startup-buffered event
    // receiver before command-specific confirmation routing subscribes.
    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    let mut confirmations = RecordingConfirmationHandler::default();

    let output = tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_command_with_confirmation("guarded", Vec::new(), &mut confirmations),
    )
    .await
    .expect("command remained blocked waiting for confirmation")
    .unwrap()
    .expect("fixture command was not registered");

    assert_eq!(output, "fixture completed");
    assert_eq!(
        confirmations.calls,
        vec![(
            "confirmation-fixture".to_owned(),
            "Allow fixture command?".to_owned()
        )]
    );
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn command_input_is_routed_through_the_interactive_owner() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("input-fixture.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.2","tools":[],"commands":[{"name":"configure","description":"Collect one secret","usage":"/configure"}],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}'
IFS= read -r command
id=$(printf '%s\n' "$command" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":"fixture-input","method":"input/request","params":{"parent_request_id":%s,"prompt":"API key:","secret":true}}\n' "$id"
IFS= read -r answer
case "$answer" in
  *'fixture-secret'*) ;;
  *) exit 42 ;;
esac
printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"configured","notifications":[],"context":[]}}\n' "$id"
IFS= read -r shutdown
id=$(printf '%s\n' "$shutdown" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"
name = "input-fixture"
version = "0.1.0"
api_version = "0.2"

[entrypoint]
command = "input-fixture.sh"

[contributes]
commands = ["configure"]
"#,
    )
    .unwrap();
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
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();

    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    let mut interaction = RecordingConfirmationHandler {
        input_value: Some("fixture-secret".to_owned()),
        ..RecordingConfirmationHandler::default()
    };
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_command_with_confirmation("configure", Vec::new(), &mut interaction),
    )
    .await
    .expect("command remained blocked waiting for input")
    .unwrap()
    .expect("fixture command was not registered");

    assert_eq!(output, "configured");
    assert_eq!(
        interaction.input_calls,
        vec![("input-fixture".to_owned(), "API key:".to_owned(), true)]
    );
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn noninteractive_command_confirmation_is_denied() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("noninteractive-confirmation-fixture.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[{"name":"guarded","description":"Wait for an explicit confirmation","usage":"/guarded"}]}}'
IFS= read -r command
printf '%s\n' '{"jsonrpc":"2.0","id":"fixture-confirmation","method":"confirmation/request","params":{"prompt":"Allow fixture command?","detail":"The fixture will not finish until octet answers.","destructive":false,"default":false}}'
IFS= read -r confirmation
case "$confirmation" in
  *'"id":"fixture-confirmation"'*'"confirmed":false'*) ;;
  *) exit 41 ;;
esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"text":"fixture denied","notifications":[],"context":[]}}'
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"
name = "noninteractive-confirmation-fixture"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "noninteractive-confirmation-fixture.sh"

[contributes]
commands = ["guarded"]
confirmations = true
"#,
    )
    .unwrap();
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
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();

    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_command_without_confirmation("guarded", Vec::new()),
    )
    .await
    .expect("command remained blocked waiting for confirmation")
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("requires an interactive confirmation surface"));
    assert!(process.shutdown().await);
}
