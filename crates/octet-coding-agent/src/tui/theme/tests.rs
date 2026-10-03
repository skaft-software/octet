//! Tests for theme compilation, selection, and live preview.
//!
//! Why this is a separate module: `theme.rs` owns the compile pipeline and the
//! selection state machine, which are the two things a reader needs to keep in
//! their head at once. Everything else here is evidence that those two hold.

use super::*;
use crate::config::{CompactionPolicy, Mode, ResumeSelector, SandboxPolicy};

fn config(workspace: PathBuf) -> Config {
    Config {
        workspace: workspace.clone(),
        invocation_cwd: workspace,
        model: None,
        model_explicit: false,
        reasoning: None,
        reasoning_explicit: false,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        reasoning_mode_explicit: false,
        cache_retention: octet_ai::CacheRetention::Short,
        cache_warming: octet_agent::CacheWarmMode::default(),
        show_cache_miss_notices: false,
        effect_policy: octet_agent::EffectPolicy::Controlled,
        sandbox: SandboxPolicy::default(),
        theme: None,
        system_prompt: None,
        theme_paths: vec![],
        color: crate::config::ColorMode::Auto,
        mouse: crate::config::MouseMode::Auto,
        plain: false,
        tern: crate::config::TernMode::Auto,
        show_images: false,
        session_dir: PathBuf::from("sessions"),
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
    }
}

fn contrast(left: Rgb, right: Rgb) -> f64 {
    let (dark, light) = if relative_luminance(left) < relative_luminance(right) {
        (left, right)
    } else {
        (right, left)
    };
    (relative_luminance(light) + 0.05) / (relative_luminance(dark) + 0.05)
}

#[test]
fn shimmer_mode_accepts_only_the_documented_values() {
    assert_eq!(ShimmerMode::parse("classic"), Some(ShimmerMode::Classic));
    assert_eq!(
        ShimmerMode::parse(" PHYSICAL "),
        Some(ShimmerMode::Physical)
    );
    assert_eq!(ShimmerMode::parse("legacy"), None);
    assert_eq!(ShimmerMode::parse(""), None);
}

#[test]
fn project_theme_is_discovered_loaded_and_names_are_deduplicated() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path().to_owned());
    let project = config.workspace.join(".octet/themes");
    let explicit = directory.path().join("explicit-themes");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&explicit).unwrap();
    std::fs::write(project.join("shared.toml"), "accent = '#123456'").unwrap();
    std::fs::write(explicit.join("custom.toml"), "accent = '#654321'").unwrap();
    config.theme_paths.push(explicit);

    assert_eq!(
        theme_path("shared", &config),
        Some(project.canonicalize().unwrap().join("shared.toml"))
    );
    let theme = load_named_theme("shared", &config).unwrap();
    assert_eq!(
        theme.resolve::<String>("accent").as_deref(),
        Some("#123456")
    );

    let names = available_themes(&config);
    assert!(names.contains(&DEFAULT_THEME_NAME.to_owned()));
    assert!(names.contains(&"shared".to_owned()));
    assert!(names.contains(&"custom".to_owned()));
    assert_eq!(names.iter().filter(|name| *name == "shared").count(), 1);
}

#[test]
fn picker_loads_only_valid_winning_theme_files_and_reserves_builtin_names() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path().to_owned());
    let project = config.workspace.join(".octet/themes");
    let explicit = directory.path().join("explicit-themes");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&explicit).unwrap();
    std::fs::write(project.join("picker-valid.toml"), "accent = '#123456'").unwrap();
    std::fs::write(project.join("picker-shared.toml"), "accent = '#111111'").unwrap();
    std::fs::write(project.join("picker-shadowed.toml"), "accent = '#222222'").unwrap();
    std::fs::write(explicit.join("picker-shared.toml"), "accent = '#abcdef'").unwrap();
    std::fs::write(explicit.join("picker-shadowed.toml"), "[invalid").unwrap();
    std::fs::write(explicit.join("picker-invalid.toml"), "[invalid").unwrap();
    std::fs::write(explicit.join("dark.toml"), "accent = '#123456'").unwrap();
    std::fs::write(explicit.join("default.toml"), "accent = '#123456'").unwrap();
    std::fs::write(
        explicit.join("picker-double.toml.toml"),
        "accent = '#123456'",
    )
    .unwrap();
    std::fs::write(
        explicit.join("picker-too-large.toml"),
        vec![b' '; MAX_THEME_BYTES as usize + 1],
    )
    .unwrap();
    let standalone = directory.path().join("picker-standalone.toml");
    std::fs::write(&standalone, "accent = '#654321'").unwrap();
    config.theme_paths.push(explicit.clone());
    config.theme_paths.push(standalone);

    let options = selectable_file_themes(&config, TerminalBackground::Dark);
    let names: Vec<_> = options.iter().map(|(name, _)| name.as_str()).collect();
    assert!(names.contains(&"picker-valid"));
    assert!(names.contains(&"picker-shared"));
    assert!(names.contains(&"picker-standalone"));
    for hidden in [
        "picker-shadowed",
        "picker-invalid",
        "picker-too-large",
        "dark",
        "default",
        "picker-double.toml",
    ] {
        assert!(
            !names.contains(&hidden),
            "unexpected picker option {hidden}"
        );
    }
    let shared = &options
        .iter()
        .find(|(name, _)| name == "picker-shared")
        .unwrap()
        .1;
    assert_eq!(
        shared.source_path(),
        Some(
            explicit
                .canonicalize()
                .unwrap()
                .join("picker-shared.toml")
                .as_path()
        )
    );
    assert_eq!(
        shared.resolve::<String>("accent").as_deref(),
        Some("#abcdef")
    );
    config.theme = Some("dark".into());
    assert!(load_theme_for_background(&config, TerminalBackground::Unknown).is_compiled_default());
    assert_eq!(
        load_theme_for_background(&config, TerminalBackground::Unknown).background(),
        TerminalBackground::Dark
    );
}

