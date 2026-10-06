//! Tests for the `Cli` surface and the argument/config persistence helpers it owns.
//!
//! Why this is a separate module: `cli.rs` is the crate's argument surface and the
//! persistence helpers behind `--model` and `--theme`. Keeping the fixtures and
//! assertions in a sibling file leaves the parser readable as a straight list of
//! fields, and lets this suite grow without touching the grammar.

use super::*;

fn cwd() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn base() -> Cli {
    Cli {
        command: None,
        message: None,
        additional_messages: Vec::new(),
        parity: Default::default(),
        login: None,
        logout: None,
        headless: false,
        mode: None,
        print: false,
        continue_: false,
        resume: None,
        fork: None,
        model: None,
        reasoning: None,
        reasoning_mode: None,
        cache_retention: None,
        cache_warming: None,
        workspace: None,
        theme: None,
        theme_dirs: vec![],
        color: None,
        mouse: None,
        tern: None,
        plain: false,
        show_images: false,
        show_reasoning: false,
        max_turns: None,
        session_dir: None,
        prompt_template: None,
        debug_prompt: false,
        telemetry: None,
        prompt_templates: vec![],
        skill_dirs: vec![],
        extension_dirs: vec![],
        enable_extensions: vec![],
        trust_extensions: vec![],
        experimental_streamable_http_mcp: false,
        workspace_trusted: false,
        safe_mode: false,
        effect_policy: None,
        tools: None,
        powershell: false,
        exclude_tools: vec![],
        no_tools: false,
        no_edit: false,
        no_write: false,
        no_process: false,
        no_shell: false,
        allow_shell: false,
        allow_remote_read: false,
        shell_path: None,
        no_context_files: false,
        offline: false,
        strict_config: false,
        bash_timeout_secs: None,
        max_output_bytes: None,
        system_prompt: None,
    }
}

fn config_with_empty_global(cli: Cli, directory: &Path) -> anyhow::Result<Config> {
    build_config_with_global_path(cli, directory, Some(&directory.join("missing-global.toml")))
}

#[test]
fn gpt_6_astra_uses_the_generic_cli_model_path() {
    let directory = cwd();
    let cli = Cli::try_parse_from(["octet", "--model", "gpt-6-astra", "--offline"]).unwrap();
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(config.model, Some(octet_ai::ModelId("gpt-6-astra".into())));
    assert!(config.model_explicit);

    let catalog = octet_ai::ModelCatalog::builtin().unwrap();
    let model = catalog.resolve(config.model.as_ref().unwrap()).unwrap();
    assert_eq!(model.spec.api_name, "gpt-6-astra");
    assert_eq!(model.endpoint.id.0, "openai");
}

#[test]
fn codex_context_window_flag_parses_and_fails_closed_above_the_cap() {
    let parsed = Cli::try_parse_from([
        "octet",
        "--codex-context-window",
        "500000",
        "--codex-context-window-acknowledge-cost-cliff",
    ])
    .unwrap();
    assert!(parsed.parity.validate().is_ok());
    let override_ = parsed.parity.codex_context_override().unwrap();
    assert_eq!(override_.requested_tokens, Some(500_000));
    assert!(override_.acknowledge_cost_cliff);

    // Garbage is refused by clap before validation runs.
    assert!(Cli::try_parse_from(["octet", "--codex-context-window", "garbage"]).is_err());
    // Zero, oversized and above-cap-without-acknowledgement fail validation.
    let zero = Cli::try_parse_from(["octet", "--codex-context-window", "0"]).unwrap();
    assert!(zero.parity.validate().is_err(), "zero must fail closed");
    let oversized = Cli::try_parse_from(["octet", "--codex-context-window", "2000000"]).unwrap();
    assert!(
        oversized.parity.validate().is_err(),
        "above every entitlement must fail"
    );
    let unacknowledged =
        Cli::try_parse_from(["octet", "--codex-context-window", "500000"]).unwrap();
    let error = unacknowledged.parity.validate().unwrap_err();
    assert!(error.to_string().contains("double-priced"), "{error}");
    assert!(error.to_string().contains("websocket"), "{error}");
    // The acknowledgement flag requires the window flag.
    assert!(
        Cli::try_parse_from(["octet", "--codex-context-window-acknowledge-cost-cliff"]).is_err()
    );
    // No flag leaves the deliberate default untouched (no override).
    let absent = Cli::try_parse_from(["octet", "--offline"]).unwrap();
    assert!(absent.parity.validate().is_ok());
    assert!(absent.parity.codex_context_override().is_none());
}

fn extension_flag(
    name: &str,
    kind: ExtensionFlagType,
    default: serde_json::Value,
) -> ExtensionFlag {
    ExtensionFlag {
        name: name.to_owned(),
        kind,
        default,
        description: Some(format!("{name} extension option")),
    }
}

#[test]
fn extension_flag_bootstrap_preserves_authority_options_after_dynamic_flags() {
    for safe in ["--safe-mode", "--safe"] {
        let args = [
            "octet",
            "--fixture-option",
            "--enable-extension",
            "fixture",
            safe,
        ]
        .map(OsString::from);
        let bootstrap = extension_flag_bootstrap(&args).unwrap();
        assert!(bootstrap.safe_mode);
        assert_eq!(bootstrap.enable_extensions, ["fixture"]);
        assert!(bootstrap.trust_extensions.is_empty());
    }
    for policy in ["unsafe_host", "controlled", "controlled_bash_approval"] {
        for options in [
            vec!["--effect-policy".to_owned(), policy.to_owned()],
            vec![format!("--effect-policy={policy}")],
        ] {
            let mut args = vec![OsString::from("octet"), OsString::from("--fixture-option")];
            args.extend(options.into_iter().map(OsString::from));
            let bootstrap = extension_flag_bootstrap(&args).unwrap();
            assert_eq!(bootstrap.effect_policy.as_deref(), Some(policy));
            assert!(!bootstrap.safe_mode);
        }
    }
    let args = ["octet", "--", "--safe-mode", "--effect-policy=controlled"].map(OsString::from);
    let bootstrap = extension_flag_bootstrap(&args).unwrap();
    assert!(!bootstrap.safe_mode);
    assert!(bootstrap.effect_policy.is_none());
}

#[test]
fn extension_flags_parse_types_defaults_inverses_and_help() {
    let registered = register_extension_flags(vec![
        (
            "flag-fixture".to_owned(),
            extension_flag(
                "fixture-enabled",
                ExtensionFlagType::Boolean,
                serde_json::json!(true),
            ),
        ),
        (
            "flag-fixture".to_owned(),
            extension_flag(
                "fixture-label",
                ExtensionFlagType::String,
                serde_json::json!("default"),
            ),
        ),
        (
            "flag-fixture".to_owned(),
            extension_flag(
                "fixture-count",
                ExtensionFlagType::Integer,
                serde_json::json!(2),
            ),
        ),
    ])
    .expect("register fixture flags");

    let default_matches = extension_flag_command(&registered)
        .try_get_matches_from(["octet"])
        .expect("parse default extension flags");
    let default_values =
        resolve_extension_flag_values(&default_matches, &registered).expect("resolve defaults");
    assert_eq!(
        default_values["flag-fixture"]["fixture-enabled"],
        serde_json::json!(true)
    );

    let matches = extension_flag_command(&registered)
        .try_get_matches_from([
            "octet",
            "--fixture-enabled",
            "--fixture-label",
            "custom",
            "--fixture-count",
            "-7",
        ])
        .expect("parse extension flags");
    assert!(Cli::from_arg_matches(&matches)
        .expect("project dynamic matches into the static CLI")
        .command
        .is_none());
    assert_eq!(
        resolve_extension_flag_values(&matches, &registered).expect("resolve values"),
        BTreeMap::from([(
            "flag-fixture".to_owned(),
            BTreeMap::from([
                ("fixture-count".to_owned(), serde_json::json!(-7)),
                ("fixture-enabled".to_owned(), serde_json::json!(true)),
                ("fixture-label".to_owned(), serde_json::json!("custom")),
            ]),
        )])
    );

    let inverse_matches = extension_flag_command(&registered)
        .try_get_matches_from(["octet", "--no-fixture-enabled"])
        .expect("parse inverse boolean");
    let inverse_values = resolve_extension_flag_values(&inverse_matches, &registered)
        .expect("resolve inverse boolean");
    assert_eq!(
        inverse_values["flag-fixture"]["fixture-enabled"],
        serde_json::json!(false)
    );
    assert_eq!(
        inverse_values["flag-fixture"]["fixture-label"],
        serde_json::json!("default")
    );
    assert_eq!(
        inverse_values["flag-fixture"]["fixture-count"],
        serde_json::json!(2)
    );

    let mut command = extension_flag_command(&registered);
    let help = command.render_long_help().to_string();
    assert!(help.contains("--fixture-enabled"));
    assert!(help.contains("--no-fixture-enabled"));
    assert!(help.contains("--fixture-label <STRING>"));
    assert!(help.contains("--fixture-count <INTEGER>"));
}

