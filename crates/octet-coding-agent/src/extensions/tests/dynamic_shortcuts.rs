//! Extensions registering their own key bindings at runtime.
//!
//! Covers host-binding registration (first owner wins, reserved host bindings are
//! never taken over) and dispatch of a registered shortcut into the owning
//! process. Split from the terminal-grant tests because the ownership rule here
//! is about *key bindings* rather than terminal grants.

use super::*;

fn shortcut(key: &str, name: &str) -> ShortcutDefinition {
    ShortcutDefinition {
        key: key.to_owned(),
        name: name.to_owned(),
        description: format!("{name} action"),
    }
}

#[test]
fn shortcut_registration_keeps_host_bindings_and_first_owner() {
    let mut registered = Vec::new();
    let mut diagnostics = Vec::new();
    register_extension_shortcut(
        &mut registered,
        &mut diagnostics,
        "trusted-one",
        &shortcut("ctrl+shift+p", "first"),
    );
    register_extension_shortcut(
        &mut registered,
        &mut diagnostics,
        "trusted-two",
        &shortcut("control+shift+p", "second"),
    );
    register_extension_shortcut(
        &mut registered,
        &mut diagnostics,
        "trusted-three",
        &shortcut("ctrl+d", "close"),
    );
    register_extension_shortcut(
        &mut registered,
        &mut diagnostics,
        "trusted-four",
        &shortcut("shift+p", "editor"),
    );

    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].invocation.extension, "trusted-one");
    assert_eq!(registered[0].invocation.name, "first");
    assert!(diagnostics
        .iter()
        .any(|message| message.contains("conflicts")));
    assert!(diagnostics
        .iter()
        .any(|message| message.contains("reserved")));
    assert!(diagnostics
        .iter()
        .any(|message| message.contains("must include ctrl, alt, or super")));
}

#[cfg(unix)]
#[tokio::test]
async fn shortcut_dispatch_runs_registered_action_in_background() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("shortcut-fixture.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.2","tools":[],"commands":[],"shortcuts":[{"key":"ctrl+shift+p","name":"open_panel","description":"Open the panel"}],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}'
IFS= read -r shortcut
case "$shortcut" in
  *'"method":"shortcut/execute"'*'"name":"open_panel"'*) ;;
  *) exit 41 ;;
esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"text":"shortcut output","notifications":[],"context":[]}}'
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"name = "shortcut-fixture"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "shortcut-fixture.sh"
[contributes]
shortcuts = [{ key = "ctrl+shift+p", name = "open_panel", description = "Open the panel" }]
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
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
    extensions.processes.push(process);
    let (shortcuts, diagnostics) = register_extension_shortcuts(&extensions.processes);
    assert!(diagnostics.is_empty());
    extensions.shortcuts = shortcuts;

    let event = Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('P'),
        crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
    ));
    let invocation = extensions
        .dispatch_shortcut_for_event(&event)
        .expect("registered shortcut is dispatched");
    assert_eq!(invocation.extension, "shortcut-fixture");
    assert_eq!(invocation.name, "open_panel");

    let messages = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = extensions.drain_background_updates();
            if !updates.shortcut_messages.is_empty() {
                return updates.shortcut_messages;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("shortcut result was not delivered");
    assert_eq!(messages, vec!["shortcut output"]);
    extensions.shutdown().await;
}