#[test]
fn cards_example_theme_is_valid_for_every_background_profile() {
    // `examples/themes/Cards.toml` is the source for the `Cards` built-in.
    // Every release build compiles it in, so a change to the example must
    // never break schema validation, the bounded size limit, or any
    // background profile it claims to support.
    const CARDS: &str = include_str!("../../../../../examples/themes/Cards.toml");
    assert!(
        CARDS.len() as u64 <= MAX_THEME_BYTES,
        "Cards.toml exceeds MAX_THEME_BYTES"
    );
    // Only string values are schema-checked for control bytes; ordinary
    // newlines and tabs in comments and whitespace are fine.
    for line in CARDS.lines() {
        if line.trim_start().starts_with('#') {
            assert!(
                !line.chars().any(|ch| ch.is_control() && ch != '\t'),
                "control byte in comment: {line:?}"
            );
            continue;
        }
        assert!(
            !line.chars().any(char::is_control),
            "control byte in {CARDS}: {line:?}"
        );
    }
    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        let theme = load_theme_source_for(
            CARDS,
            "Cards",
            ThemeSource::CompiledCards,
            "Cards",
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            background,
        )
        .expect("Cards example theme must compile for every background");
        assert_eq!(theme.background(), background);
        assert_eq!(theme.metadata().name, "Cards");
        assert!(!theme.is_compiled_default());
        assert_eq!(
            theme.resolve::<String>("prompt_wash").as_deref(),
            Some("false")
        );
        assert_eq!(
            theme.resolve::<String>("splash_compact").as_deref(),
            Some("true")
        );
        assert_eq!(
            theme.resolve::<String>("splash_model_adaptive").as_deref(),
            Some("true")
        );
        // The adaptive splash claims the whole splash, so `Cards` must not
        // also pin a `splash` colour.
        assert_eq!(theme.resolve::<String>("splash"), None);
    }
}

#[test]
fn cards_is_a_compiled_builtin_that_reserved_files_cannot_shadow() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path().to_owned());
    let themes = directory.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    // A user's own `Cards.toml` must not replace, or be replaced by, the
    // compiled-in selector.
    std::fs::write(
        themes.join("Cards.toml"),
        "[metadata]\nname = \"Impostor\"\n[colors]\naccent = '#123456'\n",
    )
    .unwrap();
    config.theme_paths.push(themes);

    let names = available_themes(&config);
    assert!(names.contains(&CARDS_THEME_NAME.to_owned()));
    for selector in ["Cards", "cards", "CARDS", "Cards.toml"] {
        let theme = load_named_theme(selector, &config)
            .unwrap_or_else(|error| panic!("{selector}: {error}"));
        assert!(!theme.is_compiled_default(), "{selector}");
        assert_eq!(theme.metadata().name, "Cards", "{selector}");
        assert!(matches!(theme.source(), ThemeSource::CompiledCards));
        assert_eq!(theme.source_path(), None, "{selector}");
    }
    // The impostor file stays out of the file picker and out of reach.
    assert!(
        !selectable_file_themes(&config, TerminalBackground::Unknown)
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(CARDS_THEME_NAME))
    );
    assert!(is_reserved_theme_name("Cards"));
    assert!(is_reserved_theme_name("cards"));

    // Startup honours the selector and keeps its own background profile.
    config.theme = Some(CARDS_THEME_NAME.to_owned());
    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        let theme = load_theme_for_background(&config, background);
        assert!(!theme.is_compiled_default(), "{background:?}");
        assert_eq!(theme.background(), background);
        assert_eq!(theme.metadata().name, "Cards");
        // A compiled-in theme never creates a reload watcher.
        assert!(theme.source_path().is_none(), "{background:?}");
    }
}

#[test]
fn still_example_theme_is_valid_for_every_background_profile() {
    // `examples/themes/Still.toml` is the source for the `Still` built-in.
    // Every release build compiles it in, so a change to the example must
    // never break schema validation, the bounded size limit, or any
    // background profile it claims to support.
    const STILL: &str = include_str!("../../../../../examples/themes/Still.toml");
    assert!(
        STILL.len() as u64 <= MAX_THEME_BYTES,
        "Still.toml exceeds MAX_THEME_BYTES"
    );
    // Only string values are schema-checked for control bytes; ordinary
    // newlines and tabs in comments and whitespace are fine.
    for line in STILL.lines() {
        if line.trim_start().starts_with('#') {
            assert!(
                !line.chars().any(|ch| ch.is_control() && ch != '\t'),
                "control byte in comment: {line:?}"
            );
            continue;
        }
        assert!(
            !line.chars().any(char::is_control),
            "control byte in {STILL}: {line:?}"
        );
    }
    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        let theme = load_theme_source_for(
            STILL,
            "Still",
            ThemeSource::CompiledStill,
            "Still",
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            background,
        )
        .expect("Still example theme must compile for every background");
        assert_eq!(theme.background(), background);
        assert_eq!(theme.metadata().name, "Still");
        assert!(!theme.is_compiled_default());
        // Model-adaptive is the point of this theme: the prompt chevron,
        // composer marker, and shimmer follow the active model family.
        assert!(theme.uses_model_lab_color());
        assert_eq!(
            theme.resolve::<String>("model.use_lab_color").as_deref(),
            Some("true")
        );
        // The startup splash follows the same rule, in the compact geometry
        // the compiled default uses. The adaptive splash claims the whole
        // mark, so Still must not also pin a `splash` colour.
        assert_eq!(
            theme.resolve::<String>("splash_model_adaptive").as_deref(),
            Some("true")
        );
        assert_eq!(
            theme.resolve::<String>("splash_compact").as_deref(),
            Some("true")
        );
        assert_eq!(theme.resolve::<String>("splash"), None);
        // Still uses one soft prompt band; activity and prose stay plain.
        for kind in [
            "assistant",
            "reasoning",
            "tool",
            "notice",
            "outcome",
            "shell",
            "compaction",
        ] {
            let surface = theme.surface_for_width(kind, 100);
            assert_eq!(surface.chrome, ThemeSurfaceChrome::Plain, "{kind}");
            assert_eq!(surface.padding, 0, "{kind}");
        }
        let prompt = theme.surface_for_width("user", 100);
        assert_eq!(prompt.chrome, ThemeSurfaceChrome::Band);
        assert_eq!(prompt.padding, 1);
        assert_eq!(
            theme.resolve::<String>("composer").as_deref(),
            Some("shaded")
        );
        assert_eq!(theme.resolve::<u16>("content_max_width"), None);
        assert_eq!(theme.resolve::<u16>("event_marker_gutter"), Some(3));
        // A live model family overrides the quiet sage `model_accent`.
        let mut adapted = theme;
        apply_model_lab(&mut adapted, ModelLab::Anthropic);
        assert_ne!(
            adapted.resolve::<String>("model_accent").as_deref(),
            Some("#6f9182"),
            "model-adaptive Still must not keep its fixed fallback accent"
        );
    }
}