#[test]
fn extension_flags_reject_static_and_cross_extension_collisions() {
    for reserved in ["workspace", "help", "version"] {
        let static_collision = register_extension_flags(vec![(
            "fixture".to_owned(),
            extension_flag(reserved, ExtensionFlagType::String, serde_json::json!(".")),
        )])
        .expect_err("static option collision");
        assert!(static_collision.to_string().contains("conflicts"));
    }

    let dynamic_collision = register_extension_flags(vec![
        (
            "first".to_owned(),
            extension_flag(
                "shared",
                ExtensionFlagType::Boolean,
                serde_json::json!(false),
            ),
        ),
        (
            "second".to_owned(),
            extension_flag("shared", ExtensionFlagType::String, serde_json::json!("")),
        ),
    ])
    .expect_err("cross-extension collision");
    assert!(dynamic_collision.to_string().contains("--shared"));
}

#[test]
fn runtime_subcommand_scan_keeps_dynamic_flags_off_early_exit_paths() {
    let registered = register_extension_flags(vec![
        (
            "fixture".to_owned(),
            extension_flag(
                "fixture-enabled",
                ExtensionFlagType::Boolean,
                serde_json::json!(false),
            ),
        ),
        (
            "fixture".to_owned(),
            extension_flag(
                "fixture-label",
                ExtensionFlagType::String,
                serde_json::json!("default"),
            ),
        ),
        (
            "fixture".to_owned(),
            extension_flag(
                "fixture-count",
                ExtensionFlagType::Integer,
                serde_json::json!(0),
            ),
        ),
    ])
    .expect("register fixture flags");
    let os = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();

    assert!(!invocation_has_top_level_subcommand(
        &os(&["octet", "--fixture-label", "migrate"]),
        &registered,
    ));
    assert!(invocation_has_top_level_subcommand(
        &os(&["octet", "--fixture-enabled", "migrate", "--help"]),
        &registered,
    ));
    assert!(invocation_has_top_level_subcommand(
        &os(&["octet", "--fixture-count", "-7", "migrate"]),
        &registered,
    ));
    assert!(invocation_has_top_level_subcommand(
        &os(&["octet", "--workspace", "/tmp", "migrate", "pi"]),
        &registered,
    ));
    assert!(invocation_has_top_level_subcommand(
        &os(&["octet", "--unknown-extension-flag", "migrate", "--help"]),
        &registered,
    ));
}

#[test]
fn runtime_flag_parser_bypasses_only_true_early_exit_invocations() {
    let os = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(!uses_runtime_extension_flag_parser(&os(&[
        "octet", "--login", "codex"
    ])));
    assert!(!uses_runtime_extension_flag_parser(&os(&[
        "octet",
        "--workspace",
        "/tmp",
        "migrate",
        "pi",
    ])));
    assert!(!uses_runtime_extension_flag_parser(&os(&[
        "octet",
        "--version"
    ])));
    assert!(uses_runtime_extension_flag_parser(&os(&[
        "octet",
        "--extension-flag",
        "migrate",
    ])));
    assert!(!uses_runtime_extension_flag_parser(&os(&[
        "octet",
        "--extension-flag",
        "migrate",
        "--login",
        "codex",
    ])));
    assert!(uses_runtime_extension_flag_parser(&os(&[
        "octet", "--", "--login"
    ])));
    assert!(!uses_runtime_extension_flag_parser(&os(&[
        "octet", "migrate"
    ])));
}

#[test]
fn cache_retention_can_disable_prompt_caching() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.cache_retention = Some("none".into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(config.cache_retention, octet_ai::CacheRetention::None);
}

#[test]
fn colour_policy_resolves_from_cli() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.color = Some("never".into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(config.color, ColorMode::Never);
}

#[test]
fn mouse_policy_owner_decision_defaults_to_auto_preserving_native_gestures() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    // Owner decision: omitted mouse policy preserves native scrollback/selection.
    assert_eq!(config.mouse, config::MouseMode::Auto);
    assert!(!config.mouse.application_owned());

    // An explicit inline mode still resolves normally.
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.mouse = Some("auto".into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(config.mouse, config::MouseMode::Auto);
    assert!(!config.mouse.application_owned());

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.mouse = Some("app".into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(config.mouse, config::MouseMode::App);
    assert!(config.mouse.application_owned());
}

#[test]
fn explicit_mouse_settings_and_cli_override_the_owner_default() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    for saved in ["auto", "app", "terminal", "off"] {
        std::fs::write(&global, format!("mouse = {saved:?}\n")).unwrap();
        let config =
            build_config_with_global_path(base(), directory.path(), Some(&global)).unwrap();
        assert_eq!(config.mouse, config::MouseMode::parse(saved).unwrap());
        for explicit in ["auto", "app"] {
            let mut cli = base();
            cli.mouse = Some(explicit.into());
            let config =
                build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
            assert_eq!(config.mouse, config::MouseMode::parse(explicit).unwrap());
        }
    }
}

#[test]
fn mouse_help_documents_auto_without_setting_a_cli_override() {
    use clap::CommandFactory;
    let help = Cli::command().render_long_help().to_string();
    assert!(help.contains("Mouse ownership (default: auto)"));
    assert!(Cli::try_parse_from(["octet"]).unwrap().mouse.is_none());
}

#[test]
fn shell_path_and_bash_timeout_resolve_from_cli() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.shell_path = Some(PathBuf::from("/opt/homebrew/bin/bash"));
    cli.bash_timeout_secs = Some(45);
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(
        config.sandbox.shell_path,
        Some(PathBuf::from("/opt/homebrew/bin/bash"))
    );
    assert_eq!(config.sandbox.bash_timeout_secs, 45);
}

#[test]
fn policy_value_source_prefers_environment_over_config() {
    assert_eq!(
        policy_value_source(false, false),
        PolicyValueSource::Default
    );
    assert_eq!(policy_value_source(false, true), PolicyValueSource::Config);
    assert_eq!(
        policy_value_source(true, false),
        PolicyValueSource::Environment
    );
    assert_eq!(
        policy_value_source(true, true),
        PolicyValueSource::Environment
    );
}

