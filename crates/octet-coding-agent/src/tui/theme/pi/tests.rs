use super::super::{apply_model_lab, ColorDepth, ModelLab};
use super::*;
use sexy_tui_rs::Color;

fn oracle() -> Value {
    serde_json::from_str(include_str!("vectors.json")).unwrap()
}

fn input(case: &Value) -> SystemInput {
    serde_json::from_value(case["input"].clone()).unwrap()
}

fn case(name: &str) -> Value {
    oracle()["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()
        .clone()
}

fn rgb(value: &Value) -> Rgb {
    let color = value.as_str().unwrap().strip_prefix('#').unwrap();
    (
        u8::from_str_radix(&color[0..2], 16).unwrap(),
        u8::from_str_radix(&color[2..4], 16).unwrap(),
        u8::from_str_radix(&color[4..6], 16).unwrap(),
    )
}

fn native_color(value: &Value) -> Color {
    if let Some(index) = value.as_u64() {
        Color::Indexed(index as u8)
    } else if value.as_str() == Some("") {
        Color::Default
    } else {
        let (r, g, b) = rgb(value);
        Color::Rgb(r, g, b)
    }
}

fn resolved(input: SystemInput, generated: &SystemColors, token: &str) -> Rgb {
    let color = &generated.colors[token];
    if color == "" {
        if TOKENS
            .iter()
            .find(|candidate| candidate.name == token)
            .unwrap()
            .panel
        {
            input.background.unwrap()
        } else {
            input.foreground.unwrap()
        }
    } else {
        rgb(color)
    }
}

#[test]
fn system_recipe_matches_every_hash_verified_upstream_oracle_vector_exactly() {
    let vectors = oracle();
    assert_eq!(vectors["version"], "1.0.2");
    assert_eq!(vectors["vectors"].as_array().unwrap().len(), 119);
    for case in vectors["vectors"].as_array().unwrap() {
        let generated = generate(input(case));
        assert_eq!(generated.colors.len(), 56);
        assert_eq!(
            serde_json::to_value(&generated).unwrap(),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn native_projection_preserves_all_namespaced_colors_and_faint_flags() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    for case in oracle()["vectors"].as_array().unwrap() {
        let generated = generate(input(case));
        let theme = compile(capabilities, TerminalBackground::Unknown, &generated).unwrap();
        assert_eq!(theme.source(), &ThemeSource::CompiledPi);
        assert_eq!(theme.metadata().name, PI_THEME_NAME);
        assert!(!theme.metadata().adaptive);
        assert!(!theme.prompt_wash());
        assert!(!theme.uses_model_lab_color());
        for token in TOKENS {
            let style = theme.semantic_style(&format!("extension.pi.{}", token.name));
            let color = if token.panel {
                style.background
            } else {
                style.foreground
            };
            assert_eq!(
                color,
                native_color(&generated.colors[token.name]),
                "{} {}",
                case["name"],
                token.name
            );
            assert_eq!(
                style.attributes.dim,
                generated.dim.contains(&token.name),
                "{} {}",
                case["name"],
                token.name
            );
        }
        for &(pi, native) in NATIVE_FOREGROUNDS {
            assert_eq!(
                theme.resolve::<String>(native).unwrap(),
                native_value(&generated.colors[pi]),
                "{} {native}",
                case["name"]
            );
            assert_eq!(
                theme.semantic_style(native).foreground,
                native_color(&generated.colors[pi])
            );
            assert_eq!(
                theme.semantic_style(native).attributes.dim,
                generated.dim.contains(&pi)
            );
        }
        for (pi, native) in [
            ("selectedBg", "selected_bg"),
            ("userMessageBg", "user_msg_bg"),
            ("toolPendingBg", "tool_pending_bg"),
            ("toolSuccessBg", "tool_success_bg"),
            ("toolErrorBg", "tool_error_bg"),
        ] {
            assert_eq!(
                theme.resolve::<String>(native).unwrap(),
                native_value(&generated.colors[pi]),
                "{} {native}",
                case["name"]
            );
        }
    }
}

#[test]
fn indexed_fallback_keeps_terminal_default_backgrounds_and_closes_faint() {
    for depth in [
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
        ColorDepth::None,
    ] {
        let capabilities = TerminalCapabilities::test(true, true, depth);
        for background in [
            TerminalBackground::Unknown,
            TerminalBackground::Dark,
            TerminalBackground::Light,
        ] {
            // Explicitly absent replies stay indexed even when the terminal
            // reports an appearance hint. Native reference profiles are separate.
            let theme = pi_theme_with_colors(capabilities, background, None, None, None).unwrap();
            assert_eq!(theme.resolve::<String>("error").as_deref(), Some("index:1"));
            assert_eq!(
                theme.resolve::<String>("foreground").as_deref(),
                Some("default")
            );
            assert_eq!(
                theme.resolve::<String>("user_msg_bg").as_deref(),
                Some("default")
            );
            assert_eq!(
                theme.resolve::<String>("tool_pending_bg").as_deref(),
                Some("default")
            );
            assert!(!theme.semantic_style("foreground").attributes.dim);
            assert!(theme.semantic_style("muted").attributes.dim);
            assert!(
                theme
                    .semantic_style("extension.pi.scrollbarTrack")
                    .attributes
                    .dim
            );
            let emitted = theme.apply_semantic_role("extension.pi.muted", "x");
            if depth == ColorDepth::None {
                assert_eq!(emitted, "x");
                assert_eq!(theme.apply_semantic_role("extension.pi.error", "x"), "x");
                assert_eq!(theme.fg("extension.pi.muted", "x"), "x");
                assert_eq!(theme.fg("muted", "x"), "x");
                assert_eq!(theme.dim("x"), "x");
            } else {
                assert!(emitted.contains("\x1b[2m"), "{emitted:?}");
                let close = &emitted[emitted.find('x').unwrap() + 1..];
                assert!(
                    close.contains("22") || close.contains("\x1b[0m"),
                    "{emitted:?}"
                );
            }
        }
    }
}

#[test]
fn known_native_reference_profiles_use_pi_guessed_defaults_not_octet_colors() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    for (background, name) in [
        (TerminalBackground::Dark, "referenceDark"),
        (TerminalBackground::Light, "referenceLight"),
    ] {
        let expected = generate(input(&case(name)));
        let theme = pi_theme_for(capabilities, background).unwrap();
        assert_eq!(theme.background(), background);
        for token in TOKENS {
            let style = theme.semantic_style(&format!("extension.pi.{}", token.name));
            assert_eq!(
                if token.panel {
                    style.background
                } else {
                    style.foreground
                },
                native_color(&expected.colors[token.name])
            );
        }
        assert_eq!(
            theme.resolve::<String>("model_accent").unwrap(),
            native_value(&expected.colors["accent"])
        );
    }
    let unknown = pi_theme_for(capabilities, TerminalBackground::Unknown).unwrap();
    assert_eq!(
        unknown.resolve::<String>("error").as_deref(),
        Some("index:1")
    );
}

#[test]
fn compiled_pi_native_hook_supplies_reference_profiles_without_reported_rgb() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::Ansi256);
    let theme = pi_theme_for(capabilities, TerminalBackground::Unknown).unwrap();
    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let native = theme.for_native_background(background).unwrap();
        let reference = pi_theme_for(
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            background,
        )
        .unwrap();
        assert_eq!(
            native.resolve::<String>("accent"),
            reference.resolve::<String>("accent")
        );
        assert_eq!(
            native.resolve::<String>("user_msg_bg"),
            reference.resolve::<String>("user_msg_bg")
        );
        assert_eq!(native.source(), &ThemeSource::CompiledPi);
    }
}

