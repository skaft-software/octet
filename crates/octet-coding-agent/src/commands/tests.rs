//! Tests for the slash-command surface: the `/session` durable-fact rows, the
//! trust-queue wording, and the command table routing that produces them.
//!
//! Moved out of commands.rs so the command definitions and their argument
//! parsing stay readable on their own. The suite is a specification of what the
//! durable facts are required to claim, which is a different reader from the
//! one looking for which command a name maps to.

use super::*;

/// Row 2d.10: `/session` is the durable-fact surface, so every field it
/// claims must come from the session (file, id, head, message count, token
/// buckets, cost) and unknown exposure must be named rather than folded into
/// an exact-looking total.
#[test]
fn session_detail_reports_file_identity_messages_tokens_cost_and_uncertainty() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("detail-session.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![octet_ai::UserPart::Text("first question".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![AssistantPart::Text("first answer".into())],
                model: octet_ai::ModelId("fixture-model".into()),
                protocol: Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    let text = session_text(&session);
    assert!(text.contains("Session: detail-session"), "{text}");
    assert!(
        text.contains(&format!("File: {}", path.display())),
        "{text}"
    );
    assert!(text.contains("Head: 002"), "{text}");
    assert!(text.contains("Entries: 2"), "{text}");
    assert!(text.contains("Active-branch messages: 2"), "{text}");
    assert!(text.contains("Checkpoints: 0"), "{text}");
    assert!(
        text.contains("Tokens: 0 input · 0 cache-read · 0 cache-write · 0 output"),
        "{text}"
    );
    assert!(text.contains("Cost: $0.000000"), "{text}");
    assert!(
        !text.contains("uncertain"),
        "a fully priced, settled session is not uncertain: {text}"
    );

    // Unknown exposure must never read as an exact bill.
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("fixture-endpoint".into()),
            octet_ai::ModelId("fixture-model".into()),
            "assistant_turn",
        )
        .unwrap();
    let text = session_text(&session);
    assert!(
        text.contains("known subtotal only; usage or pricing uncertain"),
        "{text}"
    );
}