#[test]
fn effective_policy_provenance_tracks_config_and_cli_precedence() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(
        &global,
        r#"
effect_policy = "controlled"
allow_external_paths = false
allow_edit = false
allow_write = false
allow_process = false
allow_shell = false
allow_remote_read = true
shell_path = "/opt/config/bash"
bash_timeout_secs = 30
max_output_bytes = 4096
"#,
    )
    .unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.allow_shell = true;
    cli.bash_timeout_secs = Some(45);
    cli.max_output_bytes = Some(8192);

    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    let policy = config
        .sandbox
        .effective_tool_policy(&config.workspace, config.effect_policy);

    assert_eq!(
        policy.workspace_confinement.source,
        PolicyValueSource::Config
    );
    assert!(policy.workspace_confinement.value);
    assert_eq!(policy.effect_policy.value, EffectPolicy::Controlled);
    assert_eq!(policy.effect_policy.source, PolicyValueSource::Config);
    assert!(!policy.allow_edit.value);
    assert_eq!(policy.allow_edit.source, PolicyValueSource::Config);
    assert!(!policy.allow_write.value);
    assert_eq!(policy.allow_write.source, PolicyValueSource::Config);
    assert_eq!(policy.allow_process.source, PolicyValueSource::Cli);
    assert!(policy.allow_process.value);
    assert_eq!(policy.allow_shell.source, PolicyValueSource::Cli);
    assert!(policy.allow_shell.value);
    assert_eq!(policy.shell_path.source, PolicyValueSource::Config);
    assert_eq!(
        policy.shell_path.value.selection,
        octet_agent::ShellSelection::Configured
    );
    assert_eq!(policy.bash_timeout_ms.source, PolicyValueSource::Cli);
    assert_eq!(policy.bash_timeout_ms.value, 45_000);
    assert_eq!(policy.max_output_bytes.source, PolicyValueSource::Cli);
    assert_eq!(policy.max_output_bytes.value, 8192);
    assert_eq!(policy.allow_remote_read.source, PolicyValueSource::Config);
    assert!(policy.allow_remote_read.value);
}

#[test]
fn telemetry_path_resolves_relative_to_invocation_directory() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.telemetry = Some(PathBuf::from("metrics/run.jsonl"));
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(
        config.telemetry,
        Some(
            directory
                .path()
                .canonicalize()
                .unwrap()
                .join("metrics/run.jsonl")
        )
    );
}

#[test]
fn remote_reads_are_default_off_and_require_user_level_opt_in() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert!(!config.sandbox.allow_remote_read);

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.allow_remote_read = true;
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert!(config.sandbox.allow_remote_read);

    let global = directory.path().join("global.toml");
    std::fs::write(&global, "allow_remote_read = true\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert!(config.sandbox.allow_remote_read);
}

#[test]
fn experimental_streamable_http_mcp_is_cli_only() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    assert!(
        !config_with_empty_global(cli, directory.path())
            .unwrap()
            .experimental_streamable_http_mcp
    );

    let global = directory.path().join("global.toml");
    std::fs::write(&global, "experimental_streamable_http_mcp = true\n").unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "experimental_streamable_http_mcp = true\n",
    )
    .unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    assert!(
        !build_config_with_global_path(cli, directory.path(), Some(&global))
            .unwrap()
            .experimental_streamable_http_mcp,
        "global and trusted-project configuration cannot grant the experimental transport"
    );

    assert!(
        Cli::try_parse_from(["octet", "--experimental-streamable-http-m"]).is_err(),
        "the process-owner gate must not accept an abbreviated spelling"
    );
    let mut cli = Cli::try_parse_from(["octet", "--experimental-streamable-http-mcp"]).unwrap();
    cli.workspace = Some(directory.path().into());
    assert!(
        config_with_empty_global(cli, directory.path())
            .unwrap()
            .experimental_streamable_http_mcp
    );
}

#[test]
fn project_config_cannot_grant_remote_network_authority_and_offline_revokes_it() {
    let directory = cwd();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "allow_remote_read = true\n",
    )
    .unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert!(!config.sandbox.allow_remote_read);

    let global = directory.path().join("global.toml");
    std::fs::write(&global, "allow_remote_read = true\noffline = true\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert!(config.offline);
    assert!(!config.sandbox.allow_remote_read);
}

#[test]
fn effect_policy_layers_respect_project_tightening_and_cli_override() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "effect_policy = 'controlled'\n").unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    let project = directory.path().join(".octet/config.toml");
    std::fs::write(&project, "effect_policy = 'unsafe_host'\n").unwrap();

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.effect_policy, EffectPolicy::Controlled);
    assert_eq!(
        config
            .sandbox
            .effective_tool_policy(&config.workspace, config.effect_policy)
            .effect_policy
            .source,
        PolicyValueSource::Config
    );

    std::fs::write(&project, "effect_policy = 'controlled_bash_approval'\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(
        config.effect_policy,
        EffectPolicy::ControlledBashApproval,
        "a trusted project may tighten the global profile"
    );

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    cli.effect_policy = Some("unsafe_host".into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.effect_policy, EffectPolicy::UnsafeHost);
    assert_eq!(
        config
            .sandbox
            .effective_tool_policy(&config.workspace, config.effect_policy)
            .effect_policy
            .source,
        PolicyValueSource::Cli
    );
}

#[test]
fn cache_warming_defaults_to_streaming_and_is_global_only() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(
        &global,
        "cache_warming = 'off'\nshow_cache_miss_notices = true\n",
    )
    .unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "cache_warming = 'invalid-project-policy'\nshow_cache_miss_notices = false\n",
    )
    .unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.cache_warming, octet_agent::CacheWarmMode::Off);
    assert!(config.show_cache_miss_notices);
    let config = config_with_empty_global(base(), directory.path()).unwrap();
    assert_eq!(config.cache_warming, octet_agent::CacheWarmMode::Streaming);
    assert!(!config.show_cache_miss_notices);
}

#[test]
fn cache_warming_cli_and_layer_precedence_are_explicit() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "cache_warming = 'off'\n").unwrap();
    let mut cli = base();
    cli.cache_warming = Some("idle".into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.cache_warming, octet_agent::CacheWarmMode::Idle);
    let mut layer = ConfigLayer {
        cache_warming: Some("off".into()),
        ..Default::default()
    };
    layer.merge(ConfigLayer {
        cache_warming: Some("streaming".into()),
        ..Default::default()
    });
    assert_eq!(layer.cache_warming.as_deref(), Some("streaming"));
    for value in ["off", "streaming", "idle"] {
        assert!(Cli::try_parse_from(["octet", "--cache-warming", value]).is_ok());
    }
    assert!(Cli::try_parse_from(["octet", "--cache-warming", "always"]).is_err());
    std::fs::write(&global, "cache_warming = 'always'\n").unwrap();
    assert!(
        build_config_with_global_path(base(), directory.path(), Some(&global))
            .unwrap_err()
            .to_string()
            .contains("cache warming")
    );
}

#[test]
fn cache_warming_keys_are_known_and_persistence_preserves_other_user_settings() {
    let directory = cwd();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "# user choices\nmodel = 'keep-model'\nshow_cache_miss_notices = true\n[compaction]\nmode = 'local'\n").unwrap();
    persist_key_to_path("cache_warming", "idle", &path).unwrap();
    let loaded = read_layer(&path, ConfigSourceKind::Global).unwrap();
    assert!(loaded.diagnostics.is_empty());
    assert_eq!(loaded.values.cache_warming.as_deref(), Some("idle"));
    assert_eq!(loaded.values.show_cache_miss_notices, Some(true));
    assert_eq!(loaded.values.model.as_deref(), Some("keep-model"));
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("# user choices"));
    persist_key_to_path("cache_warming", "off", &path).unwrap();
    assert_eq!(
        read_layer(&path, ConfigSourceKind::Global)
            .unwrap()
            .values
            .cache_warming
            .as_deref(),
        Some("off")
    );
}

