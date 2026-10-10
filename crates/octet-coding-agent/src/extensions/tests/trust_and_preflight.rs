//! Trust resolution and the preflight that decides what may start.
//!
//! Covers provider preflight excluding non-provider extensions, the CLI flag
//! declarations that require enablement plus exact trust before a process is
//! started, the subagents extension's full-access-only preflight, offering the
//! active-session lifecycle to interactive frontends only, and the three ways a
//! name or a path can be trusted (global name never transfers to a project
//! shadow, directory and manifest names must match, exact paths normalise only
//! the parent).

use super::*;

fn write_extension_manifest(directory: &Path, name: &str, description: &str) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(
        directory.join(EXTENSION_MANIFEST_FILENAME),
        format!(
            r#"name = {name:?}
version = "0.1.0"
api_version = "0.1"
description = {description:?}

[entrypoint]
command = "does-not-exist"
"#
        ),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn provider_preflight_config_excludes_non_provider_extensions() {
    let temp = tempfile::tempdir().unwrap();
    let extension_root = temp.path().join("extensions");
    write_extension_manifest(
        &extension_root.join("ordinary"),
        "ordinary",
        "ordinary tool",
    );
    let provider = extension_root.join("provider");
    std::fs::create_dir_all(&provider).unwrap();
    std::fs::write(
        provider.join(EXTENSION_MANIFEST_FILENAME),
        r#"name = "provider"
version = "0.3.0"
api_version = "0.3"

[entrypoint]
command = "must-not-run"

[contributes]
providers = true
"#,
    )
    .unwrap();
    let mut config = executable_extension_config(temp.path(), &extension_root, "ordinary");
    config.enabled_extensions.push("provider".into());

    let preflight = provider_preflight_config(&config);
    assert_eq!(preflight.enabled_extensions, vec!["provider"]);
}

#[cfg(unix)]
#[test]
fn selected_cli_flags_require_enablement_and_exact_trust_without_starting_processes() {
    let temp = tempfile::tempdir().unwrap();
    let extension_root = temp.path().join(".octet/extensions");
    for (name, flag) in [
        ("flag-trusted", "trusted-option"),
        ("flag-untrusted", "workspace"),
    ] {
        let directory = extension_root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(EXTENSION_MANIFEST_FILENAME),
            format!(
                r#"name = {name:?}
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "must-not-run"
[contributes]
flags = [{{ name = {flag:?}, type = "boolean", default = false }}]
"#
            ),
        )
        .unwrap();
    }
    let mut config = executable_extension_config(temp.path(), &extension_root, "flag-trusted");
    config.enabled_extensions.push("flag-untrusted".into());
    config.extension_paths.clear();
    config.workspace_trusted = true;
    let selected = selected_extension_flag_declarations(&config);
    assert_eq!(
        selected
            .iter()
            .map(|(extension, flag)| (extension.as_str(), flag.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("flag-trusted", "trusted-option")]
    );
}

#[cfg(unix)]
#[test]
fn full_access_cli_flags_need_enablement_but_no_extra_trust() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".octet/extensions");
    let directory = root.join("flag-fixture");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(EXTENSION_MANIFEST_FILENAME),
        r#"name = "flag-fixture"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "must-not-run"
[contributes]
flags = [{ name = "fixture-option", type = "boolean", default = false }]
"#,
    )
    .unwrap();
    let mut config = executable_extension_config(temp.path(), &root, "flag-fixture");
    config.invocation_trusted_extensions.clear();
    config.extension_paths.clear();
    config.workspace_trusted = true;
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    assert_eq!(selected_extension_flag_declarations(&config).len(), 1);
    config.enabled_extensions.clear();
    assert!(selected_extension_flag_declarations(&config).is_empty());
    config.enabled_extensions.push("flag-fixture".into());
    for policy in [
        octet_agent::EffectPolicy::Controlled,
        octet_agent::EffectPolicy::ControlledBashApproval,
    ] {
        config.effect_policy = policy;
        assert!(selected_extension_flag_declarations(&config).is_empty());
    }
    assert!(config.trusted_extensions.is_empty());
    assert!(config.invocation_trusted_extensions.is_empty());
}

#[cfg(unix)]
#[test]
fn active_session_lifecycle_is_offered_only_to_interactive_frontends() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = executable_extension_config(temp.path(), temp.path(), "fixture");
    assert!(active_session_lifecycle_enabled(&config));

    config.mode = crate::config::Mode::Print {
        prompt: "one-shot".into(),
    };
    assert!(!active_session_lifecycle_enabled(&config));

    config.mode = crate::config::Mode::Rpc;
    assert!(!active_session_lifecycle_enabled(&config));
}

#[test]
fn global_name_trust_never_transfers_to_project_shadow() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let global_octet = temp.path().join("global/.octet");
    write_extension_manifest(
        &global_octet.join("extensions/git-tools"),
        "git-tools",
        "global",
    );
    write_extension_manifest(
        &workspace.join(".octet/extensions/git-tools"),
        "git-tools",
        "project",
    );
    let resolver = ResourceResolver::with_global_octet_dir(workspace, true, global_octet);
    let snapshot = resolver.discover(ResourceKind::Extension, &[]);
    let resource = snapshot.get("git-tools").unwrap();
    assert_eq!(resource.scope, ResourceScope::Project);

    let mut policy = ExtensionPolicy::default();
    policy.enable("git-tools");
    policy.trust("git-tools");
    let mut diagnostics = Vec::new();
    let descriptor =
        load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics).unwrap();

    assert_eq!(descriptor.source, ExtensionSource::Project);
    assert_eq!(
        descriptor.activation.trust,
        ExtensionTrust::Untrusted,
        "a bare global grant must not launch the shadowing project process"
    );
    assert_eq!(descriptor.manifest.description.as_deref(), Some("project"));
    assert!(diagnostics.is_empty());

    policy.trust_source("git-tools", resource.path.clone());
    let descriptor =
        load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics).unwrap();
    assert_eq!(descriptor.activation.trust, ExtensionTrust::Trusted);
}

#[test]
fn directory_and_manifest_names_must_match() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let global_octet = temp.path().join("global/.octet");
    write_extension_manifest(
        &global_octet.join("extensions/alias"),
        "actual-name",
        "mismatch",
    );
    let resolver = ResourceResolver::with_global_octet_dir(workspace, false, global_octet);
    let snapshot = resolver.discover(ResourceKind::Extension, &[]);
    let resource = snapshot.get("alias").unwrap();
    let mut diagnostics = Vec::new();

    for policy in [
        ExtensionPolicy::default(),
        ExtensionPolicy::for_effect_policy(octet_agent::EffectPolicy::UnsafeHost),
    ] {
        diagnostics.clear();
        let descriptor = load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics);

        assert!(descriptor.is_none());
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("must match manifest name")));
    }
}

#[test]
fn exact_trust_paths_normalize_only_the_parent() {
    let temp = tempfile::tempdir().unwrap();
    let extension = temp.path().join("git-tools");
    std::fs::create_dir_all(&extension).unwrap();
    let manifest = extension.join(EXTENSION_MANIFEST_FILENAME);

    assert_eq!(
        normalize_trusted_manifest_path(&manifest).unwrap(),
        extension
            .canonicalize()
            .unwrap()
            .join(EXTENSION_MANIFEST_FILENAME)
    );
    assert!(
        normalize_trusted_manifest_path(Path::new("relative/git-tools/extension.toml")).is_err()
    );
    assert!(normalize_trusted_manifest_path(&extension.join("other.toml")).is_err());
}