#[test]
fn still_is_a_compiled_builtin_that_reserved_files_cannot_shadow() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path().to_owned());
    let themes = directory.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    // A user's own `Still.toml` must not replace, or be replaced by, the
    // compiled-in selector.
    std::fs::write(
        themes.join("Still.toml"),
        "[metadata]\nname = \"Impostor\"\n[colors]\naccent = '#123456'\n",
    )
    .unwrap();
    config.theme_paths.push(themes);

    let names = available_themes(&config);
    assert!(names.contains(&STILL_THEME_NAME.to_owned()));
    for selector in ["Still", "still", "STILL", "Still.toml"] {
        let theme = load_named_theme(selector, &config)
            .unwrap_or_else(|error| panic!("{selector}: {error}"));
        assert!(!theme.is_compiled_default(), "{selector}");
        assert_eq!(theme.metadata().name, "Still", "{selector}");
        assert!(matches!(theme.source(), ThemeSource::CompiledStill));
        assert_eq!(theme.source_path(), None, "{selector}");
    }
    // The impostor file stays out of the file picker and out of reach.
    assert!(
        !selectable_file_themes(&config, TerminalBackground::Unknown)
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(STILL_THEME_NAME))
    );
    assert!(is_reserved_theme_name("Still"));
    assert!(is_reserved_theme_name("still"));

    // Startup honours the selector and keeps its own background profile.
    config.theme = Some(STILL_THEME_NAME.to_owned());
    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        let theme = load_theme_for_background(&config, background);
        assert!(!theme.is_compiled_default(), "{background:?}");
        assert_eq!(theme.background(), background);
        assert_eq!(theme.metadata().name, "Still");
        // A compiled-in theme never creates a reload watcher.
        assert!(theme.source_path().is_none(), "{background:?}");
    }
}

/// Every compiled-in file theme must reach `reload` through the same table
/// that resolves its selector, so a new built-in cannot compile on first
/// load and then silently fall back on reload.
#[test]
fn every_compiled_file_theme_reloads_from_its_own_embedded_source() {
    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        for selector in compiled_file_theme_names() {
            let theme = load_named_theme_for_background(
                selector,
                &config(std::env::temp_dir()),
                background,
            )
            .unwrap_or_else(|error| panic!("{selector}: {error}"));
            assert!(!theme.is_compiled_default(), "{selector}");
            let reloaded = theme
                .reload()
                .unwrap_or_else(|error| panic!("{selector} reload: {error}"));
            assert_eq!(
                reloaded.metadata().name,
                theme.metadata().name,
                "{selector}"
            );
            assert_eq!(reloaded.source(), theme.source(), "{selector}");
            assert_eq!(reloaded.background(), background, "{selector}");
        }
    }
}

#[test]
fn missing_and_legacy_names_keep_the_compiled_default_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path().to_owned());
    let names = available_themes(&config);
    assert!(names.contains(&DEFAULT_THEME_NAME.to_owned()));
    assert!(load_named_theme(DEFAULT_THEME_NAME, &config).is_ok());
    assert!(load_named_theme("default.toml", &config)
        .unwrap()
        .is_compiled_default());
    for name in ["legacy-theme", "custom", "compact"] {
        assert!(
            load_named_theme(name, &config).is_err(),
            "unexpected theme availability for {name}"
        );
    }

    let custom_dir = directory.path().join("themes");
    std::fs::create_dir_all(&custom_dir).unwrap();
    std::fs::write(custom_dir.join("custom.toml"), "accent = '#123456'").unwrap();
    let mut configured = config;
    configured.theme_paths.push(custom_dir);
    configured.theme = Some("custom".to_owned());
    assert!(available_themes(&configured).contains(&"custom".to_owned()));
    assert!(
        !load_theme_for_background(&configured, TerminalBackground::Unknown).is_compiled_default()
    );

    for name in ["legacy-theme", "compact"] {
        configured.theme = Some(name.to_owned());
        for background in [
            TerminalBackground::Unknown,
            TerminalBackground::Dark,
            TerminalBackground::Light,
        ] {
            let theme = load_theme_for_background(&configured, background);
            assert!(theme.is_compiled_default());
            assert_eq!(theme.background(), background);
            assert!(theme.layout_for_width(80).show_footer);
        }
    }
}

#[test]
fn malformed_and_oversized_named_themes_fall_back_without_startup_error() {
    let directory = tempfile::tempdir().unwrap();
    let themes = directory.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(themes.join("malformed.toml"), "[colors\naccent = '#123456'").unwrap();
    std::fs::write(
        themes.join("oversized.toml"),
        vec![b' '; MAX_THEME_BYTES as usize + 1],
    )
    .unwrap();

    let mut config = config(directory.path().to_owned());
    config.theme_paths.push(themes);
    for (name, expected_error) in [("malformed", ""), ("oversized", "too large")] {
        config.theme = Some(name.to_owned());
        let error = load_named_theme(name, &config).unwrap_err().to_string();
        if !expected_error.is_empty() {
            assert!(error.contains(expected_error), "{error}");
        }
        assert!(
            load_theme_for_background(&config, TerminalBackground::Unknown).is_compiled_default(),
            "{name} must use the compiled fallback"
        );
    }
}

#[test]
fn theme_names_cannot_traverse_outside_discovered_roots() {
    let directory = tempfile::tempdir().unwrap();
    let themes = directory.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(themes.join("safe.toml"), "accent = '#123456'").unwrap();
    std::fs::write(directory.path().join("outside.toml"), "accent = '#654321'").unwrap();
    let mut config = config(directory.path().to_owned());
    config.theme_paths.push(themes);

    assert!(theme_path("safe", &config).is_some());
    for name in [
        "../outside",
        r"..\outside",
        "/tmp/outside",
        "safe/../safe",
        "..",
    ] {
        assert!(
            theme_path(name, &config).is_none(),
            "accepted unsafe name {name:?}"
        );
        assert!(
            load_named_theme(name, &config).is_err(),
            "loaded unsafe name {name:?}"
        );
    }
}