#[test]
fn environment_effect_policy_layer_overrides_config() {
    let mut values = ConfigLayer {
        effect_policy: Some("controlled".into()),
        ..ConfigLayer::default()
    };
    values.merge(ConfigLayer {
        effect_policy: Some("unsafe_host".into()),
        ..ConfigLayer::default()
    });

    assert_eq!(values.effect_policy.as_deref(), Some("unsafe_host"));
    assert_eq!(
        policy_value_source(true, values.effect_policy.is_some()),
        PolicyValueSource::Environment
    );
}

#[test]
fn effect_policy_is_full_access_by_default_and_yolo_is_removed() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(config.effect_policy, octet_agent::EffectPolicy::UnsafeHost);
    assert!(config.sandbox.allow_external_paths);
    assert!(config.enabled_extensions.is_empty());
    assert!(config.trusted_extensions.is_empty());
    assert!(config.invocation_trusted_extensions.is_empty());
    assert!(Cli::try_parse_from(["octet", "--yolo"]).is_err());
}

#[test]
fn safe_mode_uses_the_controlled_approval_profile() {
    let directory = cwd();

    let mut cli = Cli::try_parse_from(["octet", "--safe-mode"]).unwrap();
    assert!(cli.safe_mode);
    cli.workspace = Some(directory.path().into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(
        config.effect_policy,
        octet_agent::EffectPolicy::ControlledBashApproval
    );

    let cli = Cli::try_parse_from(["octet", "--safe"]).unwrap();
    assert!(cli.safe_mode);
    assert!(
        Cli::try_parse_from(["octet", "--safe-mode", "--effect-policy", "unsafe_host",]).is_err()
    );
}

#[tokio::test]
async fn safe_mode_extension_selection_preserves_every_bash_approval() {
    let directory = cwd();
    for explicit_trust in [false, true] {
        let mut cli = base();
        cli.safe_mode = true;
        cli.enable_extensions.push("fixture".into());
        if explicit_trust {
            cli.trust_extensions.push("fixture".into());
        }
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.effect_policy, EffectPolicy::ControlledBashApproval);
        assert!(config.sandbox.process_execution_allowed());
        assert!(!config.sandbox.allow_external_paths);
        let broker = octet_agent::EffectBroker::new(config.effect_policy);
        for command in ["ls", "printf changed > file.txt"] {
            let intent = octet_agent::EffectIntent::new(
                "principal",
                "run",
                1,
                "call",
                "bash",
                octet_agent::ToolEffect::HostProcess,
                serde_json::json!({"command": command}),
            )
            .unwrap();
            assert!(matches!(
                broker.authorize(&intent, None).await,
                Err(octet_agent::EffectBrokerError::ApprovalUnavailable { .. })
            ));
        }
        let extension_intent = octet_agent::EffectIntent::new(
            "principal",
            "run",
            1,
            "extension-call",
            "fixture",
            octet_agent::ToolEffect::Extension,
            serde_json::json!({}),
        )
        .unwrap();
        assert!(matches!(
            broker.authorize(&extension_intent, None).await,
            Err(octet_agent::EffectBrokerError::Denied { .. })
        ));
    }
}

#[test]
fn invalid_effect_policy_is_secret_safe() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.effect_policy = Some("sensitive-invalid-policy-value".into());

    let error = config_with_empty_global(cli, directory.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid effect policy"));
    assert!(!error.contains("sensitive-invalid-policy-value"));
}

#[test]
fn safe_mode_forces_workspace_only_paths() {
    let directory = cwd();
    assert!(SandboxPolicy::default().allow_external_paths);

    let global = directory.path().join("global.toml");
    std::fs::write(&global, "allow_external_paths = true\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.effect_policy, octet_agent::EffectPolicy::UnsafeHost);
    assert!(config.sandbox.allow_external_paths);

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.safe_mode = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(
        config.effect_policy,
        octet_agent::EffectPolicy::ControlledBashApproval
    );
    assert!(!config.sandbox.allow_external_paths);
}

#[test]
fn legacy_host_authority_config_does_not_select_a_policy() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "unsafe_host_effects = false\n").unwrap();

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.effect_policy, octet_agent::EffectPolicy::UnsafeHost);
}

#[test]
fn print_mode_requires_prompt_text() {
    let directory = cwd();
    let mut cli = base();
    cli.print = true;
    cli.model = Some("m".into());
    cli.workspace = Some(directory.path().into());
    assert!(config_with_empty_global(cli, directory.path()).is_err());
}

#[test]
fn print_mode_builds_print_config() {
    let directory = cwd();
    let mut cli = base();
    cli.message = Some("hi".into());
    cli.print = true;
    cli.model = Some("m".into());
    cli.workspace = Some(directory.path().into());
    cli.show_reasoning = true;
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert!(matches!(config.mode, Mode::Print { prompt } if prompt == "hi"));
    assert!(config.show_reasoning_in_print);
}

#[test]
fn continue_sets_resume_selector_and_interactive_mode() {
    let directory = cwd();
    let mut cli = base();
    cli.continue_ = true;
    cli.workspace = Some(directory.path().into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert!(matches!(config.resume, ResumeSelector::Continue));
    assert!(matches!(config.mode, Mode::Interactive));
}

#[test]
fn clap_parses_fork_and_rejects_resume_conflicts() {
    let parsed = Cli::try_parse_from(["octet", "--fork", "source-id"]).unwrap();
    assert_eq!(parsed.fork, Some(Some("source-id".into())));
    assert!(Cli::try_parse_from(["octet", "--fork", "--resume"]).is_err());
    assert!(Cli::try_parse_from(["octet", "--fork", "--continue"]).is_err());
}

#[test]
fn fork_without_an_id_is_distinct_from_fork_by_id() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.fork = Some(None);
    assert!(matches!(
        config_with_empty_global(cli, directory.path())
            .unwrap()
            .resume,
        ResumeSelector::Fork(None)
    ));

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.fork = Some(Some("session-id".into()));
    assert!(matches!(
        config_with_empty_global(cli, directory.path())
            .unwrap()
            .resume,
        ResumeSelector::Fork(Some(id)) if id == "session-id"
    ));
}

#[test]
fn unset_reasoning_is_distinct_from_explicit_off() {
    let directory = cwd();
    let config = config_with_empty_global(base(), directory.path()).unwrap();
    assert_eq!(config.reasoning, None);
    assert!(!config.reasoning_explicit);

    let mut cli = base();
    cli.reasoning = Some("off".into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(config.reasoning, Some(octet_ai::ReasoningConfig::Off));
    assert!(config.reasoning_explicit);
}

#[test]
fn reasoning_is_parsed_and_invalid_values_fail() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.reasoning = Some("off".into());
    assert!(config_with_empty_global(cli, directory.path()).is_ok());

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.reasoning = Some("budget=2048".into());
    assert!(config_with_empty_global(cli, directory.path()).is_ok());

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.reasoning_mode = Some("pro".into());
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(config.reasoning_mode, octet_ai::ReasoningMode::Pro);
    assert!(config.reasoning_mode_explicit);

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.reasoning = Some("nonsense".into());
    assert!(config_with_empty_global(cli, directory.path()).is_err());

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.reasoning_mode = Some("turbo".into());
    assert!(config_with_empty_global(cli, directory.path()).is_err());
}

#[test]
fn resume_without_an_id_is_distinct_from_resume_by_id() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.resume = Some(None);
    assert!(matches!(
        config_with_empty_global(cli, directory.path())
            .unwrap()
            .resume,
        ResumeSelector::Resume(None)
    ));
}

#[test]
fn strict_config_flag_is_parsed() {
    let cli = Cli::try_parse_from(["octet", "--strict-config"]).unwrap();
    assert!(cli.strict_config);
}