#[test]
fn reported_colors_take_precedence_over_appearance_hint_and_survive_projection() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    for name in ["dracula", "solarizedLight", "backgroundOnly", "frappe"] {
        let input = input(&case(name));
        let generated = generate(input);
        let hint = if generated.appearance == Some(Appearance::Dark) {
            TerminalBackground::Light
        } else {
            TerminalBackground::Dark
        };
        let theme = pi_theme_with_colors(
            capabilities,
            hint,
            input.foreground,
            input.background,
            input.palette,
        )
        .unwrap();
        assert_eq!(
            theme.background(),
            generated.appearance.unwrap().background()
        );
        assert_eq!(
            theme.resolve::<String>("accent").unwrap(),
            native_value(&generated.colors["accent"])
        );
        assert_eq!(
            theme.resolve::<String>("diff_added").unwrap(),
            native_value(&generated.colors["toolDiffAdded"])
        );
        assert_eq!(
            theme.resolve::<String>("diff_removed").unwrap(),
            native_value(&generated.colors["toolDiffRemoved"])
        );
    }
}

#[test]
fn model_switches_do_not_rebalance_fixed_pi_accent_or_enable_prompt_wash() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    for name in ["dracula", "frappe", "solarizedLight", "indexedUnknown"] {
        let input = input(&case(name));
        let generated = generate(input);
        let mut theme = compile(capabilities, TerminalBackground::Unknown, &generated).unwrap();
        let accent = theme.resolve::<String>("model_accent");
        let text = theme.resolve::<String>("model_assistant");
        for lab in [
            ModelLab::OpenAi,
            ModelLab::Anthropic,
            ModelLab::Google,
            ModelLab::Unknown,
        ] {
            apply_model_lab(&mut theme, lab);
            assert_eq!(theme.resolve::<String>("model_accent"), accent);
            assert_eq!(theme.resolve::<String>("model_assistant"), text);
            assert_eq!(theme.model_rgb(Some(lab)), theme.role_rgb("model_accent"));
            assert!(!theme.prompt_wash());
            assert!(!theme.uses_model_lab_color());
        }
    }
}