#[test]
fn untrusted_project_themes_are_not_selected() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path().to_owned());
    config.workspace_trusted = false;
    let project = config.workspace.join(".octet/themes");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("untrusted-project.toml"), "accent = '#123456'").unwrap();

    assert!(theme_path("untrusted-project", &config).is_none());
    assert!(!available_themes(&config).contains(&"untrusted-project".to_owned()));
    assert!(
        !selectable_file_themes(&config, TerminalBackground::Unknown)
            .iter()
            .any(|(name, _)| name == "untrusted-project")
    );
    assert!(theme_discovery_diagnostics(&config)
        .iter()
        .any(|diagnostic| { diagnostic.message.contains("workspace is not trusted") }));
    config.theme = Some("untrusted-project".to_owned());
    assert!(load_theme_for_background(&config, TerminalBackground::Unknown).is_compiled_default());
}

#[cfg(unix)]
#[test]
fn symlink_and_fifo_theme_candidates_are_not_selected() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let themes = directory.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    let target = directory.path().join("target.toml");
    std::fs::write(&target, "accent = '#123456'").unwrap();
    symlink(&target, themes.join("linked.toml")).unwrap();

    let fifo = themes.join("pipe.toml");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

    let mut config = config(directory.path().to_owned());
    config.theme_paths.push(themes);
    let names = available_themes(&config);
    assert!(!names.contains(&"linked".to_owned()));
    assert!(!names.contains(&"pipe".to_owned()));
    assert!(
        !selectable_file_themes(&config, TerminalBackground::Unknown)
            .iter()
            .any(|(name, _)| name == "linked" || name == "pipe")
    );
    assert!(theme_path("linked", &config).is_none());
    assert!(theme_path("pipe", &config).is_none());
    assert!(theme_discovery_diagnostics(&config)
        .iter()
        .any(|diagnostic| {
            diagnostic.path.ends_with("linked.toml")
                && diagnostic
                    .message
                    .contains("candidate must not be a symlink")
        }));
}

#[test]
fn compiled_default_keeps_baseline_layout_and_plain_surfaces() {
    let theme = test_theme();
    assert_eq!(theme.layout, ThemeLayout::default());
    assert_eq!(theme.surfaces, default_surfaces());
    assert_eq!(
        theme.metadata.description,
        "Terminal-neutral compiled theme"
    );
}

#[test]
fn the_default_theme_opts_into_the_full_cell_prompt_wash() {
    // The wash is a theme capability, not a compiled-theme special case, so
    // the default theme declares it like every other theme would.
    let theme = test_theme();
    assert_eq!(
        theme.resolve::<String>("prompt_wash").as_deref(),
        Some("true")
    );
    assert!(theme.prompt_wash());
    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        assert!(
            test_theme_for(
                background,
                TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            )
            .prompt_wash(),
            "{background:?}"
        );
    }
    // A file theme inherits the wash, and `prompt_wash = false` opts out.
    let inherited = test_theme_from_source("[colors]\naccent = \"#456789\"");
    assert!(inherited.prompt_wash());
    let opted_out = test_theme_from_source("[colors]\nprompt_wash = false");
    assert!(!opted_out.prompt_wash());
}

#[test]
fn resolver_selected_theme_paths_are_bounded_validated_and_reloadable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    std::fs::write(
        &path,
        r##"
                [metadata]
                name = "Custom"
                [colors]
                accent = "#456789"
                [roles."extension.custom"]
                foreground = "accent"
                bold = true
                [glyphs]
                prompt = ":"
            "##,
    )
    .unwrap();
    let config = config(directory.path().to_owned());
    let theme = load_theme_path_for(
        &path,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Unknown,
    )
    .unwrap();
    assert_eq!(theme.metadata().name, "Custom");
    assert_eq!(theme.source_path(), Some(path.as_path()));
    assert_eq!(theme.glyph("prompt"), ":");
    assert!(theme
        .apply_semantic_role("extension.custom", "custom")
        .contains("custom"));

    std::fs::write(
        &path,
        "[metadata]\nname = 'Reloaded'\n[glyphs]\nprompt = '#'\n",
    )
    .unwrap();
    let reloaded = theme.reload().unwrap();
    assert_eq!(reloaded.metadata().name, "Reloaded");
    assert_eq!(reloaded.glyph("prompt"), "#");

    let oversized = directory.path().join("oversized.toml");
    std::fs::write(&oversized, vec![b' '; MAX_THEME_BYTES as usize + 1]).unwrap();
    let error = load_theme_path(&oversized, &config)
        .unwrap_err()
        .to_string();
    assert!(error.contains("too large"));
}

#[cfg(unix)]
#[test]
fn file_theme_reload_rejects_a_path_swapped_to_a_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("active.toml");
    let replacement = directory.path().join("replacement.toml");
    std::fs::write(&path, "[metadata]\nname = 'Initial'\n").unwrap();
    std::fs::write(&replacement, "[metadata]\nname = 'Replacement'\n").unwrap();
    let theme = load_theme_path_for(
        &path,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Unknown,
    )
    .unwrap();

    std::fs::remove_file(&path).unwrap();
    symlink(&replacement, &path).unwrap();
    assert!(theme.reload().is_err());
}

#[test]
fn shipped_reference_theme_is_schema_valid_and_variant_aware() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/themes/octet-default.toml");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert!(
        source.len() as u64 <= MAX_THEME_BYTES,
        "reference theme exceeds the bounded file size"
    );

    let dark = theme_schema::parse_theme(&source, "reference", TerminalBackground::Dark).unwrap();
    let light = theme_schema::parse_theme(&source, "reference", TerminalBackground::Light).unwrap();
    assert_eq!(
        dark.tokens.get("accent").map(String::as_str),
        Some("#16876d")
    );
    assert_eq!(
        dark.tokens.get("md_code_bg").map(String::as_str),
        Some("#202630"),
        "dark variant keeps the dark fenced-code surface"
    );
    assert_eq!(
        light.tokens.get("md_code_bg").map(String::as_str),
        Some("#f1f5f4"),
        "light variant overrides the universal fenced-code surface"
    );
    assert!(dark.roles.contains_key("extension.example.badge"));
    assert_eq!(
        dark.glyphs.len(),
        dark.ascii_glyphs.len(),
        "every unicode glyph has an ASCII fallback"
    );

    // The file must compile through the real bounded loader, not just parse.
    let compiled = load_resolved_theme_for(
        &path,
        &source,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Dark,
    )
    .unwrap();
    assert!(matches!(compiled.source(), ThemeSource::File(_)));
    assert_eq!(compiled.metadata().name, "octet default reference");
    assert_eq!(
        compiled.resolve::<String>("md_code_bg").as_deref(),
        Some("#202630")
    );
}