#[test]
fn cli_overrides_project_which_overrides_global() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(
        &global,
        "model = 'global'\ntheme = 'global-theme'\nmax_turns = 7\n",
    )
    .unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "model = 'project'\ntheme = 'project-theme'\nmax_turns = 9\nallow_external_paths = false\n",
    )
    .unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    cli.model = Some("cli".into());
    cli.max_turns = Some(11);
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.model.as_ref().unwrap().0, "cli");
    assert!(config.model_explicit);
    assert!(!config.reasoning_explicit);
    assert_eq!(config.theme.as_deref(), Some("project-theme"));
    assert!(!config.theme_explicit);
    assert_eq!(config.max_turns, Some(11));
    assert!(!config.sandbox.allow_external_paths);
}

#[test]
fn cli_theme_choice_is_explicit_without_rewriting_saved_appearance() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "theme = 'dark'\n").unwrap();
    let cli = Cli::try_parse_from(["octet", "--theme", "Still"]).unwrap();
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.theme.as_deref(), Some("Still"));
    assert!(config.theme_explicit);
    assert_eq!(std::fs::read_to_string(global).unwrap(), "theme = 'dark'\n");
}

#[test]
fn telemetry_uses_cli_then_project_then_global_precedence() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "telemetry = 'global.jsonl'\n").unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "telemetry = 'project.jsonl'\n",
    )
    .unwrap();

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    cli.telemetry = Some("cli.jsonl".into());
    let canonical = directory.path().canonicalize().unwrap();
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.telemetry, Some(canonical.join("cli.jsonl")));

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.telemetry, Some(canonical.join("project.jsonl")));

    std::fs::remove_file(directory.path().join(".octet/config.toml")).unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.telemetry, Some(canonical.join("global.jsonl")));
}

#[test]
fn system_prompt_layered_precedence_prefers_cli_over_project_then_global() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "system_prompt = 'global'\n").unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "system_prompt = 'project'\n",
    )
    .unwrap();

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    cli.system_prompt = Some("cli".into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.system_prompt.as_deref(), Some("cli"));

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.system_prompt.as_deref(), Some("project"));
}

#[test]
fn system_prompt_explicit_empty_cli_value_is_preserved() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "system_prompt = 'global'\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.system_prompt = Some("".into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.system_prompt.as_deref(), Some(""));
}

#[test]
fn parse_system_prompt_flag_without_value() {
    let cli =
        Cli::try_parse_from(["octet", "--system-prompt", "--print", "--prompt", "review"]).unwrap();
    assert!(cli.system_prompt.is_some());
    assert_eq!(cli.system_prompt.as_deref(), Some(""));
}

#[test]
fn trusted_project_may_tighten_but_never_relax_global_authority() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "allow_write = false\nallow_edit = true\n").unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "allow_write = true\nallow_edit = false\n",
    )
    .unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert!(!config.sandbox.allow_write);
    assert!(!config.sandbox.allow_edit);
}

#[test]
fn trusted_project_may_enable_but_cannot_trust_an_executable_extension() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(
        &global,
        "enabled_extensions = ['user-tool']\ntrusted_extensions = ['user-tool']\n",
    )
    .unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "enabled_extensions = ['project-tool']\ntrusted_extensions = ['project-tool']\n",
    )
    .unwrap();

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();

    assert_eq!(config.enabled_extensions, vec!["project-tool"]);
    assert!(config.extension_activation_overridden);
    assert_eq!(config.trusted_extensions, vec!["user-tool"]);
    assert!(config.invocation_trusted_extensions.is_empty());
}

#[test]
fn activation_menu_revalidates_a_project_override_added_after_startup() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "enabled_extensions = ['global-tool']\n").unwrap();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert!(!config.extension_activation_overridden);
    assert!(extension_activation_menu_authoritative(&config).unwrap());

    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "enabled_extensions = ['project-tool']\n",
    )
    .unwrap();
    assert!(!extension_activation_menu_authoritative(&config).unwrap());

    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "not valid = [\n",
    )
    .unwrap();
    assert!(extension_activation_menu_authoritative(&config).is_err());
}

#[test]
fn unavailable_home_never_loads_project_config_as_global_config() {
    let directory = cwd();
    std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
    std::fs::write(
        directory.path().join(".octet/config.toml"),
        "enabled_extensions = ['project-tool']\ntrusted_extensions = ['project-tool']\n",
    )
    .unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());

    let config = build_config_with_global_path(cli, directory.path(), None).unwrap();

    assert!(config.enabled_extensions.is_empty());
    assert!(!config.extension_activation_overridden);
    assert!(config.trusted_extensions.is_empty());
    assert!(config.invocation_trusted_extensions.is_empty());
}

#[test]
fn relative_home_is_not_a_global_config_root() {
    assert_eq!(global_config_path_from_home(None), None);
    assert_eq!(global_config_path_from_home(Some(".".into())), None);
    let absolute_home = std::env::temp_dir().join("octet-home");
    assert_eq!(
        global_config_path_from_home(Some(absolute_home.clone())),
        Some(absolute_home.join(".octet/config.toml"))
    );
}

#[test]
fn cli_activation_marks_the_interactive_user_config_menu_non_authoritative() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "enabled_extensions = ['global-tool']\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.enable_extensions.push("cli-tool".into());

    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();

    assert_eq!(config.enabled_extensions, ["cli-tool", "global-tool"]);
    assert!(config.extension_activation_overridden);
}

#[test]
fn extension_name_lists_are_normalized_and_deduplicated() {
    let names =
        normalize_extension_names(split_names("Git-Tools, local-model,git-tools".to_owned()))
            .unwrap();
    assert_eq!(names, vec!["git-tools", "local-model"]);
}

#[test]
fn persistent_extension_trust_grants_preserve_exact_source_paths() {
    // Absolute-path detection is platform-relative: Unix spellings are
    // not absolute on Windows, so each platform exercises its own form.
    #[cfg(windows)]
    let grants = normalize_extension_trust_grants([
        "Git-Tools".to_owned(),
        "git-tools@C:/workspace/.octet/extensions/git-tools/extension.toml".to_owned(),
        "git-tools@C:/dev@home/git-tools/extension.toml".to_owned(),
        " Git-Tools ".to_owned(),
    ])
    .unwrap();
    #[cfg(not(windows))]
    let grants = normalize_extension_trust_grants([
        "Git-Tools".to_owned(),
        "git-tools@/workspace/.octet/extensions/git-tools/extension.toml".to_owned(),
        "git-tools@/Volumes/dev@home/git-tools/extension.toml".to_owned(),
        " Git-Tools ".to_owned(),
    ])
    .unwrap();
    #[cfg(windows)]
    assert_eq!(
        grants,
        vec![
            "git-tools",
            "git-tools@C:/dev@home/git-tools/extension.toml",
            "git-tools@C:/workspace/.octet/extensions/git-tools/extension.toml",
        ]
    );
    #[cfg(not(windows))]
    assert_eq!(
        grants,
        vec![
            "git-tools",
            "git-tools@/Volumes/dev@home/git-tools/extension.toml",
            "git-tools@/workspace/.octet/extensions/git-tools/extension.toml",
        ]
    );
}

#[test]
fn persistent_source_trust_rejects_relative_paths() {
    let error = normalize_extension_trust_grants([
        "git-tools@.octet/extensions/git-tools/extension.toml".to_owned(),
    ])
    .unwrap_err();
    assert!(error.to_string().contains("absolute path"));
}

#[test]
fn cli_extension_trust_is_kept_one_shot() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, "trusted_extensions = ['global-tool']\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.trust_extensions = vec!["Project-Tool".into()];

    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();

    assert_eq!(config.trusted_extensions, vec!["global-tool"]);
    assert_eq!(config.invocation_trusted_extensions, vec!["project-tool"]);
}

