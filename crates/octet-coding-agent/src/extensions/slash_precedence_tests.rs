use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn pi_commands_keep_non_pi_native_names_and_namespaced_collisions() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("commands.sh");
    std::fs::write(&fixture, r#"#!/bin/sh
IFS= read -r initialize
id=$(printf '%s' "$initialize" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.1","tools":[],"commands":[{"name":"goal","description":"Extension goal"},{"name":"search","description":"Extension search"},{"name":"model","description":"Conflicting model"}]}}\n' "$id"
while IFS= read -r request; do :; done
"#).unwrap();
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manifest = ExtensionManifest::parse(
        r#"
name = "octet-pi-compat"
version = "0.1.0"
api_version = "0.1"
[entrypoint]
command = "commands.sh"
[contributes]
commands = ["goal", "search", "model"]
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: root.path().join("extension.toml"),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(root.path()),
    )
    .await
    .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.processes.push(process);
    let names = extensions
        .command_suggestions()
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert!(names.contains(&"goal".into()), "{names:?}");
    assert!(names.contains(&"search".into()), "{names:?}");
    assert!(names.contains(&"octet-pi-compat:model".into()), "{names:?}");
    assert!(
        matches!(extensions.parse_command("/goal pause"), crate::commands::Command::Unknown(text) if text == "/goal pause")
    );
    assert_eq!(
        extensions.parse_command("/octet:goal pause"),
        crate::commands::parse("/goal pause")
    );
    assert_eq!(
        extensions.parse_command("/model"),
        crate::commands::parse("/model")
    );
    assert_eq!(
        extensions.command_owner("goal").as_deref(),
        Some("octet-pi-compat")
    );
    assert_eq!(
        extensions
            .registered_command_name("octet-pi-compat", "octet-pi-compat:model")
            .as_deref(),
        Some("model")
    );
    extensions.shutdown().await;
}

#[test]
fn native_command_escape_keeps_goal_and_model_accessible() {
    for command in ["goal", "goal clear", "model", "skills search needle"] {
        let native = crate::commands::parse(&format!("/{command}"));
        assert!(
            !matches!(native, crate::commands::Command::Unknown(_)),
            "fixture must name a valid native command: {command}"
        );
        assert_eq!(crate::commands::parse(&format!("/octet:{command}")), native);
    }
    assert!(matches!(
        crate::commands::parse("/octet:does-not-exist"),
        crate::commands::Command::Unknown(_)
    ));
}