#[test]
fn midgray_relaxation_and_body_text_repair_follow_upstream_best_effort_contrast() {
    for name in [
        "dracula",
        "solarizedLight",
        "backgroundOnly",
        "midGray",
        "gray117",
        "gray118",
        "gray119",
        "gray120",
        "gray127",
        "gray128",
        "gray137",
    ] {
        let input = input(&case(name));
        let generated = generate(input);
        for (token, surfaces) in [
            ("text", &["background", "selectedBg"][..]),
            ("userMessageText", &["userMessageBg"][..]),
            (
                "toolTitle",
                &["toolPendingBg", "toolSuccessBg", "toolErrorBg"][..],
            ),
        ] {
            let text = resolved(input, &generated, token);
            let background = |surface| {
                if surface == "background" {
                    input.background.unwrap()
                } else {
                    resolved(input, &generated, surface)
                }
            };
            // Pi's foreground-side and multi-surface constraints can miss 4.5
            // around midgray, even if the opposite extreme would pass (e.g.
            // gray117 toolTitle on toolSuccessBg = 4.3862509175895505).
            // Qualify parity against independently generated oracle colors,
            // not an accessibility guarantee absent from upstream's contract.
            let expected = case(name);
            let oracle_rgb = |token: &str| {
                let value = &expected["expected"]["colors"][token];
                if value == "" {
                    input.foreground.unwrap()
                } else {
                    rgb(value)
                }
            };
            for &surface in surfaces {
                let oracle_background = if surface == "background" {
                    input.background.unwrap()
                } else {
                    oracle_rgb(surface)
                };
                let measured = contrast(text, background(surface));
                let oracle_contrast = contrast(oracle_rgb(token), oracle_background);
                assert_eq!(measured, oracle_contrast, "{name} {token} on {surface}");
                if !matches!(name, "gray117" | "gray118" | "gray119" | "gray120") {
                    assert!(measured >= 4.5, "{name} {token} on {surface}: {measured}");
                }
            }
        }
    }
}

#[test]
fn reference_foreground_order_and_panel_contrast_match_upstream_expectations() {
    for name in ["dracula", "solarizedLight", "backgroundOnly", "midGray"] {
        let input = input(&case(name));
        let generated = generate(input);
        let background = input.background.unwrap();
        let offset = |token| lightness(resolved(input, &generated, token)) - lightness(background);
        if name != "midGray" {
            assert!(offset("text").abs() > offset("muted").abs(), "{name}");
            assert!(offset("muted").abs() > offset("dim").abs(), "{name}");
        }
        for token in [
            "userMessageBg",
            "toolPendingBg",
            "toolSuccessBg",
            "toolErrorBg",
            "selectedBg",
        ] {
            assert!(
                contrast(resolved(input, &generated, token), background) < 2.0,
                "{name} {token}"
            );
            assert_eq!(
                offset(token) > 0.0,
                generated.appearance == Some(Appearance::Dark)
            );
        }
    }
}

#[test]
fn catppuccin_frappe_chroma_cap_prevents_pastel_accent_from_doubling_chroma() {
    let input = input(&case("frappe"));
    let generated = generate(input);
    let pink = input.palette.unwrap()[5];
    let accent = resolved(input, &generated, "accent");
    assert!(lightness(accent) < lightness(pink) - 0.05);
    assert!(colors::rgb_to_oklch(accent)[1] <= colors::rgb_to_oklch(pink)[1] * 1.03);
    for panel in ["userMessageBg", "customMessageBg"] {
        assert!(colors::rgb_to_oklch(resolved(input, &generated, panel))[1] <= 0.1);
    }
    assert_eq!(
        serde_json::to_value(&generated).unwrap(),
        case("frappe")["expected"]
    );
}

#[test]
fn palette_slot_exceptions_are_preserved_in_indexed_fallback() {
    let generated = generate(SystemInput::default());
    for (token, slot) in [
        ("syntaxString", 2),
        ("syntaxNumber", 5),
        ("searchMatchBg", 3),
    ] {
        if token.ends_with("Bg") {
            assert_eq!(generated.colors[token], "");
        } else {
            assert_eq!(generated.colors[token], slot);
        }
    }
    assert_eq!(generated.colors["thinkingXhigh"], 13);
    assert_eq!(generated.colors["thinkingMax"], 1);
}

#[test]
fn grayscale_clamping_and_palette_without_background_follow_upstream_tiers() {
    let mut input = input(&case("dracula"));
    input.saturation = 0.0;
    let generated = generate(input);
    assert!(colors::rgb_to_oklch(rgb(&generated.colors["error"]))[1] < 0.005);
    for name in ["foregroundWithoutBackground", "paletteWithoutBackground"] {
        assert_eq!(
            serde_json::to_value(generate(self::input(&case(name)))).unwrap(),
            case("indexedUnknown")["expected"]
        );
    }
    assert_eq!(
        case("draculaSaturation-0.5")["expected"],
        case("draculaSaturation0")["expected"]
    );
    assert_eq!(
        case("draculaSaturation2")["expected"],
        case("draculaSaturation1")["expected"]
    );
}