#[test]
fn no_edit_and_explicit_allowlists_match_the_provider_tool_surface() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.no_edit = true;
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert!(!config.sandbox.allow_edit);
    assert!(!config.sandbox.allow_write);
    assert!(!config.tools.enabled("edit"));
    assert!(!config.tools.enabled("write"));

    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.tools = Some(vec!["read".into(), "search".into()]);
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    assert_eq!(
        config.tools.names().collect::<Vec<_>>(),
        vec!["read", "search"]
    );
}

#[test]
fn cost_and_compaction_settings_merge_from_layered_toml() {
    let global: ConfigLayer = toml::from_str(
        "max_cost_microdollars = 100\ncost_warning_microdollars = 25\n[compaction]\nenabled = false\nmax_active_tokens = 272000\ncompact_model = 'cheap'",
    )
    .unwrap();
    let project: ConfigLayer = toml::from_str(
        "cost_warning_microdollars = 40\n[compaction]\nmax_active_tokens = 200000\nkeep_recent_tokens = 2",
    )
    .unwrap();
    let mut merged = global;
    merged.merge(project);
    assert_eq!(merged.max_cost_microdollars, Some(100));
    assert_eq!(merged.cost_warning_microdollars, Some(40));
    let compaction = merged.compaction.unwrap();
    assert_eq!(compaction.enabled, Some(false));
    assert_eq!(compaction.compact_model.as_deref(), Some("cheap"));
    assert_eq!(compaction.max_active_tokens, Some(200_000));
    assert_eq!(compaction.keep_recent_tokens, Some(2));
}

#[test]
fn explicit_compaction_mode_and_legacy_enabled_map_without_silent_fallback() {
    let directory = cwd();
    let global = directory.path().join("global.toml");
    std::fs::write(
        &global,
        "[compaction]\nmode = 'native-responses'\nmax_active_tokens = 0\n",
    )
    .unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.compaction.mode, CompactionMode::NativeResponses);
    assert_eq!(config.compaction.max_active_tokens, Some(0));

    std::fs::write(&global, "[compaction]\nenabled = true\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.compaction.mode, CompactionMode::Local);

    std::fs::write(&global, "[compaction]\nenabled = false\n").unwrap();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
    assert_eq!(config.compaction.mode, CompactionMode::Disabled);
}

// --- extension activation persistence ---

#[test]
fn persist_extension_activation_changes_only_the_selected_user_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "# keep this comment\nenabled_extensions = [\"octet-computer-use\", \"octet-ssh\"]\ntrusted_extensions = [\"octet-computer-use\", \"octet-ssh\"]\n",
    )
    .unwrap();

    assert_eq!(
        persist_extension_enabled_to_path("octet-web-search", true, &path).unwrap(),
        vec!["octet-computer-use", "octet-ssh", "octet-web-search"]
    );
    assert_eq!(
        persist_extension_enabled_to_path("octet-computer-use", false, &path).unwrap(),
        vec!["octet-ssh", "octet-web-search"]
    );

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = toml::from_str(&content).unwrap();
    let enabled = parsed["enabled_extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(enabled, ["octet-ssh", "octet-web-search"]);
    assert_eq!(
        parsed["trusted_extensions"].as_array().unwrap().len(),
        2,
        "trust is an independent decision"
    );
    assert!(content.contains("# keep this comment"), "{content}");
}

#[test]
fn host_authority_grants_migrate_and_revoke_without_changing_activation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let project = dir.path().join("project/.octet/extensions/fixture");
    std::fs::create_dir_all(&project).unwrap();
    let exact = format!("fixture@{}", project.join("extension.toml").display());
    std::fs::write(
        &path,
        "# retained\nenabled_extensions = ['fixture']\ntrusted_extensions = ['fixture']\n",
    )
    .unwrap();
    assert_eq!(
        persist_extension_host_authority_to_path(&exact, true, &path).unwrap(),
        vec!["fixture", exact.as_str()]
    );
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("enabled_extensions = ['fixture']"));
    assert!(content.contains("# retained"));
    assert_eq!(
        persist_extension_host_authority_to_path("fixture", false, &path).unwrap(),
        vec![exact.as_str()]
    );
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("enabled_extensions = ['fixture']"));
    let mut cli = base();
    cli.workspace = Some(dir.path().into());
    cli.safe_mode = true;
    let config = build_config_with_global_path(cli, dir.path(), Some(&path)).unwrap();
    assert_eq!(config.enabled_extensions, vec!["fixture"]);
    assert_eq!(config.trusted_extensions, vec![exact]);
    assert_eq!(
        config.effect_policy,
        octet_agent::EffectPolicy::ControlledBashApproval
    );
}

#[test]
fn revoking_source_authority_removes_all_normalized_aliases_and_startup_authority() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let path = root.join("config.toml");
    let selected = root.join(".octet/extensions/fixture");
    let unrelated_workspace = root.join("other-workspace");
    let unrelated = unrelated_workspace.join(".octet/extensions/fixture");
    for source in [&selected, &unrelated] {
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(
            source.join("extension.toml"),
            r#"
name = "fixture"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "must-not-run"
[contributes]
flags = [{name = "fixture-authorized", type = "boolean", default = false}]
"#,
        )
        .unwrap();
    }
    let exact = format!("fixture@{}", selected.join("extension.toml").display());
    let alias = format!(
        "fixture@{}",
        selected.join("nested/../extension.toml").display()
    );
    let other = format!("fixture@{}", unrelated.join("extension.toml").display());
    std::fs::write(&path, format!("enabled_extensions = ['fixture']\ntrusted_extensions = [{exact:?}, {alias:?}, {other:?}]\n")).unwrap();
    let mut cli = base();
    cli.workspace = Some(root.clone());
    cli.safe_mode = true;
    let mut config = build_config_with_global_path(cli, &root, Some(&path)).unwrap();
    config.workspace_trusted = true;
    config.extension_paths.clear();
    assert_eq!(
        crate::extensions::selected_extension_flag_declarations(&config).len(),
        1
    );
    let grants = persist_extension_host_authority_to_path(&exact, false, &path).unwrap();
    assert_eq!(grants, vec![other.clone()]);
    // Reload the same menu result and invoke real startup declaration policy:
    // the source is still enabled, but neither alias can authorize its code.
    config.trusted_extensions = grants;
    assert!(crate::extensions::selected_extension_flag_declarations(&config).is_empty());
    config.workspace = unrelated_workspace;
    assert_eq!(
        crate::extensions::selected_extension_flag_declarations(&config).len(),
        1
    );

    // Global name grants and exact global aliases are equivalent sources too.
    let global = root.join("extensions/fixture");
    std::fs::create_dir_all(&global).unwrap();
    let global_exact = format!("fixture@{}", global.join("extension.toml").display());
    std::fs::write(
        &path,
        format!("trusted_extensions = ['fixture', {global_exact:?}, {other:?}]\n"),
    )
    .unwrap();
    assert_eq!(
        persist_extension_host_authority_to_path("fixture", false, &path).unwrap(),
        vec![other]
    );
}

#[cfg(unix)]
#[test]
fn atomic_config_update_preserves_existing_permissions_and_uses_private_new_files() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.toml");
    std::fs::write(&existing, "enabled_extensions = []\n").unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o640)).unwrap();
    persist_extension_enabled_to_path("octet-ssh", true, &existing).unwrap();
    assert_eq!(
        std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
        0o640
    );

    let new = dir.path().join("new.toml");
    persist_extension_enabled_to_path("octet-ssh", true, &new).unwrap();
    assert_eq!(
        std::fs::metadata(&new).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let staging_files = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
        .count();
    assert_eq!(staging_files, 0, "atomic staging files must be removed");
}

