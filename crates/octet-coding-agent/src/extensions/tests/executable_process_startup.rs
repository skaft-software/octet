//! Starting real extension processes under the effect and trust policies.
//!
//! These are the slow ones: they launch an executable extension and observe the
//! result. Covers a controlled policy refusing to launch an enabled, ungranted
//! executable, and an installed extension staying disabled by default
//! with full-access trust not being persisted.

use super::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn controlled_policy_needs_host_authority_to_launch_extension() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let extension_root = temp.path().join(".octet/extensions");
    let extension_dir = extension_root.join("controlled-launch-probe");
    std::fs::create_dir_all(&extension_dir).unwrap();
    std::fs::write(
        extension_dir.join(EXTENSION_MANIFEST_FILENAME),
        r#"name = "controlled-launch-probe"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "launch-probe.sh"
"#,
    )
    .unwrap();
    let executable = extension_dir.join("launch-probe.sh");
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf launched > \"$OCTET_WORKSPACE/controlled-extension-launched\"\nexit 1\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).unwrap();

    for (effect_policy, allow_process) in [
        (octet_agent::EffectPolicy::Controlled, true),
        (octet_agent::EffectPolicy::ControlledBashApproval, true),
        (octet_agent::EffectPolicy::UnsafeHost, false),
    ] {
        let mut config =
            executable_extension_config(temp.path(), &extension_root, "controlled-launch-probe");
        config.effect_policy = effect_policy;
        config.sandbox.allow_process = allow_process;
        config.invocation_trusted_extensions.clear();
        config.extension_paths.clear();
        config.workspace_trusted = true;
        assert!(config.sandbox.allow_shell);
        let session =
            Session::create(temp.path().join(format!("session-{effect_policy:?}.jsonl"))).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let sessions = SessionStore::new(&config.session_dir, temp.path());
        let mut host = ExtensionHost::new();

        let mut extensions = ExecutableExtensions::discover_and_start(
            &config,
            &session,
            &model,
            &ReasoningConfig::Off,
            &sessions,
            &mut host,
        );

        assert!(!temp.path().join("controlled-extension-launched").exists());
        assert!(extensions.processes.is_empty());
        assert!(extensions.summaries.iter().any(|extension| {
            extension.name == "controlled-launch-probe"
                && extension.enabled
                && extension.trusted == (effect_policy == octet_agent::EffectPolicy::UnsafeHost)
                && !extension.running
        }));
        if allow_process {
            assert!(extensions
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic == CONTROLLED_EXTENSION_START_DIAGNOSTIC));
        } else {
            assert!(extensions
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("process execution is disabled")));
        }
        extensions.shutdown().await;
    }
}