#[test]
fn active_theme_reload_poll_applies_edits_and_retains_last_good() {
    use crate::tui::theme_reload::{try_send_change, FileChangeEvent, FileChangeKind};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("active.toml");
    std::fs::write(
        &path,
        "[metadata]\nname = 'Initial'\n[colors]\naccent = '#111111'\n",
    )
    .unwrap();
    let theme = load_theme_path_for(
        &path,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Dark,
    )
    .unwrap();

    let (mut reload, sender) =
        ThemeFileReload::new(&theme, ThemeReloadMode::Interactive, Duration::ZERO).unwrap();
    assert!(
        !reload.set_active_theme(&theme).unwrap(),
        "same source is idempotent"
    );
    let watch = reload.watch_spec().expect("interactive file theme watches");
    assert_eq!(watch.directory(), directory.path());
    assert!(!watch.recursive());

    let now = Instant::now();
    // A busy boundary drains the event but never admits a reload.
    assert!(try_send_change(
        &sender,
        FileChangeEvent::new(&path, FileChangeKind::Modify)
    ));
    assert!(reload.poll(now, ReloadBoundary::Busy).is_none());

    // A real edit loads through OctetTheme::reload at the idle boundary.
    std::fs::write(
        &path,
        "[metadata]\nname = 'Edited'\n[colors]\naccent = '#222222'\n",
    )
    .unwrap();
    assert!(try_send_change(
        &sender,
        FileChangeEvent::new(&path, FileChangeKind::Modify)
    ));
    match reload.poll(now, ReloadBoundary::Idle).expect("applied") {
        ReloadDecision::Applied(theme) => assert_eq!(theme.metadata().name, "Edited"),
        other => panic!("expected an applied theme, got {other:?}"),
    }

    // An invalid edit retains the last-good theme instead of applying it.
    std::fs::write(&path, "[metadata]\nname = 7\n").unwrap();
    assert!(try_send_change(
        &sender,
        FileChangeEvent::new(&path, FileChangeKind::Modify)
    ));
    assert!(matches!(
        reload.poll(now, ReloadBoundary::Idle),
        Some(ReloadDecision::RetainedLastGood {
            failure: ReloadFailureKind::Invalid
        })
    ));
    assert_eq!(reload.last_good().metadata().name, "Edited");

    // A removed source installs the compiled fallback.
    std::fs::remove_file(&path).unwrap();
    assert!(try_send_change(
        &sender,
        FileChangeEvent::new(&path, FileChangeKind::Remove)
    ));
    assert!(matches!(
        reload.poll(now, ReloadBoundary::Idle),
        Some(ReloadDecision::FellBackToCompiledDefault(_))
    ));

    // Non-interactive modes stay inert.
    reload.set_mode(ThemeReloadMode::Print);
    assert!(reload.watch_spec().is_none());
    assert!(reload.poll(now, ReloadBoundary::Idle).is_none());
}

#[test]
fn published_semantic_role_vocabulary_is_closed_and_accepted() {
    let mut seen = std::collections::BTreeSet::new();
    for name in SEMANTIC_ROLE_VOCABULARY {
        assert!(seen.insert(*name), "duplicate published role {name}");
        assert!(
            semantic_text_role(name).is_some(),
            "published role {name} is not a mapped semantic role"
        );
        let source = format!("[roles.{name}]\nbold = true\n");
        let parsed = theme_schema::parse_theme(&source, "vocabulary", TerminalBackground::Unknown)
            .unwrap_or_else(|error| panic!("role {name}: {error}"));
        assert!(parsed.roles.contains_key(*name));
        let theme = load_theme_source_for(
            &source,
            "vocabulary",
            ThemeSource::CompiledDefault,
            "Vocabulary",
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            TerminalBackground::Unknown,
        )
        .unwrap_or_else(|error| panic!("role {name}: {error}"));
        assert!(
            theme.semantic_styles.contains_key(*name),
            "role {name} was not retained as a semantic style"
        );
    }

    // The extension-namespaced channel is open but still typed.
    let extension = "[roles.\"extension.git.branch\"]\nforeground = \"accent\"\n";
    let parsed =
        theme_schema::parse_theme(extension, "extension", TerminalBackground::Unknown).unwrap();
    assert!(parsed.roles.contains_key("extension.git.branch"));
    assert!(theme_schema::parse_theme(
        "[roles.\"private state\"]\nbold = true\n",
        "bad-role",
        TerminalBackground::Unknown,
    )
    .is_err());
}

#[test]
fn model_identity_prefers_creator_markers_over_compatible_endpoint() {
    assert_eq!(
        classify_model_identity("claude-sonnet-4-5", "claude-sonnet", "openai"),
        ModelLab::Anthropic
    );
    assert_eq!(
        classify_model_identity("qwen3-coder", "qwen3-coder", "openai-compatible"),
        ModelLab::Alibaba
    );
    assert_eq!(
        classify_model_identity("gpt-5.4", "gpt-5.4", "openai-codex"),
        ModelLab::OpenAi
    );
    assert_eq!(
        classify_model_identity("deepseek-v4-pro", "deepseek-v4-pro", "deepseek"),
        ModelLab::DeepSeek
    );
    assert_eq!(
        classify_model_identity("custom", "custom", "xai"),
        ModelLab::XAi
    );
    assert_eq!(
        classify_model_identity("custom", "custom", "meta"),
        ModelLab::Meta
    );
}

#[test]
fn source_colors_track_recognizable_lab_chart_colors() {
    assert_eq!(ModelLab::OpenAi.source_color(), Some("#1f1f1f"));
    assert_eq!(ModelLab::Anthropic.source_color(), Some("#cc785c"));
    assert_eq!(ModelLab::Google.source_color(), Some("#34a853"));
    assert_eq!(ModelLab::DeepSeek.source_color(), Some("#2243e6"));
    assert_eq!(ModelLab::Mistral.source_color(), Some("#fd6f00"));
}

#[test]
fn diff_rows_use_standard_background_surfaces_and_normal_foregrounds() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    for (background, expected_add, expected_remove) in [
        (TerminalBackground::Dark, "#10261e", "#2a171b"),
        (TerminalBackground::Light, "#e8f6ee", "#fcebed"),
    ] {
        let theme = default_theme_for(background, capabilities);
        assert_eq!(
            theme.resolve::<String>("diff_added").as_deref(),
            Some("default")
        );
        assert_eq!(
            theme.resolve::<String>("diff_removed").as_deref(),
            Some("default")
        );
        assert_eq!(
            theme.resolve::<String>("diff_added_bg").as_deref(),
            Some(expected_add)
        );
        assert_eq!(
            theme.resolve::<String>("diff_removed_bg").as_deref(),
            Some(expected_remove)
        );
    }

    let unknown = default_theme_for(TerminalBackground::Unknown, capabilities);
    assert_eq!(
        unknown.resolve::<String>("diff_added_bg").as_deref(),
        Some("default")
    );
    assert_eq!(
        unknown.resolve::<String>("diff_removed_bg").as_deref(),
        Some("default")
    );
}