#[test]
fn atomic_config_publish_rejects_a_non_locking_external_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = "enabled_extensions = [\"octet-computer-use\"]\n";
    let external = "enabled_extensions = [\"octet-ssh\"]\ntrusted_extensions = [\"octet-ssh\"]\n";
    std::fs::write(&path, original).unwrap();
    let expected = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, external).unwrap();

    let error =
        write_config_atomically(&path, "enabled_extensions = []\n", Some(&expected)).unwrap_err();

    assert!(error.to_string().contains("changed"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), external);
    assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        !name.contains(".tmp-") && !name.contains(".octet-tmp-")
    }));
}

#[test]
fn concurrent_config_update_fails_without_rewriting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = "enabled_extensions = [\"octet-computer-use\"]\n";
    std::fs::write(&path, original).unwrap();
    let lock = config_update_lock(&path).unwrap();

    let error = persist_extension_enabled_to_path("octet-ssh", true, &path).unwrap_err();
    assert!(
        error.to_string().contains("another config update"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    drop(lock);

    assert_eq!(
        persist_extension_enabled_to_path("octet-ssh", true, &path).unwrap(),
        ["octet-computer-use", "octet-ssh"]
    );
}

#[test]
fn persist_extension_activation_rejects_a_non_array_without_rewriting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let invalid = "enabled_extensions = \"octet-computer-use\"\n";
    std::fs::write(&path, invalid).unwrap();

    assert!(persist_extension_enabled_to_path("octet-ssh", true, &path).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), invalid);
}

// --- persist_bool_key_to_path / remove_key_from_path ---

#[test]
fn bool_settings_persist_as_toml_booleans_and_removable_keys_disappear() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    persist_bool_key_to_path("show_images", true, &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let document = content.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        document["show_images"].as_bool(),
        Some(true),
        "boolean settings must not round-trip as strings: {content}"
    );
    persist_bool_key_to_path("show_images", false, &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let document = content.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(document["show_images"].as_bool(), Some(false));

    // A string-valued key round-trips, then removal restores the rest of
    // the document untouched.
    persist_key_to_path("models", "a:high,b", &path).unwrap();
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("models = \"a:high,b\""));
    remove_key_from_path("models", &path).unwrap();
    let remaining = std::fs::read_to_string(&path).unwrap();
    assert!(!remaining.contains("models"), "{remaining}");
    assert!(remaining.contains("show_images = false"), "{remaining}");
    // Removing a key from a missing file is already the desired state.
    remove_key_from_path("models", &dir.path().join("absent.toml")).unwrap();
}

// --- persist_model_to_path ---

fn read_model_from_config(path: &std::path::Path) -> Option<String> {
    let source = std::fs::read_to_string(path).unwrap();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some(after) = trimmed.strip_prefix("model") {
            let after = after.trim_start();
            if let Some(val) = after.strip_prefix('=') {
                return Some(val.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

#[test]
fn persist_model_creates_file_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    persist_model_to_path("gpt-4o-mini", &path).unwrap();
    assert_eq!(
        read_model_from_config(&path).as_deref(),
        Some("gpt-4o-mini")
    );
}

#[test]
fn persist_model_updates_existing_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "model = \"old-model\"\ntheme = \"dusk\"\n").unwrap();
    persist_model_to_path("new-model", &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("model = \"new-model\""), "{content}");
    assert!(
        content.contains("theme = \"dusk\""),
        "theme line preserved: {content}"
    );
}

#[test]
fn persist_model_appends_when_no_model_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "theme = \"dusk\"\n").unwrap();
    persist_model_to_path("gpt-4o-mini", &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("model = \"gpt-4o-mini\""), "{content}");
}

#[test]
fn persist_theme_choice_preserves_unrelated_user_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "# keep this comment\nmodel = \"gpt-4o-mini\"\ntheme = \"auto\"\n[compaction]\nkeep_recent_tokens = 8\n",
    )
    .unwrap();

    for choice in ["light", "dark", "auto", "MyTheme"] {
        persist_theme_to_path(theme_choice_key(choice).unwrap(), &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        assert_eq!(parsed["theme"].as_str(), Some(choice));
        assert_eq!(parsed["model"].as_str(), Some("gpt-4o-mini"));
        assert_eq!(
            parsed["compaction"]["keep_recent_tokens"].as_integer(),
            Some(8)
        );
        assert!(content.contains("# keep this comment"), "{content}");
    }
}

#[test]
fn theme_choice_persistence_rejects_unsafe_and_reserved_file_names() {
    assert_eq!(theme_choice_key(" DARK ").unwrap(), "dark");
    assert_eq!(theme_choice_key("MyTheme").unwrap(), "MyTheme");
    for name in ["../other", "a/b", "default", "foo.toml", "", "bad name"] {
        assert!(theme_choice_key(name).is_err(), "accepted {name:?}");
    }
}

#[test]
fn theme_choice_persistence_accepts_the_compiled_in_file_themes() {
    // The picker offers `Cards` and `Still` through the same file variant
    // as a discovered theme, and their stems are reserved so a local file
    // cannot shadow them. Selecting the built-in must still save, otherwise
    // the picker reports `invalid theme selector` for a theme it just showed.
    for (choice, expected) in [("Cards", "Cards"), ("Still", "Still")] {
        assert_eq!(theme_choice_key(choice).unwrap(), expected);
        // Case and the `.toml` spelling resolve to the one built-in name.
        assert_eq!(theme_choice_key(&choice.to_lowercase()).unwrap(), expected);
        assert_eq!(
            theme_choice_key(&format!("{choice}.toml")).unwrap(),
            expected
        );
    }
    // The compiled default is still not a selectable file name: it is the
    // fallback, not a theme the picker offers.
    assert!(theme_choice_key("default").is_err());
}

#[test]
fn theme_onboarding_only_applies_to_fresh_interactive_installs() {
    let directory = cwd();
    let mut cli = base();
    cli.workspace = Some(directory.path().into());
    cli.workspace_trusted = true;
    let config = config_with_empty_global(cli, directory.path()).unwrap();
    let missing_global = directory.path().join("missing-user-config.toml");
    assert!(should_offer_theme_onboarding_at(
        &config,
        Some(&missing_global)
    ));

    std::fs::write(&missing_global, "theme = \"auto\"\n").unwrap();
    assert!(!should_offer_theme_onboarding_at(
        &config,
        Some(&missing_global)
    ));

    let mut configured = config.clone();
    configured.theme = Some("legacy-theme".into());
    assert!(!should_offer_theme_onboarding_at(
        &configured,
        Some(&directory.path().join("still-missing.toml"))
    ));

    let mut plain = config;
    plain.plain = true;
    assert!(!should_offer_theme_onboarding_at(
        &plain,
        Some(&directory.path().join("another-missing.toml"))
    ));

    let mut print = plain.clone();
    print.plain = false;
    print.mode = Mode::Print {
        prompt: "hello".into(),
    };
    assert!(!should_offer_theme_onboarding_at(
        &print,
        Some(&directory.path().join("print-missing.toml"))
    ));

    let mut rpc = print;
    rpc.mode = Mode::Rpc;
    assert!(!should_offer_theme_onboarding_at(
        &rpc,
        Some(&directory.path().join("rpc-missing.toml"))
    ));
}

#[test]
fn recognized_terminal_appearance_environment_counts_as_configured() {
    for value in ["auto", "dark", "light", "unknown", "universal"] {
        assert!(terminal_appearance_environment_is_configured_value(Some(
            value
        )));
    }
    assert!(!terminal_appearance_environment_is_configured_value(Some(
        "neon"
    )));
    assert!(!terminal_appearance_environment_is_configured_value(None));
}