/// The native-host protocol reports discovery only. Activation, full access,
/// an explicit directory and a persistent grant together still start nothing.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn native_host_surface_never_starts_granted_extensions() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let extension_root = temp.path().join("explicit-extensions");
    let extension_dir = extension_root.join("host-launch-probe");
    std::fs::create_dir_all(&extension_dir).unwrap();
    let manifest = extension_dir.join(EXTENSION_MANIFEST_FILENAME);
    std::fs::write(
        &manifest,
        r#"name = "host-launch-probe"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "launch-probe.sh"
"#,
    )
    .unwrap();
    let executable = extension_dir.join("launch-probe.sh");
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf launched > \"$OCTET_WORKSPACE/host-extension-launched\"\nexit 1\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).unwrap();

    for effect_policy in [
        octet_agent::EffectPolicy::Controlled,
        octet_agent::EffectPolicy::UnsafeHost,
    ] {
        let mut config =
            executable_extension_config(temp.path(), &extension_root, "host-launch-probe");
        config.effect_policy = effect_policy;
        config.start_extension_processes = false;
        config
            .trusted_extensions
            .push(format!("host-launch-probe@{}", manifest.display()));
        let session =
            Session::create(temp.path().join(format!("session-{effect_policy:?}.jsonl"))).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let sessions = SessionStore::new(&config.session_dir, temp.path());
        let mut host = ExtensionHost::new();

        let mut extensions = ExecutableExtensions::discover_and_start(
            &config,
            &session,
            &model,
            &ReasoningConfig::Off,
            &sessions,
            &mut host,
        );

        assert!(!temp.path().join("host-extension-launched").exists());
        assert!(extensions.processes.is_empty());
        assert!(extensions.summaries.iter().any(|extension| {
            extension.name == "host-launch-probe" && extension.enabled && !extension.running
        }));
        assert!(extensions
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic == NATIVE_HOST_EXTENSION_START_DIAGNOSTIC));
        assert!(!extensions
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("host authority")));
        extensions.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn existing_source_bound_trust_config_starts_under_safe_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".octet/extensions");
    let directory = root.join("migrated");
    std::fs::create_dir_all(&directory).unwrap();
    let manifest = directory.join(EXTENSION_MANIFEST_FILENAME);
    std::fs::write(&manifest, "name = 'migrated'\nversion = '0.1.0'\napi_version = '0.1'\n[entrypoint]\ncommand = 'extension.sh'\n").unwrap();
    let executable = directory.join("extension.sh");
    std::fs::write(&executable, "#!/bin/sh\nprintf launched > \"$OCTET_WORKSPACE/safe-mode-launched\"\nIFS= read -r initialize\nid=$(printf '%s\\n' \"$initialize\" | sed -n 's/.*\"id\":\\([0-9][0-9]*\\).*/\\1/p')\nprintf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"api_version\":\"0.1\",\"tools\":[],\"commands\":[]}}\\n' \"$id\"\nwhile IFS= read -r request; do :; done\n").unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).unwrap();

    // A 0.8.1 user-config entry remains a per-source host authority grant.
    let content = format!(
        "enabled_extensions = ['migrated']\ntrusted_extensions = ['migrated@{}']\n",
        manifest.display()
    );
    let existing: toml::Value = toml::from_str(&content).unwrap();
    let mut config = executable_extension_config(temp.path(), &root, "migrated");
    config.workspace_trusted = true;
    config.extension_paths.clear();
    config.effect_policy = octet_agent::EffectPolicy::ControlledBashApproval;
    config.invocation_trusted_extensions.clear();
    config.trusted_extensions = existing["trusted_extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect();
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let mut host = ExtensionHost::new();
    let mut extensions = ExecutableExtensions::discover_and_start(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
    );
    assert!(
        extensions
            .summaries
            .iter()
            .any(|summary| summary.name == "migrated"
                && summary.enabled
                && summary.trusted
                && summary.running),
        "{:?}",
        extensions.diagnostics.entries
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("safe-mode-launched")).unwrap(),
        "launched"
    );
    extensions.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn installed_extension_is_disabled_by_default_and_full_access_trust_is_not_persisted() {
    use octet_agent::EffectPolicy::{Controlled, ControlledBashApproval, UnsafeHost};

    for (effect_policy, allow_process, allow_shell, explicit_trust) in [
        (Controlled, true, true, false),
        (ControlledBashApproval, true, true, false),
        (Controlled, true, true, true),
        (ControlledBashApproval, true, true, true),
        (UnsafeHost, false, true, false),
        (UnsafeHost, false, true, true),
        (UnsafeHost, true, false, false),
        (UnsafeHost, true, false, true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("extensions");
        let name = "policy-fixture";
        let manifest = format!(
            r#"name = "policy-fixture"
    version = "0.1.0"
    api_version = "0.2"
    requires_octet = "={}"
    [entrypoint]
    command = "extension.sh"
    [runtime]
    lifecycle = "workspace_service"
    sharing = "workspace"
    "#,
            env!("CARGO_PKG_VERSION")
        );
        let script = r#"#!/bin/sh
    printf 'launched\n' >> "$OCTET_WORKSPACE/policy-extension-starts"
    while IFS= read -r request; do
      id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      case "$request" in
        *'"method":"initialize"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.2","tools":[],"commands":[],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}\n' "$id"
          ;;
        *'"method":"shutdown"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
          exit 0
          ;;
      esac
    done
    "#;
        let archive_path = temp.path().join("policy-fixture.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&archive_path).unwrap(),
            flate2::Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_cksum();
        archive
            .append_data(&mut header, name, std::io::empty())
            .unwrap();
        for (file, body, mode) in [
            ("extension.toml", manifest.as_bytes(), 0o644),
            ("extension.sh", script.as_bytes(), 0o755),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_mode(mode);
            header.set_size(body.len() as u64);
            header.set_cksum();
            archive
                .append_data(&mut header, format!("{name}/{file}"), body)
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
        crate::extension_bundle::install_local(&root, &archive_path, false).unwrap();
        let install_path = root.join(name).join("install.json");
        let install_record = std::fs::read(&install_path).unwrap();
        let marker = temp.path().join("policy-extension-starts");
        assert!(!marker.exists(), "installation must never execute code");
        assert!(!temp.path().join("config.toml").exists());

        let mut config = executable_extension_config(temp.path(), &root, name);
        config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
        config.enabled_extensions.clear();
        config.invocation_trusted_extensions.clear();
        let session = Session::create(temp.path().join("session.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let sessions = SessionStore::new(&config.session_dir, temp.path());
        let mut host = ExtensionHost::new();
        let mut disabled = ExecutableExtensions::discover_and_start(
            &config,
            &session,
            &model,
            &ReasoningConfig::Off,
            &sessions,
            &mut host,
        );
        assert!(disabled.summaries.iter().any(|entry| {
            entry.name == name && !entry.enabled && entry.trusted && !entry.running
        }));
        assert!(disabled.processes.is_empty());
        assert!(!marker.exists());
        disabled.shutdown().await;

        config.enabled_extensions.push(name.into());
        let mut started = ExecutableExtensions::discover_and_start(
            &config,
            &session,
            &model,
            &ReasoningConfig::Off,
            &sessions,
            &mut host,
        );
        assert!(
            started.summaries.iter().any(|entry| {
                entry.name == name && entry.enabled && entry.trusted && entry.running
            }),
            "{:?}",
            started.diagnostics.entries
        );
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "launched\n");
        assert!(config.trusted_extensions.is_empty());
        assert!(config.invocation_trusted_extensions.is_empty());

        // Retained services must obey policy/process floors even if trust
        // remains valid through full access or an explicit invocation grant.
        config.effect_policy = effect_policy;
        config.sandbox.allow_process = allow_process;
        config.sandbox.allow_shell = allow_shell;
        if explicit_trust {
            config.invocation_trusted_extensions.push(name.into());
        }
        let expected_running = allow_process && allow_shell; // --extension-dir grants authority.
        let mut safe_host = ExtensionHost::new();
        let mut safe = ExecutableExtensions::discover_and_start_with_runtime_manager(
            &config,
            &session,
            &model,
            &ReasoningConfig::Off,
            &sessions,
            &mut safe_host,
            started.runtime_manager(),
        );
        assert!(safe.summaries.iter().any(|entry| {
            entry.name == name
                && entry.enabled
                && entry.trusted
                && entry.running == expected_running
        }));
        assert_eq!(safe.processes.len(), usize::from(expected_running));
        if !allow_process || !allow_shell {
            assert!(safe
                .diagnostics
                .iter()
                .any(|entry| entry.contains("process execution is disabled")));
        } else {
            assert!(!safe
                .diagnostics
                .iter()
                .any(|entry| entry == CONTROLLED_EXTENSION_START_DIAGNOSTIC));
        }
        if !expected_running {
            assert_eq!(
                started.processes[0].health_snapshot().state,
                ExtensionHealthState::Stopped
            );
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "launched\n");
        } else {
            // An eligible workspace service may be retained instead of restarted.
            let starts = std::fs::read_to_string(&marker).unwrap();
            assert!(
                starts == "launched\n" || starts == "launched\nlaunched\n",
                "{starts:?}"
            );
        }
        assert!(config.trusted_extensions.is_empty());
        assert_eq!(
            config.invocation_trusted_extensions.is_empty(),
            !explicit_trust
        );
        assert_eq!(std::fs::read(&install_path).unwrap(), install_record);
        assert!(!temp.path().join("config.toml").exists());
        safe.shutdown().await;
        started.shutdown().await;
    }
}

#[tokio::test]
async fn subagents_runtime_controls_never_appear_in_generated_extension_options() {
    use std::os::unix::fs::PermissionsExt as _;

    for name in [SUBAGENTS_EXTENSION_NAME, "other-extension"] {
        for supports_menu in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let script = temp.path().join("options-fixture.sh");
            std::fs::write(
                &script,
                r#"#!/bin/sh
request_id() { sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
IFS= read -r initialize
id=$(printf '%s' "$initialize" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.4","tools":[],"commands":[{"name":"subagents","description":"Worker controls"},{"name":"configure","description":"Configuration"}],"protocol":{"version":"0.4","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}\n' "$id"
while IFS= read -r request; do
  id=$(printf '%s' "$request" | request_id)
  case "$request" in
    *'"method":"menu/collect"'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32000,"message":"options unavailable"}}\n' "$id"
      ;;
    *'"method":"shutdown"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      exit 0
      ;;
  esac
done
"#,
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let manifest = ExtensionManifest::parse(&format!(
                r#"name = {name:?}
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "options-fixture.sh"
[contributes]
commands = ["subagents", "configure"]
menu = {supports_menu}
"#,
            ))
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
            extensions.processes.push(process.clone());
            let options = extensions.options_menu(name).await;
            let options = if supports_menu {
                assert!(options.is_err(), "fixture must exercise menu failure");
                extensions.generated_options_menu(name).unwrap()
            } else {
                options.unwrap().unwrap()
            };
            assert!(options.generated);
            let commands = options
                .menu
                .items
                .iter()
                .map(|item| item.command.as_deref().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                commands,
                if name == SUBAGENTS_EXTENSION_NAME {
                    vec!["configure"]
                } else {
                    vec!["subagents", "configure"]
                },
                "{name}, menu={supports_menu}"
            );
            assert!(process.shutdown().await);
        }
    }
}

/// Two enabled extensions declaring one tool name must not fail startup. The
/// first-party extension keeps the name; the external one is turned off with a
/// notice that names the clash, and it never launches.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn tool_clash_turns_off_the_external_extension_instead_of_failing() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let extension_root = temp.path().join("explicit-extensions");
    for (name, tools) in [
        ("octet-web-search", r#"["web_search"]"#),
        ("external-search", r#"["web_search", "external_only"]"#),
    ] {
        let directory = extension_root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(EXTENSION_MANIFEST_FILENAME),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\napi_version = \"0.1\"\n\n[entrypoint]\ncommand = \"launch.sh\"\n\n[contributes]\ntools = {tools}\n"
            ),
        )
        .unwrap();
        let executable = directory.join("launch.sh");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\nprintf launched > \"$OCTET_WORKSPACE/{name}-launched\"\nexit 1\n"),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
    }
    let mut config = executable_extension_config(temp.path(), &extension_root, "octet-web-search");
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.enabled_extensions.push("external-search".into());
    config
        .invocation_trusted_extensions
        .push("external-search".into());
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let sessions = SessionStore::new(&config.session_dir, temp.path());
    let mut host = ExtensionHost::new();

    let mut extensions = ExecutableExtensions::discover_and_start(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
    );

    assert!(temp.path().join("octet-web-search-launched").exists());
    assert!(!temp.path().join("external-search-launched").exists());
    assert!(
        extensions.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("\"external-search\" was turned off")
                && diagnostic.contains("`web_search` (provided by \"octet-web-search\")")
        }),
        "{:?}",
        extensions.diagnostics.iter().collect::<Vec<_>>()
    );
    let external = extensions
        .summaries
        .iter()
        .find(|extension| extension.name == "external-search")
        .unwrap();
    assert!(!external.running);
    assert!(external
        .health
        .as_ref()
        .and_then(|health| health.last_error.as_deref())
        .is_some_and(|error| error.starts_with("turned off:")));
    extensions.shutdown().await;
}