fn required_rgb_token(theme: &OctetTheme, token: &str) -> Rgb {
    let value = theme
        .resolve::<String>(token)
        .unwrap_or_else(|| panic!("missing token {token}"));
    parse_hex_color(&value).unwrap_or_else(|| panic!("{token} was not RGB: {value}"))
}

#[test]
fn standard_syntax_palette_contrasts_with_diff_surfaces() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    let syntax_tokens = STANDARD_SYNTAX_COLORS
        .iter()
        .map(|(token, _, _)| *token)
        .chain(["diff_added_marker", "diff_removed_marker"]);

    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = default_theme_for(background, capabilities);
        for surface in ["diff_added_bg", "diff_removed_bg"] {
            let surface_color = required_rgb_token(&theme, surface);
            for token in syntax_tokens.clone() {
                let foreground = required_rgb_token(&theme, token);
                assert!(
                    contrast(foreground, surface_color) >= 4.5,
                    "{:?} {:?} {token} on {surface}",
                    theme.source(),
                    background
                );
            }
        }
    }
}

#[test]
fn ansi256_diff_surfaces_preserve_syntax_contrast_and_distinction() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::Ansi256);
    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = default_theme_for(background, capabilities);
        let quantized = |token| ansi256_rgb(nearest_ansi256(required_rgb_token(&theme, token)));
        for added in [
            required_rgb_token(&theme, "diff_added_marker"),
            quantized("diff_added_marker"),
        ] {
            assert!(
                added.green > added.red && added.green > added.blue,
                "{background:?}: added marker must remain green: {added:?}"
            );
        }
        // Subtle surfaces may quantize together; signed markers retain distinction.
        assert_ne!(
            quantized("diff_added_marker"),
            quantized("diff_removed_marker")
        );
        for surface in ["diff_added_bg", "diff_removed_bg"] {
            for token in STANDARD_SYNTAX_COLORS
                .iter()
                .map(|(token, _, _)| *token)
                .chain(["diff_added_marker", "diff_removed_marker"])
            {
                assert!(
                    contrast(quantized(token), quantized(surface)) >= 4.5,
                    "{background:?}: {token} on {surface}"
                );
            }
        }
    }
}

#[test]
fn unknown_background_uses_no_fixed_diff_surfaces_and_universal_syntax() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    let theme = default_theme_for(TerminalBackground::Unknown, capabilities);
    let black = Rgb {
        red: 0,
        green: 0,
        blue: 0,
    };
    let white = Rgb {
        red: 255,
        green: 255,
        blue: 255,
    };

    for surface in ["diff_added_bg", "diff_removed_bg"] {
        assert_eq!(theme.resolve::<String>(surface).as_deref(), Some("default"));
    }
    for token in STANDARD_SYNTAX_COLORS
        .iter()
        .map(|(token, _, _)| *token)
        .chain(["diff_added_marker", "diff_removed_marker"])
    {
        let foreground = required_rgb_token(&theme, token);
        assert!(contrast(foreground, black) >= 4.5, "{token} on black");
        assert!(contrast(foreground, white) >= 4.5, "{token} on white");
    }
}

#[test]
fn composer_idle_border_moves_toward_the_terminal_background() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    let accent = (96, 80, 64);
    let dark = default_theme_for(TerminalBackground::Dark, capabilities);
    let light = default_theme_for(TerminalBackground::Light, capabilities);
    let unknown = default_theme_for(TerminalBackground::Unknown, capabilities);

    assert_eq!(dark.composer_idle_rgb(accent), (12, 10, 8));
    assert_eq!(light.composer_idle_rgb(accent), (236, 234, 232));
    assert_eq!(unknown.composer_idle_rgb(accent), (124, 122, 120));
}

#[test]
fn terminal_theme_choices_are_builtin_and_override_detection() {
    assert_eq!(
        TerminalThemeChoice::all()
            .into_iter()
            .map(TerminalThemeChoice::label)
            .collect::<Vec<_>>(),
        vec!["Auto (recommended)", "Light terminal", "Dark terminal"]
    );
    assert_eq!(
        TerminalThemeChoice::parse("LIGHT"),
        Some(TerminalThemeChoice::Light)
    );
    assert_eq!(TerminalThemeChoice::parse("custom"), None);
    assert_eq!(TerminalThemeChoice::parse("compact"), None);

    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path().to_owned());
    config.theme = Some("dark".to_owned());
    assert_eq!(
        load_theme_for_background(&config, TerminalBackground::Light).background,
        TerminalBackground::Dark
    );
    config.theme = Some("light".to_owned());
    assert_eq!(
        load_theme_for_background(&config, TerminalBackground::Dark).background,
        TerminalBackground::Light
    );
    config.theme = Some("auto".to_owned());
    assert_eq!(
        load_theme_for_background(&config, TerminalBackground::Unknown).background,
        TerminalBackground::Unknown
    );
}

#[test]
fn openai_idle_border_does_not_quantize_to_black_on_light_profiles() {
    for color in [
        ColorDepth::Ansi16,
        ColorDepth::Ansi256,
        ColorDepth::TrueColor,
    ] {
        let capabilities = TerminalCapabilities::test(true, true, color);
        let mut theme = default_theme_for(TerminalBackground::Light, capabilities);
        apply_model_lab_for(&mut theme, ModelLab::OpenAi, TerminalBackground::Light);
        let accent = theme.role_rgb("model_accent").expect("model accent");
        let idle = theme.composer_idle_rgb(accent);
        assert!(idle.0 > accent.0 && idle.1 > accent.1 && idle.2 > accent.2);

        let rendered = theme.rgb_fg(idle, "─");
        assert!(!rendered.contains("\x1b[30m"), "{color:?}: {rendered:?}");
        assert!(!rendered.contains("38;5;0m"), "{color:?}: {rendered:?}");
    }
}