#[test]
fn debug_is_hidden_but_parses_and_reports_every_rendered_line() {
    assert_eq!(parse("/debug"), Command::Debug);
    assert!(matches!(parse("/debug extra"), Command::Unknown(_)));
    // The reference hides this command from discovery; the popup must not
    // advertise it even though it is accepted.
    assert!(slash_suggestions("/deb").is_empty());
    assert!(complete_slash_command("/deb").is_none());

    let lines = vec![
        "plain".to_owned(),
        "wide 漢字".to_owned(),
        "quote\"and\\slash".to_owned(),
    ];
    let report = debug_report_text(Some((120, 40)), Some(&lines), &[]);
    assert!(report.contains("Terminal: 120x40"));
    assert!(report.contains("Total lines: 3"));
    assert!(report.contains("[1] (w=9) \"wide 漢字\""), "{report}");
    assert!(
        report.contains(r#"[2] (w=15) "quote\"and\\slash""#),
        "{report}"
    );
    assert!(report.contains("=== Agent messages (JSONL) ==="));

    let message = Message::User(octet_ai::UserMessage {
        content: vec![octet_ai::UserPart::Text("hello".into())],
    });
    let report = debug_report_text(None, None, std::slice::from_ref(&message));
    assert!(report.contains("Terminal: unknown"));
    assert!(
        report.contains("unavailable (no renderer frame)"),
        "{report}"
    );
    assert!(
        report.lines().any(|line| line.contains("\"hello\"")),
        "{report}"
    );
}

#[test]
fn settings_and_scoped_models_parse_exactly_and_reject_malformed_forms() {
    assert_eq!(parse("/settings"), Command::Settings(SettingsCommand::Show));
    assert_eq!(
        parse("/settings theme"),
        Command::Settings(SettingsCommand::Theme(None))
    );
    assert_eq!(
        parse("/settings theme light"),
        Command::Settings(SettingsCommand::Theme(Some("light".into())))
    );
    assert_eq!(
        parse("/settings images off"),
        Command::Settings(SettingsCommand::Images(Some(false)))
    );
    assert_eq!(
        parse("/settings default model custom/alpha-model"),
        Command::Settings(SettingsCommand::DefaultModel(Some(
            "custom/alpha-model".into()
        )))
    );
    assert_eq!(
        parse("/settings default reasoning high"),
        Command::Settings(SettingsCommand::DefaultReasoning(Some("high".into())))
    );
    assert_eq!(
        parse("/settings transport"),
        Command::Settings(SettingsCommand::Transport)
    );
    assert_eq!(
        parse("/settings padding"),
        Command::Settings(SettingsCommand::Padding)
    );
    // Project trust is deliberately not a setting this surface can change.
    assert!(matches!(
        parse("/settings default trust always"),
        Command::Unknown(_)
    ));
    assert!(matches!(
        parse("/settings images maybe"),
        Command::Unknown(_)
    ));
    assert!(matches!(parse("/settings bogus"), Command::Unknown(_)));

    assert_eq!(
        parse("/scoped-models"),
        Command::ScopedModels(ScopedModelsCommand::Show)
    );
    assert_eq!(
        parse("/scoped-models all"),
        Command::ScopedModels(ScopedModelsCommand::All)
    );
    assert_eq!(
        parse("/scoped-models clear"),
        Command::ScopedModels(ScopedModelsCommand::Clear)
    );
    assert_eq!(
        parse("/scoped-models toggle openai/*"),
        Command::ScopedModels(ScopedModelsCommand::Toggle("openai/*".into()))
    );
    assert_eq!(
        parse("/scoped-models move gpt-6-astra top"),
        Command::ScopedModels(ScopedModelsCommand::Move {
            model: "gpt-6-astra".into(),
            direction: ScopeMove::Top,
        })
    );
    assert!(matches!(
        parse("/scoped-models move gpt-6-astra sideways"),
        Command::Unknown(_)
    ));
    // Both commands are discoverable popup entries with real parser routes.
    for name in ["settings", "scoped-models"] {
        assert!(SLASH_COMMANDS.iter().any(|command| command.name == name));
    }
}

#[test]
fn shell_escape_parser_distinguishes_included_and_excluded_commands() {
    assert_eq!(
        parse("!git status"),
        Command::Bash(BashEscape {
            command: "git status".into(),
            excluded: false,
        })
    );
    assert_eq!(
        parse("  !!rm -rf build  "),
        Command::Bash(BashEscape {
            command: "rm -rf build".into(),
            excluded: true,
        })
    );
    // Multi-line commands survive verbatim.
    assert_eq!(
        parse("!printf 'a\\nb'"),
        Command::Bash(BashEscape {
            command: "printf 'a\\nb'".into(),
            excluded: false,
        })
    );
    // `!` and `!!` alone are parsed so dispatch reports usage, not silence.
    assert_eq!(
        parse("!"),
        Command::Bash(BashEscape {
            command: String::new(),
            excluded: false,
        })
    );
    assert_eq!(
        parse("!!"),
        Command::Bash(BashEscape {
            command: String::new(),
            excluded: true,
        })
    );
    // Ordinary prose with an exclamation mark is untouched.
    assert!(matches!(parse("hello, world!"), Command::Unknown(_)));
}

#[test]
fn shell_escape_record_bounds_labels_and_never_leaks_excluded_text_into_context() {
    let included = ShellEscapeRecord::new("git status", "clean", 0, false);
    assert_eq!(included.prefix(), "!");
    assert!(!included.excluded());
    assert!(included.context_text().contains("$ git status"));
    assert!(included.context_text().contains("exit 0"));
    assert!(included.context_text().contains("clean"));
    assert!(!included
        .transcript_text()
        .contains("excluded from model context"));

    let excluded = ShellEscapeRecord::new("git log", "deadbeef", 1, true);
    assert_eq!(excluded.prefix(), "!!");
    assert!(excluded.excluded());
    assert!(excluded
        .transcript_text()
        .contains("[excluded from model context]"));

    // Oversized output is truncated on a character boundary and says so.
    let long = "漢".repeat(SHELL_ESCAPE_RECORD_BYTES);
    let record = ShellEscapeRecord::new("cat big", long, 0, false);
    assert!(record.output().len() <= SHELL_ESCAPE_RECORD_BYTES);
    assert!(record
        .output()
        .ends_with("truncated for the durable record ..."));
    assert!(record.output().is_char_boundary(record.output().len()));
}

#[test]
fn scoped_scope_targets_toggle_move_and_persist_in_requested_order() {
    use octet_ai::ModelId;
    let available = vec![
        ("custom/alpha-model".to_owned(), "custom-openai".to_owned()),
        ("gpt-6-astra".to_owned(), "openai".to_owned()),
        ("gpt-6-luna".to_owned(), "openai".to_owned()),
    ];
    let mut scope = Vec::<ScopedModel>::new();
    // A provider glob enables every model of that provider, in catalog order.
    assert_eq!(
        set_scope_target(&mut scope, &available, "openai/*", true),
        Ok(2)
    );
    assert_eq!(
        scope
            .iter()
            .map(|entry| entry.id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["gpt-6-astra", "gpt-6-luna"]
    );
    // A toggle flips a provider selection explicitly and never silently
    // no-ops on an unmatched target.
    assert_eq!(
        toggle_scope_target(&mut scope, &available, "openai/*"),
        Ok(false)
    );
    assert!(scope.is_empty());
    assert_eq!(
        toggle_scope_target(&mut scope, &available, "openai/*"),
        Ok(true)
    );
    assert_eq!(
        set_scope_target(&mut scope, &available, "custom/*", true),
        Ok(1)
    );
    assert_eq!(
        scope
            .iter()
            .map(|entry| entry.id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["gpt-6-astra", "gpt-6-luna", "custom/alpha-model"]
    );
    // Reorder moves exactly one entry and refuses a boundary no-op.
    assert_eq!(
        move_scope_model(&mut scope, "custom/alpha-model", ScopeMove::Top),
        Ok(())
    );
    assert!(move_scope_model(&mut scope, "custom/alpha-model", ScopeMove::Up).is_err());
    assert_eq!(
        move_scope_model(&mut scope, "gpt-6-luna", ScopeMove::Up),
        Ok(())
    );
    assert_eq!(
        move_scope_model(&mut scope, "custom/alpha-model", ScopeMove::Bottom),
        Ok(())
    );
    assert_eq!(
        scope
            .iter()
            .map(|entry| entry.id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["gpt-6-luna", "gpt-6-astra", "custom/alpha-model"]
    );
    // Persisted patterns are the exact ordered id list; a level rides along.
    scope[0].reasoning = Some("high".into());
    assert_eq!(
        scope_patterns_string(&scope).as_deref(),
        Some("gpt-6-luna:high,gpt-6-astra,custom/alpha-model")
    );
    assert_eq!(scope_patterns_string(&[]), None);
    let text = scoped_models_text(Some(&scope), &available);
    assert!(text.contains("gpt-6-luna:high"), "{text}");
    assert!(
        text.contains("3 of 3 available models, in this exact order"),
        "{text}"
    );
    let mut absent = scope.clone();
    absent.push(ScopedModel {
        id: ModelId("gone".into()),
        pattern: "gone".into(),
        reasoning: None,
    });
    assert!(
        scoped_models_text(Some(&absent), &available).contains("gone (unavailable on this route)")
    );
    assert!(scoped_models_text(None, &available).contains("(unrestricted) all 3 available models"));
    assert!(set_scope_target(&mut scope, &available, "nope/*", true).is_err());
}

#[test]
fn settings_text_reports_defaults_theme_transport_images_and_no_trust_default() {
    let surface = SettingsSurface {
        default_model: Some("gpt-4o-mini".into()),
        reasoning: "high".into(),
        theme: Some("dark".into()),
        transport: "websocket-preferred",
        endpoint: "codex".into(),
        show_images: true,
    };
    let text = settings_text(&surface);
    for expected in [
        "Default model      gpt-4o-mini",
        "Default reasoning  high",
        "Theme              dark",
        "Transport          websocket-preferred (declared by the codex route; not a user preference)",
        "Inline images      on",
        "Editor padding     compiled theme layout (no persisted override)",
        "Project trust is deliberately not persisted here",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text}");
    }
    // Unset defaults are named, never rendered as an empty value.
    let empty = SettingsSurface {
        default_model: None,
        reasoning: "off".into(),
        theme: None,
        transport: "http",
        endpoint: "custom".into(),
        show_images: false,
    };
    let text = settings_text(&empty);
    assert!(
        text.contains("Default model      (chosen at startup or by the session)"),
        "{text}"
    );
    assert!(text.contains("Theme              auto"), "{text}");
    assert!(text.contains("Inline images      off"), "{text}");
}

#[test]
fn changelog_parser_discovery_and_local_help() {
    for input in ["/changelog", " /changelog  ", "/chang"] {
        assert_eq!(parse(input), Command::Changelog);
        assert!(reject_tui_changelog(input)
            .unwrap_err()
            .to_string()
            .contains("interactive TUI"));
    }
    assert!(matches!(parse("/changelog extra"), Command::Unknown(_)));
    // `/checkout` was withdrawn, so this prefix is now unambiguous.
    assert_eq!(parse("/ch"), Command::Changelog);
    assert_eq!(complete_slash_command("/chang"), Some("/changelog".into()));
    let suggestions = slash_suggestions("/chang");
    assert_eq!(suggestions.len(), 1);
    assert!(!suggestions[0].accepts_argument);
    let help = help_text(Path::new("."), Some("changelog"));
    assert!(help.contains("/changelog") && help.contains("bundled release notes"));
    assert!(help_text(Path::new("."), None).contains("/changelog"));
    assert!(reject_tui_changelog("Explain the changelog").is_ok());
}

#[test]
fn changelog_bundle_matches_current_version_and_canonical_source() {
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        CURRENT_CHANGELOG.lines().next(),
        Some(format!("# octet {version}").as_str())
    );
    // Published packages have no repository docs tree. The package-local
    // include above must still compile; a checkout additionally guards drift.
    let canonical =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../docs/releases/v{version}.md"));
    if canonical.is_file() {
        assert_eq!(
            CURRENT_CHANGELOG,
            std::fs::read_to_string(canonical).unwrap()
        );
    }
}

#[test]
fn parses_the_complete_v1_command_grammar() {
    assert_eq!(parse("/login"), Command::Login(None));
    assert_eq!(parse("/setup"), Command::Setup);
    assert_eq!(parse("/setu"), Command::Setup);
    assert!(matches!(parse("/setup key"), Command::Unknown(_)));
    assert_eq!(
        parse("/logout openai-codex"),
        Command::Logout(Some("openai-codex".into()))
    );
    assert_eq!(
        parse("/model gpt-4o-mini"),
        Command::Model(Some("gpt-4o-mini".into()))
    );
    assert_eq!(parse("/thinking"), Command::Thinking(None));
    assert_eq!(parse("/theme"), Command::Theme(None));
    assert_eq!(parse("/theme light"), Command::Theme(Some("light".into())));
    assert!(matches!(parse("/theme neon"), Command::Theme(Some(_))));
    assert_eq!(parse("/verbose on"), Command::Verbose(Some(true)));
    assert_eq!(parse("/verbose off"), Command::Verbose(Some(false)));
    assert_eq!(parse("/answer"), Command::Answer(None));
    assert_eq!(
        parse("/answer summarize the verified findings concisely"),
        Command::Answer(Some("summarize the verified findings concisely".into()))
    );
    assert_eq!(parse("/compact"), Command::Compact);
    assert_eq!(
        parse("/compact preserve the API contract\nand test evidence"),
        Command::CompactWithInstructions("preserve the API contract\nand test evidence".into())
    );
    assert_eq!(parse("/auto-compact"), Command::AutoCompact(None));
    assert_eq!(
        parse("/auto-compact off"),
        Command::AutoCompact(Some(AutoCompactSetting::Mode(CompactionMode::Disabled)))
    );
    assert_eq!(
        parse("/auto-compact native"),
        Command::AutoCompact(Some(AutoCompactSetting::Mode(
            CompactionMode::NativeResponses
        )))
    );
    assert_eq!(
        parse("/auto-compact 85%"),
        Command::AutoCompact(Some(AutoCompactSetting::ThresholdPercent(85)))
    );
    assert_eq!(parse("/reload"), Command::Reload);
    assert_eq!(parse("/new"), Command::New);
    assert_eq!(parse("/resume id"), Command::Resume(Some("id".into())));
    assert_eq!(parse("/fork"), Command::Fork);
    assert_eq!(parse("/clone"), Command::Clone);
    assert_eq!(parse("/status"), Command::Status);
    assert_eq!(parse("/context"), Command::Context);
    assert_eq!(parse("/help"), Command::Help(None));
    assert_eq!(parse("/help status"), Command::Help(Some("status".into())));
    assert_eq!(parse("/cost"), Command::Cost);
    assert_eq!(parse("/hotkeys"), Command::Hotkeys);
    assert_eq!(parse("/copy"), Command::Copy);
    assert_eq!(parse("/session"), Command::Session);
    assert_eq!(parse("/session info"), Command::Session);
    assert!(matches!(parse("/session unknown"), Command::Unknown(_)));
    assert!(matches!(parse("/copy extra"), Command::Unknown(_)));
    assert_eq!(parse("/cache"), Command::Cache);
    assert_eq!(parse("/update"), Command::Update);
    assert_eq!(parse("/prompt"), Command::Prompt(None));
    assert_eq!(
        parse("/prompt review staged changes"),
        Command::Prompt(Some("review staged changes".into()))
    );
    assert_eq!(parse("/exit"), Command::Exit);
    assert_eq!(parse("/skills"), Command::Skills(SkillsSubcommand::List));
    assert_eq!(
        parse("/skills list"),
        Command::Skills(SkillsSubcommand::List)
    );
    assert_eq!(
        parse("/sk active"),
        Command::Skills(SkillsSubcommand::Active)
    );
    assert_eq!(
        parse("/skills search rust review"),
        Command::Skills(SkillsSubcommand::Search("rust review".into()))
    );
    assert_eq!(
        parse("/skills load audit"),
        Command::Skills(SkillsSubcommand::Load("audit".into()))
    );
    assert_eq!(
        parse("/skills reload"),
        Command::Skills(SkillsSubcommand::Reload)
    );
    assert_eq!(
        parse("/skills off audit"),
        Command::Skills(SkillsSubcommand::Off("audit".into()))
    );
    assert_eq!(
        parse("/extensions"),
        Command::Extensions(ExtensionsSubcommand::Menu)
    );
    assert_eq!(
        parse("/extensions list"),
        Command::Extensions(ExtensionsSubcommand::Menu)
    );
    assert_eq!(
        parse("/extensions status"),
        Command::Extensions(ExtensionsSubcommand::Status)
    );
    assert_eq!(
        parse("/extensions inspect agent-session:abc"),
        Command::Extensions(ExtensionsSubcommand::Inspect {
            reference: "agent-session:abc".into(),
        })
    );
    assert_eq!(
        parse("/extensions action octet-subagents stop-worker"),
        Command::Extensions(ExtensionsSubcommand::Action {
            extension: "octet-subagents".into(),
            action: "stop-worker".into(),
        })
    );
}

#[test]
fn slash_suggestions_filter_and_tab_complete_unique_prefixes() {
    assert_eq!(slash_suggestions("/").len(), SLASH_COMMANDS.len());
    assert_eq!(slash_suggestions("/mod")[0].usage, "/model [id]");
    for prefix in ["/log", "/logi", "/setu"] {
        let names = slash_suggestions(prefix)
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();
        assert!(
            names.contains(&"login") && names.contains(&"setup"),
            "{prefix}: {names:?}"
        );
        assert_eq!(
            names[0],
            if prefix.starts_with("/log") {
                "login"
            } else {
                "setup"
            }
        );
        assert_eq!(complete_slash_command(prefix), None);
    }
    assert_eq!(
        slash_suggestions("/login")
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>(),
        ["login"]
    );
    assert_eq!(
        slash_suggestions("/setup")
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>(),
        ["setup"]
    );
    assert_eq!(slash_suggestions("/th").len(), 2);
    assert!(slash_suggestions("/model ").is_empty());
    assert_eq!(complete_slash_command("/mod"), Some("/model ".to_owned()));
    assert_eq!(
        complete_slash_command("/thi"),
        Some("/thinking ".to_owned())
    );
    assert_eq!(
        complete_slash_command("/status"),
        Some("/status".to_owned())
    );
}

#[test]
fn popup_registry_includes_self_help_without_removed_commands() {
    assert!(SLASH_COMMANDS.iter().any(|command| command.name == "help"));
    for removed in ["cycle-model", "docs", "sessions", "tool"] {
        assert!(SLASH_COMMANDS.iter().all(|command| command.name != removed));
    }
    assert!(SLASH_COMMANDS
        .iter()
        .all(|command| command.name != "Session"));
}

#[test]
fn every_discovered_builtin_has_an_executable_parser_route() {
    for command in SLASH_COMMANDS {
        let invocation = match command.name {
            "name" => "/name release audit".to_owned(),
            "export" => "/export audit.md".to_owned(),
            name => format!("/{name}"),
        };
        assert!(
            !matches!(parse(&invocation), Command::Unknown(_)),
            "popup advertises /{} but its representative invocation {invocation:?} has no parser route",
            command.name
        );
    }
}

#[test]
fn parses_unambiguous_command_prefixes() {
    assert_eq!(parse("/mod"), Command::Model(None));
    assert_eq!(
        parse("/mo gpt-4o-mini"),
        Command::Model(Some("gpt-4o-mini".into()))
    );
    assert_eq!(parse("/comp"), Command::Compact);
    // /c and /t each match multiple commands, so they remain unknown.
    assert!(matches!(parse("/c"), Command::Unknown(_)));
    assert!(matches!(parse("/t"), Command::Unknown(_)));
}

#[test]
fn rejects_unknown_or_malformed_commands() {
    assert!(matches!(parse("hello"), Command::Unknown(_)));
    assert!(matches!(parse("/new extra"), Command::Unknown(_)));
    assert!(matches!(parse("/auto-compact 0%"), Command::Unknown(_)));
    assert!(matches!(parse("/auto-compact 101%"), Command::Unknown(_)));
    for removed in ["/cycle-model", "/docs", "/sessions", "/tool"] {
        assert!(matches!(parse(removed), Command::Unknown(_)));
    }
}

fn app_for_status() -> (tempfile::TempDir, App) {
    use crate::app::bootstrap::{bootstrap, build_app, LaunchSelection, SessionSelection};
    use crate::config::{CompactionPolicy, Config, Mode, ResumeSelector, SandboxPolicy};
    use octet_ai::{ModelId, ReasoningConfig};

    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        workspace: directory.path().to_owned(),
        invocation_cwd: directory.path().to_owned(),
        model: Some(ModelId("gpt-4o-mini".into())),
        model_explicit: false,
        reasoning: None,
        reasoning_explicit: false,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        reasoning_mode_explicit: false,
        cache_retention: octet_ai::CacheRetention::Short,
        effect_policy: octet_agent::EffectPolicy::Controlled,
        sandbox: SandboxPolicy {
            allow_external_paths: false,
            ..SandboxPolicy::default()
        },
        theme: None,
        system_prompt: None,
        theme_paths: vec![],
        color: crate::config::ColorMode::Auto,
        mouse: crate::config::MouseMode::Auto,
        plain: false,
        tern: crate::config::TernMode::Auto,
        show_images: false,
        session_dir: directory.path().join("sessions"),
        compaction: CompactionPolicy::default(),
        max_cost_microdollars: None,
        cost_warning_microdollars: None,
        max_turns: Some(40),
        show_reasoning_in_print: false,
        initial_prompt: None,
        prompt_template: None,
        debug_prompt: false,
        prompt_paths: vec![],
        mode: Mode::Interactive,
        resume: ResumeSelector::New,
        skill_paths: vec![],
        extension_paths: vec![],
        enabled_extensions: vec![],
        extension_activation_overridden: false,
        trusted_extensions: vec![],
        invocation_trusted_extensions: vec![],
        start_extension_processes: true,
        experimental_streamable_http_mcp: false,
        extension_flag_values: Default::default(),
        tools: crate::config::ToolPolicy::default(),
        telemetry: None,
        context_files: true,
        offline: true,
        workspace_trusted: true,
    };
    let boot = bootstrap(config).unwrap();
    let app = build_app(
        boot,
        LaunchSelection {
            model: ModelId("gpt-4o-mini".into()),
            session: SessionSelection::CreateNew(directory.path().join("session.jsonl")),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
        },
        "system".into(),
    )
    .unwrap();
    (directory, app)
}

#[test]
fn unpriced_usage_reports_remain_uncertain_on_a_priced_model_and_reopen() {
    let (_directory, mut app) = app_for_status();
    assert!(app.model.spec.pricing.is_some());
    let endpoint = app.model.endpoint.id.clone();
    let model = app.model.spec.id.clone();
    // Known zero is still an exact price; absent pricing is different.
    app.agent
        .session_mut()
        .record_compaction_usage(
            endpoint.clone(),
            model.clone(),
            Usage::default(),
            Some(Cost::default()),
        )
        .unwrap();
    assert!(!app.agent.session().has_unpriced_usage());
    assert!(!status_text(&app, None).contains("known subtotal"));
    app.agent
        .session_mut()
        .record_compaction_usage(
            endpoint,
            model,
            Usage {
                input_tokens: 5,
                output_tokens: 2,
                total_tokens: 7,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    for reopened in [false, true] {
        if reopened {
            app = crate::app::bootstrap::rebuild_app(app, None, None, None, None).unwrap();
        }
        let session = app.agent.session();
        assert!(session.has_unpriced_usage());
        assert!(
            !session.has_uncertain_usage(),
            "do not invent an unknown-token attempt"
        );
        assert!(
            app.model.spec.pricing.is_some(),
            "active catalog pricing cannot price a historical receipt"
        );
        assert!(status_text(&app, None).contains("known subtotal"));
        assert!(session_text(session).contains("known subtotal"));
        assert!(cost_text(session, &app.model).contains("Known subtotal only"));
        assert!(session.usage_uncertainty_records().is_empty());
    }
}

#[test]
fn status_references_real_runtime_features() {
    let (_directory, app) = app_for_status();
    let queued = Reconfig::NewSession;
    let status = status_text(&app, Some(&queued));
    for expected in [
        "Provider       openai",
        "Model          gpt-4o-mini",
        "Reasoning      off",
        "Workspace",
        "Session",
        "Context",
        "Model turns",
        "Tool calls",
        "Security model: local agent with workspace trust gates",
        "Effect policy: controlled (workspace mutation and unsafe bash calls need approval; other ambient host effects denied)",
        "Built-in file paths: workspace-only guard",
        "File edits: enabled",
        "Process execution: enabled",
        "Shell execution: enabled",
        "OS isolation: none",
        "Process privileges: current user",
        "Repository trust: trusted (project config/context/skills enabled)",
        "NewSession",
    ] {
        assert!(
            status.contains(expected),
            "missing {expected:?} in {status}"
        );
    }
    let expected_skills = format!(
        "Skills         0 active / {} discovered",
        app.skills.descriptors().len()
    );
    assert!(
        status.contains(&expected_skills),
        "missing {expected_skills:?} in {status}"
    );
}

/// A Codex-declared route with an explicit effective window, built on the
/// existing bootstrap fixture so no second catalog is invented.
fn codex_route(model_id: &str, context_window: u64) -> octet_ai::Model {
    let (_directory, app) = app_for_status();
    let mut model = app.model.clone();
    std::sync::Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
    std::sync::Arc::make_mut(&mut model.spec).id = octet_ai::ModelId(model_id.into());
    std::sync::Arc::make_mut(&mut model.spec)
        .limits
        .context_window = context_window;
    std::sync::Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_profile = octet_ai::ResponsesRuntimeProfile::Codex;
    model
}

#[test]
fn codex_context_surface_is_absent_for_every_other_route() {
    let (_directory, app) = app_for_status();
    assert_eq!(
        CodexContextSurface::capture(&app.model, true),
        None,
        "a non-Codex route must not offer a Codex context-window surface"
    );
}

#[test]
fn codex_context_surface_reports_the_deliberate_cap_and_why() {
    // astra advertises 872K, which the deliberate 272K cap reduces.
    let surface = CodexContextSurface::capture(&codex_route("gpt-6-astra", 272_000), true)
        .expect("Codex route");
    assert_eq!(surface.effective_window(), 272_000);
    assert!(!surface.has_uncertain_usage());
    let clamp = surface
        .clamp()
        .expect("the deliberate cap must be reported");
    assert_eq!(clamp.advertised_context_window, 872_000);
    assert_eq!(clamp.effective_context_window, 272_000);
    let message = clamp.message();
    assert!(message.contains("double-priced"), "{message}");
    assert!(message.contains("websocket"), "{message}");
    let summary = surface.summary_lines().join("\n");
    // Labelled windows, matching the session note's house style.
    assert!(summary.contains("advertised 872K"), "{summary}");
    assert!(summary.contains("effective 272K"), "{summary}");
}

/// The effort menu is user-facing prose. It must never render an internal
/// API path, function call, or operation identifier, and every window it
/// quotes must be labelled.
#[test]
fn the_effort_menu_summary_never_renders_internal_identifiers() {
    for (model_id, window) in [
        ("gpt-6-astra", 272_000u64),
        ("gpt-5.6-luna", 372_000),
        ("gpt-6-astra", 872_000),
        ("gpt-5.6-luna", 1_000_000),
    ] {
        for entitled in [false, true] {
            let surface = CodexContextSurface::capture(&codex_route(model_id, window), entitled)
                .expect("Codex route");
            let summary = surface.summary_lines().join("\n");
            for leak in [
                "Session::",
                "record_usage_uncertainty",
                "codex-context-above-272k",
                "uncertain_usage_operation",
                "::",
                "()",
                "crates/",
                "fn ",
                "CodexContext",
            ] {
                assert!(
                    !summary.contains(leak),
                    "{model_id}/{window}/entitled={entitled}: internal identifier {leak:?} \
                     leaked: {summary}"
                );
            }
            // Every quoted window is labelled, never a bare number.
            assert!(
                !summary.contains("272000") && !summary.contains("372000"),
                "unlabelled window: {summary}"
            );
            assert!(!summary.contains('$'), "exact cost figure: {summary}");
            // The deliberate cap is described as a decision, never a defect.
            for wrong in ["bug", "regression", "broken", "incorrect"] {
                assert!(!summary.contains(wrong), "{wrong:?} in {summary}");
            }
            assert!(
                !summary.contains("no ceiling") && !summary.contains("?"),
                "placeholder-style blob: {summary}"
            );
        }
    }
}

#[test]
fn above_the_standard_tier_cost_is_uncertain_never_an_exact_figure() {
    // `gpt-5.6-luna` is the documented 372K family.
    let surface = CodexContextSurface::capture(&codex_route("gpt-5.6-luna", 372_000), true)
        .expect("Codex route");
    assert!(surface.has_uncertain_usage());
    assert_eq!(
        surface.uncertain_usage_operation(),
        Some(crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION)
    );
    let summary = surface.summary_lines().join("\n");
    assert!(summary.contains("UNCERTAIN"), "{summary}");
    assert!(summary.contains("double-priced"), "{summary}");
    assert!(summary.contains("websocket"), "{summary}");
    assert!(
        !summary.contains('$'),
        "no exact-looking cost figure may be rendered above the standard tier: {summary}"
    );
    // The operation id is an internal diagnostic; it is recorded by the
    // session, never rendered. `the_effort_menu_summary_never_renders_...`
    // asserts that for every Codex family and entitlement.
    assert!(!summary.contains("codex-context-above-272k"), "{summary}");
}

#[test]
fn a_raise_fails_closed_without_the_entitlement_or_the_acknowledgement() {
    let unentitled = CodexContextSurface::capture(&codex_route("gpt-6-astra", 272_000), false)
        .expect("Codex route");
    assert_eq!(unentitled.raise_target(), None);
    let refused = unentitled.raise(872_000, true).unwrap_err();
    assert!(refused.contains("Pro or ProLite"), "{refused}");
    assert!(
        unentitled
            .summary_lines()
            .join("\n")
            .contains("Pro or ProLite"),
        "the unentitled surface must say what a raise needs"
    );

    let entitled = CodexContextSurface::capture(&codex_route("gpt-6-astra", 272_000), true)
        .expect("Codex route");
    assert_eq!(entitled.raise_target(), Some(872_000));
    let unacknowledged = entitled.raise(872_000, false).unwrap_err();
    assert!(unacknowledged.contains("double-priced"), "{unacknowledged}");
    assert!(unacknowledged.contains("websocket"), "{unacknowledged}");
    assert_eq!(entitled.raise(872_000, true), Ok(872_000));
    // Above the model's entitlement the request is refused outright.
    let above = entitled.raise(4_000_000, true).unwrap_err();
    assert!(above.contains("872000"), "{above}");
    assert!(entitled
        .raise_instruction(872_000)
        .contains("--codex-context-window 872000"));
}