#[test]
fn persist_model_skips_commented_model_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "# model = \"commented-out\"\ntheme = \"dusk\"\n").unwrap();
    persist_model_to_path("active-model", &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    // The TOML-based parser does not preserve comments since they are
    // not part of the parsed representation. The commented line is
    // intentionally dropped in exchange for structurally correct updates
    // that never corrupt multi-line values or cause partial-key collisions.
    assert!(
        content.contains("model = \"active-model\""),
        "new entry set: {content}"
    );
    assert!(
        content.contains("theme = \"dusk\""),
        "existing key preserved: {content}"
    );
}

#[test]
fn persist_model_preserves_multiline_values_and_partial_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "model_alias = \"keep\"\nnotes = [\n  \"first\",\n  \"second\",\n]\n[compaction]\nkeep_recent_tokens = 4\n",
    )
    .unwrap();

    persist_model_to_path("active-model", &path).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = toml::from_str(&content).unwrap();
    assert_eq!(parsed["model"].as_str(), Some("active-model"));
    assert_eq!(parsed["model_alias"].as_str(), Some("keep"));
    assert_eq!(parsed["notes"].as_array().unwrap().len(), 2);
    assert_eq!(
        parsed["compaction"]["keep_recent_tokens"].as_integer(),
        Some(4)
    );
}

#[test]
fn persist_model_rejects_invalid_toml_without_rewriting_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let invalid = "model = [\n";
    std::fs::write(&path, invalid).unwrap();

    assert!(persist_model_to_path("active-model", &path).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), invalid);
}

#[test]
fn persist_model_escapes_special_characters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // Backslash and double-quote must be escaped in TOML basic strings.
    persist_model_to_path("model\\with\"quotes", &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("model = "), "{content}");
    // Round-trip: the written TOML must parse back to the original id.
    let parsed: std::collections::BTreeMap<String, toml::Value> = toml::from_str(&content).unwrap();
    assert_eq!(
        parsed.get("model").unwrap().as_str().unwrap(),
        "model\\with\"quotes"
    );
}

#[test]
fn doctor_command_parses_without_a_prompt() {
    let cli = Cli::try_parse_from(["octet", "--offline", "doctor"]).unwrap();
    assert!(cli.message.is_none());
    assert!(matches!(cli.command, Some(TopLevelCommand::Doctor)));
    assert!(cli.offline);
}

#[test]
fn setup_command_parses_explicit_non_interactive_inputs() {
    let cli = Cli::try_parse_from([
        "octet",
        "setup",
        "--endpoint",
        "https://models.example.test/v1/",
        "--api-key-env",
        "EXAMPLE_API_KEY",
        "--manual-model",
        "example-model",
        "--yes",
    ])
    .unwrap();
    assert!(cli.message.is_none());
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Setup { options })
            if options.preset.is_none()
                && options.endpoint.as_deref() == Some("https://models.example.test/v1/")
                && options.api_key_env.as_deref() == Some("EXAMPLE_API_KEY")
                && options.manual_model.as_deref() == Some("example-model")
                && options.yes
    ));

    let cli = Cli::try_parse_from([
        "octet",
        "setup",
        "--preset",
        "lm-studio",
        "--offline",
        "--manual-model",
        "local-model",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Setup { options })
            if options.preset == Some(SetupPreset::LmStudio)
                && options.offline
                && options.manual_model.as_deref() == Some("local-model")
    ));
}

#[test]
fn sessions_subcommands_do_not_consume_the_positional_prompt() {
    let cli = Cli::try_parse_from(["octet", "sessions", "inspect", "abc-123"]).unwrap();
    assert!(cli.message.is_none());
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Sessions {
            command: SessionCommand::Inspect { ref id }
        }) if id == "abc-123"
    ));
}

#[test]
fn extension_package_commands_parse_without_a_prompt() {
    let cli = Cli::try_parse_from(["octet", "extension", "install", "octet-subagents"]).unwrap();
    assert!(cli.message.is_none());
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Extension {
            command: ExtensionCommand::Install {
                name: Some(ref name),
                path: None,
            }
        }) if name == "octet-subagents"
    ));

    let cli = Cli::try_parse_from(["octet", "extension", "install", "--path", "./bundle.tar.gz"])
        .unwrap();
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Extension {
            command: ExtensionCommand::Install {
                name: None,
                path: Some(_),
            }
        })
    ));

    let cli = Cli::try_parse_from(["octet", "extension", "update", "octet-web-search"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Extension {
            command: ExtensionCommand::Update {
                name: Some(ref name),
                path: None,
            }
        }) if name == "octet-web-search"
    ));
    let cli =
        Cli::try_parse_from(["octet", "extension", "update", "--path", "./bundle.tar.gz"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Extension {
            command: ExtensionCommand::Update {
                name: None,
                path: Some(_),
            }
        })
    ));
}

#[test]
fn pi_migration_dry_run_parses_without_a_prompt() {
    let cli = Cli::try_parse_from([
        "octet",
        "migrate",
        "pi",
        "--dry-run",
        "--json",
        "--project",
        "./workspace",
    ])
    .unwrap();
    assert!(cli.message.is_none());
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Migrate {
            command: MigrationCommand::Pi {
                dry_run: true,
                json: true,
                project: Some(_),
                ..
            }
        })
    ));
}

#[test]
fn pi_compatibility_command_is_not_exposed_and_bridge_options_are_rejected() {
    let mut command = Cli::command();
    assert!(command.find_subcommand("pi").is_none());
    assert!(command.find_subcommand("migrate").is_some());
    let help = command.render_long_help().to_string();
    assert!(!help
        .lines()
        .any(|line| line.split_whitespace().next() == Some("pi")));
    for arguments in [
        vec![
            "octet",
            "pi",
            "install",
            "./extension.ts",
            "--pi-package",
            "./pi",
        ],
        vec!["octet", "pi", "list", "--extension-root", "./extensions"],
        vec!["octet", "pi", "publish", "--plan", "./plan.json"],
    ] {
        let error = Cli::try_parse_from(arguments).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }
}

#[test]
fn pi_is_an_ordinary_prompt_not_a_compatibility_command() {
    // Removing a subcommand does not reserve or reject ordinary prompt text.
    for arguments in [vec!["octet", "pi"], vec!["octet", "pi", "list"]] {
        let cli = Cli::try_parse_from(arguments).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.message.as_deref(), Some("pi"));
    }
}

#[test]
fn pi_import_remains_available_without_the_compatibility_bridge() {
    let cli = Cli::try_parse_from(["octet", "migrate", "import", "pi", "--dry-run"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(TopLevelCommand::Migrate {
            command: MigrationCommand::Import {
                command: crate::migrate::MigrationImportCommand::Pi { dry_run: true, .. },
            },
        })
    ));
    let cli = Cli::try_parse_from(["octet", "explain pi migration"]).unwrap();
    assert_eq!(cli.message.as_deref(), Some("explain pi migration"));
}

#[test]
fn serve_is_an_ordinary_prompt_not_a_subcommand() {
    let cli = Cli::try_parse_from(["octet", "serve"]).unwrap();
    assert!(cli.command.is_none());
    assert_eq!(cli.message.as_deref(), Some("serve"));
    assert!(Cli::try_parse_from(["octet", "serve", "--no-open"]).is_err());
}

#[test]
fn debug_prompt_is_an_explicit_prompt_template_diagnostic() {
    let cli =
        Cli::try_parse_from(["octet", "--print", "--prompt", "review", "--debug-prompt"]).unwrap();
    assert_eq!(cli.prompt_template.as_deref(), Some("review"));
    assert!(cli.debug_prompt);
}