#[test]
fn active_lab_populates_the_dedicated_model_token() {
    let mut theme = test_theme();
    apply_model_lab_for(&mut theme, ModelLab::OpenAi, TerminalBackground::Unknown);
    assert_eq!(
        theme.resolve::<String>("model_accent").as_deref(),
        Some("#767676")
    );
    apply_model_lab_for(&mut theme, ModelLab::Anthropic, TerminalBackground::Unknown);
    assert_eq!(
        theme.resolve::<String>("model_accent").as_deref(),
        Some("#a9634c")
    );
    assert_eq!(
        theme.resolve::<String>("model_assistant").as_deref(),
        Some("default")
    );
}

#[test]
fn rich_text_roles_are_semantic_not_model_accent() {
    let mut theme = test_theme();
    apply_model_lab_for(&mut theme, ModelLab::Anthropic, TerminalBackground::Unknown);
    let renderer = theme.rich_renderer();
    let accent = sexy_tui_rs::Color::Rgb(169, 99, 76);
    // Semantic roles in the rich renderer use their own colour tokens,
    // NOT the model accent. Only octet's structural chrome uses model colors.
    for role in [
        TextRole::Heading,
        TextRole::ListMarker,
        TextRole::InlineCode,
        TextRole::Border,
        TextRole::Code,
        TextRole::Link,
        TextRole::Emphasis,
        TextRole::Strong,
    ] {
        assert_ne!(
            renderer.theme().style(role).foreground,
            accent,
            "{role:?} must not use the model accent"
        );
    }
}

#[test]
fn named_theme_roles_survive_without_model_palette_opt_in() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::Ansi16);
    let mut theme = OctetTheme::new(
        SexyTheme::load(None, CapabilityTier::Baseline),
        capabilities,
        TerminalBackground::Unknown,
    );
    theme.override_token("accent", "#005f5f");
    theme.override_token("assistant_msg_text", "#7a3e65");
    apply_model_lab_for(&mut theme, ModelLab::Anthropic, TerminalBackground::Unknown);
    assert_eq!(
        theme.resolve::<String>("model_accent"),
        Some(balance_foreground("#005f5f", TerminalBackground::Unknown))
    );
    assert_eq!(
        theme.resolve::<String>("model_assistant"),
        Some(balance_foreground("#7a3e65", TerminalBackground::Unknown))
    );
}

#[test]
fn balanced_lab_colors_have_terminal_safe_contrast() {
    let labs = [
        ModelLab::OpenAi,
        ModelLab::Anthropic,
        ModelLab::Google,
        ModelLab::XAi,
        ModelLab::Meta,
        ModelLab::Mistral,
        ModelLab::DeepSeek,
        ModelLab::Alibaba,
        ModelLab::MiniMax,
        ModelLab::Kimi,
        ModelLab::ZAi,
        ModelLab::Nvidia,
        ModelLab::Xiaomi,
        ModelLab::Cohere,
        ModelLab::Amazon,
        ModelLab::Microsoft,
        ModelLab::Ai21,
        ModelLab::ByteDance,
        ModelLab::Perplexity,
        ModelLab::Ibm,
        ModelLab::Baidu,
        ModelLab::Tencent,
        ModelLab::AllenAi,
    ];
    let black = Rgb {
        red: 0,
        green: 0,
        blue: 0,
    };
    let white = Rgb {
        red: 255,
        green: 255,
        blue: 255,
    };
    let dark = Rgb {
        red: 18,
        green: 20,
        blue: 22,
    };
    let light = Rgb {
        red: 250,
        green: 250,
        blue: 250,
    };

    for lab in labs {
        let source = lab.source_color().unwrap();
        let universal =
            parse_hex_color(&balance_foreground(source, TerminalBackground::Unknown)).unwrap();
        assert!(contrast(universal, black) >= 4.5, "{lab:?} on black");
        assert!(contrast(universal, white) >= 4.5, "{lab:?} on white");

        let dark_color =
            parse_hex_color(&balance_foreground(source, TerminalBackground::Dark)).unwrap();
        assert!(contrast(dark_color, dark) >= 5.5, "{lab:?} on dark");
        assert!(
            contrast(ansi256_rgb(nearest_ansi256(dark_color)), dark) >= 4.5,
            "{lab:?} quantized on dark: {dark_color:?} -> {:?}",
            ansi256_rgb(nearest_ansi256(dark_color))
        );

        let light_color =
            parse_hex_color(&balance_foreground(source, TerminalBackground::Light)).unwrap();
        assert!(contrast(light_color, light) >= 5.5, "{lab:?} on light");
        assert!(
            contrast(ansi256_rgb(nearest_ansi256(light_color)), light) >= 4.5,
            "{lab:?} quantized on light"
        );
    }
}

#[test]
fn ansi256_semantic_surfaces_and_model_accents_use_fixed_palette() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::Ansi256);
    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        let mut theme = default_theme_for(background, capabilities);
        for lab in [
            ModelLab::OpenAi,
            ModelLab::Anthropic,
            ModelLab::Google,
            ModelLab::Alibaba,
            ModelLab::Kimi,
        ] {
            apply_model_lab_for(&mut theme, lab, background);
            let mut samples = theme
                .semantic_styles
                .keys()
                .map(|role| theme.fg(role, "sample"))
                .collect::<Vec<_>>();
            samples.push(theme.model_fg(Some(lab), "sample"));
            samples.push(theme.prompt_color_cell(lab.source_color(), "sample"));
            for (color, _) in ANSI16 {
                // These exact RGBs selected theme-owned slots before #382.
                samples.push(theme.color_text(color, "sample"));
            }
            for sample in samples {
                assert_eq!(sexy_tui_rs::strip_terminal_sequences(&sample), "sample");
                assert!(
                    !sample.contains("38;2;") && !sample.contains("48;2;"),
                    "{sample:?}"
                );
                for escape in sample.split("\x1b[").skip(1) {
                    let Some((sgr, _)) = escape.split_once('m') else {
                        continue;
                    };
                    let codes = sgr.split(';').collect::<Vec<_>>();
                    for triple in codes.windows(3) {
                        if matches!(triple[0], "38" | "48") && triple[1] == "5" {
                            let index: u8 = triple[2].parse().unwrap();
                            assert!(index >= 16, "{background:?}/{lab:?}: {sample:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn styling_degrades_without_changing_text() {
    let plain = test_theme_with(TerminalCapabilities::test(false, false, ColorDepth::None));
    assert_eq!(plain.fg("model_accent", "octet"), "octet");
    assert_eq!(plain.bold("octet"), "octet");
    assert_eq!(plain.dim("octet"), "octet");

    let ansi16 = test_theme_with(TerminalCapabilities::test(true, true, ColorDepth::Ansi16));
    let styled = ansi16.fg("model_accent", "octet");
    assert!(styled.starts_with("\x1b["));
    let dimmed = ansi16.dim("octet");
    assert_ne!(dimmed, "octet");
    assert!(!dimmed.contains("\x1b[2m"));
    assert!(!styled.contains("38;2;"));
    assert!(!styled.contains("38;5;"));

    let ansi256 = test_theme_with(TerminalCapabilities::test(true, true, ColorDepth::Ansi256));
    assert!(ansi256.fg("model_accent", "octet").contains("38;5;"));

    let truecolor = test_theme_with(TerminalCapabilities::test(
        true,
        true,
        ColorDepth::TrueColor,
    ));
    assert!(truecolor.fg("model_accent", "octet").contains("38;2;"));
}

#[test]
fn prompt_colors_follow_the_lab_palette_instead_of_exact_model_aliases() {
    let openai = prompt_color_for_lab(ModelLab::OpenAi);
    assert_eq!(openai, prompt_color_for_model_id(" GPT-5.6 "));
    assert_eq!(openai, prompt_color_for_model_id("gpt-5.6-sol"));
    assert_eq!(openai, prompt_color_for_model_id("openai/codex-mini"));

    let deepseek = prompt_color_for_lab(ModelLab::DeepSeek);
    assert_eq!(
        deepseek,
        prompt_color_for_model_id("opencode/deepseek-v4-flash-free")
    );
    assert_eq!(deepseek, prompt_color_for_model_id("deepseek-v4-pro"));
    assert_ne!(openai, deepseek);

    for color in [openai, deepseek] {
        assert_eq!(color.len(), 7);
        assert!(color.starts_with('#'));
        assert!(color[1..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}

#[test]
fn every_recognized_model_family_uses_its_lab_prompt_color() {
    for (model_id, lab) in [
        ("gpt-5.6-terra", ModelLab::OpenAi),
        ("claude-opus-4.5", ModelLab::Anthropic),
        ("gemini-3-pro", ModelLab::Google),
        ("grok-4", ModelLab::XAi),
        ("llama-4", ModelLab::Meta),
        ("mistral-large", ModelLab::Mistral),
        ("deepseek-v4", ModelLab::DeepSeek),
        ("qwen3-coder", ModelLab::Alibaba),
        ("minimax-m2", ModelLab::MiniMax),
        ("kimi-k2", ModelLab::Kimi),
        ("glm-5", ModelLab::ZAi),
        ("nemotron-4", ModelLab::Nvidia),
        ("mimo-v2", ModelLab::Xiaomi),
        ("command-r-plus", ModelLab::Cohere),
        ("nova-pro", ModelLab::Amazon),
        ("phi-4", ModelLab::Microsoft),
        ("jamba-large", ModelLab::Ai21),
        ("doubao-pro", ModelLab::ByteDance),
        ("sonar-pro", ModelLab::Perplexity),
        ("granite-4", ModelLab::Ibm),
        ("ernie-5", ModelLab::Baidu),
        ("hunyuan-t1", ModelLab::Tencent),
        ("olmo-3", ModelLab::AllenAi),
    ] {
        assert_eq!(
            prompt_color_for_model_id(model_id),
            prompt_color_for_lab(lab),
            "{model_id} did not use the {lab:?} prompt color"
        );
    }
}

#[test]
fn settled_event_dots_keep_full_strength_signal_colors() {
    let theme = test_theme_with(TerminalCapabilities::test(
        true,
        true,
        ColorDepth::TrueColor,
    ));

    let success = theme.settled_event_dot("success", "•");
    let error = theme.settled_event_dot("error", "•");
    assert!(success.contains("\x1b[38;2;82;200;116m"), "{success:?}");
    assert!(error.contains("\x1b[38;2;230;83;83m"), "{error:?}");
}

#[test]
fn exact_prompt_marker_degrades_without_painting_or_emitting_unsafe_data() {
    let plain = test_theme_with(TerminalCapabilities::test(false, false, ColorDepth::None));
    assert_eq!(plain.prompt_color_marker(Some("#123456"), "> "), "> ");

    let ansi16 = test_theme_with(TerminalCapabilities::test(true, true, ColorDepth::Ansi16));
    let rendered = ansi16.prompt_color_marker(Some("#123456"), "> ");
    assert!(rendered.contains("> "));
    assert!(rendered.contains("\x1b["));
    assert!(!rendered.contains("48;"));

    let ansi256 = test_theme_with(TerminalCapabilities::test(true, true, ColorDepth::Ansi256));
    let rendered = ansi256.prompt_color_marker(Some("#123456"), "> ");
    assert!(rendered.contains("38;5;"), "{rendered:?}");
    assert!(!rendered.contains("48;"));

    let truecolor = test_theme_with(TerminalCapabilities::test(
        true,
        true,
        ColorDepth::TrueColor,
    ));
    let rendered = truecolor.prompt_color_marker(Some("#123456"), "> ");
    assert!(rendered.contains("38;2;"), "{rendered:?}");
    assert!(!rendered.contains("48;"));
    assert_eq!(
        truecolor.prompt_color_marker(Some("#12\u{1b}3456"), "> "),
        "> "
    );
}

#[test]
fn ansi16_light_and_dark_balancing_never_paints_a_background() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::Ansi16);
    for background in [TerminalBackground::Light, TerminalBackground::Dark] {
        let mut theme = default_theme_for(background, capabilities);
        apply_model_lab_for(&mut theme, ModelLab::Anthropic, background);
        let rendered = theme.fg("model_accent", "model");
        assert!(rendered.contains("model"));
        assert!(!rendered.contains("48;"));
    }
}

#[test]
fn colorfgbg_explicit_override_and_osc_rgb_detect_backgrounds() {
    assert_eq!(
        background_from_colorfgbg("15;0"),
        Some(TerminalBackground::Dark)
    );
    assert_eq!(
        background_from_colorfgbg("0;15"),
        Some(TerminalBackground::Light)
    );
    assert_eq!(
        background_from_override("universal"),
        Some(TerminalBackground::Unknown)
    );
    assert_eq!(
        background_from_terminal_rgb(12, 18, 24),
        TerminalBackground::Dark
    );
    assert_eq!(
        background_from_terminal_rgb(240, 240, 240),
        TerminalBackground::Light
    );
}
