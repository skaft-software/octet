use super::*;
use crate::tui::view::InteractiveShell;
use sexy_tui_rs::strip_terminal_sequences;

#[test]
fn default_welcome_keeps_exit_help_quieter_than_identity() {
    let shell = InteractiveShell::test_shell();
    let state = shell.state.borrow();
    for width in [24, 36, 37, 80] {
        for budget in [3, 7, 10] {
            let rows = render_welcome_content(&state, width, budget);
            let hint = rows.iter().find(|row| row.contains("Ctrl+C")).unwrap();
            assert!(hint.contains(&state.theme.dim("Ctrl+C / Ctrl+D to exit")));
            assert!(
                !hint.contains("\x1b[1m"),
                "help must not compete with identity: {hint:?}"
            );
            assert!(rows
                .iter()
                .any(|row| row.contains(&state.theme.bold("octet"))));
        }
    }
}

#[test]
fn placeholder_geometry_matches_materialized_rows() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    let mut themes = Vec::new();
    for color in [
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
        ColorDepth::None,
    ] {
        for unicode in [true, false] {
            themes.push(crate::tui::theme::test_theme_with(
                TerminalCapabilities::test(true, unicode, color),
            ));
        }
    }
    for source in [
        include_str!("../../../../../examples/themes/Cards.toml"),
        include_str!("../../../../../examples/themes/Still.toml"),
        "[metadata]\nname = 'Plain'\n",
        "[colors]\nsplash = '#d97757'\nsplash_box = '#d97757'\n",
        "[colors]\nsplash_compact = true\nsplash_box = '#d97757'\n",
        "[colors]\nsplash_compact = true\n",
        "startup = ' pI '\n",
        "[colors]\ncontent_max_width = 32\n[layout]\ntranscript_inset = 4\ncomposer_padding = 3\nshow_header = true\n",
    ] {
        themes.push(crate::tui::theme::test_theme_from_source(source));
    }
    for (theme_index, theme) in themes.into_iter().enumerate() {
        let shell = InteractiveShell::test_shell_with_theme(theme);
        for safe_mode in [false, true] {
            for no_model in [false, true] {
                let mut state = shell.state.borrow_mut();
                state.safe_mode = safe_mode;
                state.model = if no_model { "" } else { "custom/probe" }.into();
                state.model_display = state.model.clone();
                state.available_update = Some(semver::Version::new(9, 8, 7));
                // Include both sides of the logo/text and compact breakpoints.
                for width in [
                    0, 1, 2, 4, 7, 8, 9, 12, 16, 23, 24, 29, 30, 31, 32, 33, 34, 35, 39, 40, 45,
                    46, 47, 48, 60, 71, 72, 80, 112, 120, 160,
                ] {
                    let content_width = PresentationLayout::new(&state.theme, width).content_width;
                    for budget in 0..=10 {
                        assert_eq!(
                            welcome_content_rows(&state, content_width, budget),
                            render_welcome_content(&state, content_width, budget).len(),
                            "theme={theme_index} width={width} budget={budget} safe={safe_mode} no_model={no_model}",
                        );
                    }
                }
                state.overlay = Some(crate::tui::view::ShellOverlay::Text("overlay".into()));
                for budget in [0, 1, 6, 7, 8, 10] {
                    assert_eq!(welcome_content_rows(&state, 80, budget), 0);
                    assert!(render_welcome_content(&state, 80, budget).is_empty());
                }
                state.overlay = None;
            }
        }
    }
}

#[test]
fn placeholder_seam_matches_the_admitted_welcome_without_materializing_it() {
    for theme in [
        crate::tui::theme::test_theme(),
        crate::tui::theme::test_theme_from_source("startup = 'pi'\n"),
        crate::tui::theme::test_theme_from_source(include_str!(
            "../../../../../examples/themes/Cards.toml"
        )),
        crate::tui::theme::test_theme_from_source(include_str!(
            "../../../../../examples/themes/Still.toml"
        )),
    ] {
        let shell = InteractiveShell::test_shell_with_theme(theme);
        let mut state = shell.state.borrow_mut();
        state.startup_card_started_at = Some(Instant::now());
        for width in [1, 7, 24, 48, 80, 160] {
            for height in [0, 3, 8, 18, 40] {
                state.size = (width, height);
                for composer_rows in [1, 4, 8] {
                    let budget = welcome_row_budget_for_composer(&state, width, composer_rows);
                    let rendered = render_welcome_card(&state, width, budget, Instant::now());
                    let expected = usize::from(rendered.last().is_some_and(|line| {
                        !sexy_tui_rs::strip_terminal_sequences(line)
                            .trim()
                            .is_empty()
                    }));
                    reset_placeholder_work_counts();
                    assert_eq!(
                        welcome_placeholder_separator_rows(&state, width, composer_rows),
                        expected,
                        "width={width} height={height} composer={composer_rows}"
                    );
                    assert_eq!(placeholder_work_counts(), (0, 0));
                }
            }
        }
    }
}

#[test]
fn placeholder_budget_matches_materializer_without_invisible_work() {
    let shell = InteractiveShell::test_shell();
    shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    for width in [1, 2, 7, 8, 23, 32, 46, 80, 160] {
        for height in [0, 1, 3, 5, 7, 8, 10, 11, 12, 16, 24, 40] {
            for composer_rows in [1, 2, 4, 8, 20] {
                let mut state = shell.state.borrow_mut();
                state.size = (width, height);
                let budget = welcome_row_budget_for_composer(&state, width, composer_rows);
                let expected = render_welcome_card(&state, width, budget, Instant::now()).len();
                reset_placeholder_work_counts();
                assert_eq!(
                    welcome_placeholder_rows(&state, width, composer_rows),
                    expected
                );
                assert_eq!(placeholder_work_counts(), (0, 0));
            }
            let state = shell.state.borrow();
            let budget = welcome_row_budget(&state, width);
            let expected = render_welcome_card(&state, width, budget, Instant::now()).len();
            let composer_rows = crate::tui::composer_surface::render_composer_surface(
                &state,
                width,
                Instant::now(),
            )
            .len();
            reset_placeholder_work_counts();
            assert_eq!(
                welcome_placeholder_rows(&state, width, composer_rows),
                expected
            );
            assert_eq!(placeholder_work_counts(), (0, 0));
        }
    }
}

#[test]
fn workspace_is_footer_only_across_startup_layouts() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    const WORKSPACE: &str = "/work/splash-path-regression";
    for color in [ColorDepth::TrueColor, ColorDepth::Ansi16, ColorDepth::None] {
        for custom in [false, true] {
            let mut theme =
                crate::tui::theme::test_theme_with(TerminalCapabilities::test(true, true, color));
            if custom {
                theme.override_token("splash", "#d97757");
                theme.override_token("splash_box", "#d97757");
            }
            let mut shell = InteractiveShell::test_shell_with_theme(theme);
            shell.set_workspace(std::path::PathBuf::from(WORKSPACE));
            shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
            for update in [None, Some(semver::Version::new(9, 8, 7))] {
                shell.state.borrow_mut().available_update = update;
                for width in [4, 16, 46, 80, 120] {
                    let rows =
                        render_welcome_card(&shell.state.borrow(), width, 10, Instant::now());
                    let plain = strip_terminal_sequences(&rows.join("\n"));
                    assert!(!plain.contains("/work"), "{plain}");
                    assert!(!plain.contains("splash-path"), "{plain}");
                    assert!(!plain.contains("workspace unavailable"), "{plain}");
                }
            }
            let surface = crate::tui::composer_surface::render_composer_surface(
                &shell.state.borrow(),
                120,
                Instant::now(),
            );
            let footer = strip_terminal_sequences(surface.last().unwrap());
            assert!(footer.ends_with(WORKSPACE), "{footer}");
        }
    }
}

#[test]
fn changelog_splash_tip_stays_bounded_without_moving_the_byte() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    for color in [
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
        ColorDepth::None,
    ] {
        for unicode in [true, false] {
            let mut shell =
                InteractiveShell::test_shell_with_theme(crate::tui::theme::test_theme_with(
                    TerminalCapabilities::test(true, unicode, color),
                ));
            shell.set_identity("custom", "custom/probe", "off");
            shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
            for width in [1, 4, 8, 16, 23, 24, 40, 46, 80, 120] {
                for height in [7, 8, 10] {
                    let rendered =
                        render_welcome_card(&shell.state.borrow(), width, height, Instant::now());
                    assert!(rendered.len() <= height);
                    assert!(rendered
                        .iter()
                        .all(|line| sexy_tui_rs::visible_width(line) <= usize::from(width)));
                    let plain = rendered
                        .iter()
                        .map(|line| strip_terminal_sequences(line))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if width >= 46 && height >= 8 {
                        assert!(
                            plain.contains(if unicode {
                                "/changelog · what's new"
                            } else {
                                "/changelog - what's new"
                            }),
                            "missing exact splash tip at {width}x{height}: {plain}"
                        );
                    } else if width >= 16 && height >= 8 {
                        assert!(
                            plain.contains("/changelog"),
                            "command hidden at {width}x{height}: {plain}"
                        );
                    }
                    if color == ColorDepth::None {
                        assert!(!rendered.join("\n").contains('\x1b'));
                    }
                }
            }
        }
    }
}

#[test]
fn welcome_exit_hint_includes_ctrl_c_across_layouts() {
    for theme in [
        crate::tui::theme::test_theme(),
        crate::tui::theme::test_theme_from_source("[metadata]\nname = 'Plain'\n"),
        crate::tui::theme::test_theme_from_source(
            "[colors]\nsplash = '#d97757'\nsplash_box = '#d97757'\n",
        ),
    ] {
        let shell = InteractiveShell::test_shell_with_theme(theme);
        for (width, budget) in [(24, 3), (24, 7), (80, 10), (160, 10)] {
            let rows = render_welcome_content(&shell.state.borrow(), width, budget);
            let plain = strip_terminal_sequences(&rows.join("\n"));
            assert!(
                plain.contains("Ctrl+C / Ctrl+D to exit"),
                "{width}/{budget}: {plain}"
            );
            assert!(rows
                .iter()
                .all(|row| sexy_tui_rs::visible_width(row) <= usize::from(width)));
        }
    }
}

#[test]
fn welcome_bytes_are_static_and_short_panes_keep_permissions() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    for color in [ColorDepth::TrueColor, ColorDepth::Ansi16, ColorDepth::None] {
        let mut shell = InteractiveShell::test_shell_with_theme(
            crate::tui::theme::test_theme_with(TerminalCapabilities::test(true, true, color)),
        );
        shell.set_identity("custom", "custom/probe", "off");
        let started = Instant::now();
        shell.state.borrow_mut().startup_card_started_at = Some(started);
        for width in [24, 48, 80] {
            for budget in [1, 2, 3, 6, 7, 10] {
                let state = shell.state.borrow();
                let first = render_welcome_card(&state, width, budget, started);
                assert_eq!(
                    first,
                    render_welcome_card(
                        &state,
                        width,
                        budget,
                        started + std::time::Duration::from_secs(3)
                    )
                );
                assert!(first.len() <= budget);
                let plain = strip_terminal_sequences(&first.join("\n"));
                assert!(plain.contains("full access"), "{width}/{budget}: {plain}");
                assert!(first
                    .iter()
                    .all(|row| sexy_tui_rs::visible_width(row) <= usize::from(width)));
                if color == ColorDepth::None {
                    assert!(!first.join("").contains('\x1b'));
                }
            }
        }
        shell.state.borrow_mut().model.clear();
        shell.state.borrow_mut().available_update = Some(semver::Version::new(9, 8, 7));
        let rows = render_welcome_card(&shell.state.borrow(), 48, 3, started);
        let plain = strip_terminal_sequences(&rows.join("\n"));
        assert!(plain.contains("No models configured"), "{plain}");
        assert!(plain.contains("full access"), "{plain}");
    }
}

#[test]
fn short_welcome_keeps_permissions_at_current_version_width_breakpoints() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    for color in [ColorDepth::TrueColor, ColorDepth::Ansi16, ColorDepth::None] {
        let mut shell = InteractiveShell::test_shell_with_theme(
            crate::tui::theme::test_theme_with(TerminalCapabilities::test(true, true, color)),
        );
        shell.set_identity("custom", "custom/probe", "off");
        let started = Instant::now();
        let mut state = shell.state.borrow_mut();
        state.startup_card_started_at = Some(started);
        for safe_mode in [false, true] {
            state.safe_mode = safe_mode;
            let access = if safe_mode {
                "safe mode"
            } else {
                "full access"
            };
            let full = format!("octet {version} {access}");
            let compact = format!("octet {access}");
            for width in [
                access.len() as u16,
                compact.len() as u16 - 1,
                compact.len() as u16,
                24, // v0.10.0 plus full access is 25 cells, not 24.
                full.len() as u16 - 1,
                full.len() as u16,
            ] {
                let expected = if usize::from(width) >= full.len() {
                    full.as_str()
                } else if usize::from(width) >= compact.len() {
                    compact.as_str()
                } else {
                    access
                };
                for budget in 1..7 {
                    let rows = render_welcome_card(&state, width, budget, started);
                    let identity = strip_terminal_sequences(&rows[0]);
                    assert_eq!(identity, expected, "width={width} budget={budget}");
                    assert!(identity.contains(access), "{identity}");
                    assert_eq!(rows.len(), budget.min(3));
                    assert_eq!(welcome_content_rows(&state, width, budget), rows.len());
                    assert!(rows
                        .iter()
                        .all(|line| sexy_tui_rs::visible_width(line) <= usize::from(width)));
                    if color == ColorDepth::None {
                        assert!(!rows.join("").contains('\x1b'));
                    }
                }
            }
        }
    }
}

#[test]
fn late_updates_keep_custom_theme_geometry_even_at_card_breakpoints() {
    let mut shell = InteractiveShell::test_shell_with_theme(crate::tui::theme::test_theme_from_source(
        "[metadata]\nname = \"Small welcome fixture\"\n[colors]\nsplash = \"#d97757\"\nsplash_box = \"#d97757\"\n",
    ));
    shell.set_identity("custom", "custom/probe", "off");
    shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    for width in 24..=80 {
        shell.state.borrow_mut().available_update = None;
        let first = render_welcome_card(&shell.state.borrow(), width, 10, Instant::now());
        shell.state.borrow_mut().available_update = Some(semver::Version::new(9, 8, 7));
        let updated = render_welcome_card(&shell.state.borrow(), width, 10, Instant::now());
        assert_eq!(
            first.len(),
            updated.len(),
            "hint changed card geometry at {width}"
        );
    }
}

#[test]
fn narrow_welcome_stacks_before_permissions_or_exit_help_would_clip() {
    for theme in [
        crate::tui::theme::test_theme(),
        crate::tui::theme::test_theme_from_source("[colors]\nsplash_compact = true\n"),
        crate::tui::theme::test_theme_from_source(
            "[colors]\nsplash_compact = true\nsplash_box = '#d97757'\n",
        ),
        crate::tui::theme::test_theme_from_source("[metadata]\nname = 'Large'\n"),
    ] {
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.set_identity("custom", "custom/probe", "off");
        for safe_mode in [false, true] {
            shell.state.borrow_mut().safe_mode = safe_mode;
            for width in 24..=80 {
                let rows = render_welcome_content(&shell.state.borrow(), width, 10);
                let plain = strip_terminal_sequences(&rows.join("\n"));
                let access = if safe_mode {
                    "safe mode"
                } else {
                    "full access"
                };
                for detail in [access, "Ctrl+C / Ctrl+D to exit", "/changelog"] {
                    assert!(
                        plain.contains(detail),
                        "width={width}, missing {detail}: {plain}"
                    );
                }
                assert!(rows
                    .iter()
                    .all(|row| sexy_tui_rs::visible_width(row) <= usize::from(width)));
            }
        }
    }
}

#[test]
fn tiny_welcome_sheds_permission_prefix_before_access_text() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    for color in [ColorDepth::TrueColor, ColorDepth::None] {
        for unicode in [false, true] {
            let shell =
                InteractiveShell::test_shell_with_theme(crate::tui::theme::test_theme_with(
                    TerminalCapabilities::test(true, unicode, color),
                ));
            for safe_mode in [false, true] {
                shell.state.borrow_mut().safe_mode = safe_mode;
                let access = if safe_mode {
                    "safe mode"
                } else {
                    "full access"
                };
                for width in access.len() as u16..24 {
                    let rows = render_welcome_content(&shell.state.borrow(), width, 10);
                    let plain = strip_terminal_sequences(&rows.join("\n"));
                    assert!(plain.contains(access), "width={width}: {plain}");
                    assert!(rows
                        .iter()
                        .all(|row| sexy_tui_rs::visible_width(row) <= usize::from(width)));
                    if color == ColorDepth::None {
                        assert!(!rows.join("").contains('\x1b'));
                    }
                }
            }
        }
    }
}

#[test]
fn compact_themes_keep_welcome_within_row_budget() {
    for source in [
        "[colors]\nsplash_compact = true\n",
        include_str!("../../../../../examples/themes/Cards.toml"),
        include_str!("../../../../../examples/themes/Still.toml"),
    ] {
        let shell = InteractiveShell::test_shell_with_theme(
            crate::tui::theme::test_theme_from_source(source),
        );
        for width in [24, 36, 37, 38, 39, 40, 48, 80] {
            for budget in 0..=10 {
                let rows = render_welcome_content(&shell.state.borrow(), width, budget);
                assert!(
                    rows.len() <= budget,
                    "width={width}, budget={budget}: {rows:?}"
                );
                assert_eq!(
                    welcome_content_rows(&shell.state.borrow(), width, budget),
                    rows.len()
                );
            }
        }
    }
}

#[test]
fn welcome_logo_occupied_geometry_narrow() {
    assert_welcome_logo_geometry(&[(23, 1)]);
}

#[test]
fn welcome_logo_occupied_geometry_intermediate() {
    assert_welcome_logo_geometry(&[(46, 1), (60, 1), (48, 1), (71, 1)]);
}

#[test]
fn welcome_logo_occupied_geometry_wide() {
    assert_welcome_logo_geometry(&[(80, 1), (72, 1), (120, 1)]);
}

fn assert_welcome_logo_geometry(widths: &[(u16, usize)]) {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use std::time::Duration;

    for (unicode, color, animation) in [
        (true, ColorDepth::TrueColor, true),
        (false, ColorDepth::TrueColor, true),
        (true, ColorDepth::None, true),
        (true, ColorDepth::TrueColor, false),
    ] {
        let mut capabilities = TerminalCapabilities::test(true, unicode, color);
        capabilities.animation = animation;
        let mut shell = InteractiveShell::test_shell_with_theme(
            crate::tui::theme::test_theme_with(capabilities),
        );
        shell.set_identity("anthropic", "claude-sonnet-4", "high");
        let started = Instant::now();
        shell.state.borrow_mut().startup_card_started_at = Some(started);
        let glyph = if unicode { '█' } else { '#' };
        for &(width, scale) in widths {
            let mut initial_geometry = None;
            for elapsed in [0.0, 0.5, crate::tui::splash::DURATION, 3.0] {
                let rendered = render_welcome_card(
                    &shell.state.borrow(),
                    width,
                    10,
                    started + Duration::from_secs_f32(elapsed),
                );
                assert_eq!(rendered.len(), 6, "compact welcome layout at {width}");
                let plain = strip_terminal_sequences(&rendered.join("\n"));
                assert!(
                    !plain.contains("Claude Sonnet"),
                    "footer owns model identity: {plain}"
                );
                assert!(
                    plain.contains("full access"),
                    "permission mode lost: {plain}"
                );
                let occupied = rendered
                    .iter()
                    .enumerate()
                    .flat_map(|(y, line)| {
                        strip_terminal_sequences(line)
                            .chars()
                            .enumerate()
                            .filter_map(move |(x, ch)| (ch == glyph).then_some((x, y)))
                            .collect::<Vec<_>>()
                    })
                    .collect::<std::collections::HashSet<_>>();
                let left = occupied.iter().map(|(x, _)| *x).min().unwrap();
                let right = occupied.iter().map(|(x, _)| *x).max().unwrap();
                let top = occupied.iter().map(|(_, y)| *y).min().unwrap();
                let bottom = occupied.iter().map(|(_, y)| *y).max().unwrap();
                assert_eq!((right - left + 1, bottom - top + 1), (8 * scale, 2 * scale),
                    "occupied byte, not padding: terminal width={width}, unicode={unicode}, elapsed={elapsed}");
                assert_eq!(occupied.len(), 14 * scale * scale);
                for y in 0..2 * scale {
                    for x in 0..8 * scale {
                        let expected = y >= scale || ![0, 3].contains(&(x / scale));
                        assert_eq!(occupied.contains(&(left + x, top + y)), expected,
                            "equal contiguous columns/half-height zeros: width={width}, x={x}, y={y}");
                    }
                }
                if let Some(initial) = &initial_geometry {
                    assert_eq!(&occupied, initial, "animation must not move geometry");
                } else {
                    initial_geometry = Some(occupied);
                }
            }
        }
    }
}

#[test]
fn custom_welcome_theme_uses_accent_gradient_and_framing() {
    use std::time::Duration;

    let shell = InteractiveShell::test_shell_with_theme(crate::tui::theme::test_theme_from_source(
        r##"
                [metadata]
                name = "Welcome fixture"
                adaptive = false
                [colors]
                splash = "#d97757"
                splash_box = "#d97757"
            "##,
    ));
    // Settle past the sweep so the themed gradient is deterministic.
    shell.state.borrow_mut().startup_card_started_at =
        Some(Instant::now() - Duration::from_secs(10));
    let rendered = render_welcome_card(&shell.state.borrow(), 80, 10, Instant::now());
    let joined = rendered.join("\n");
    // Splash text and frame keep the exact accent.
    assert!(joined.contains("38;2;217;119;87"), "{rendered:?}");
    // The byte mark shades the accent into a truecolor column gradient.
    assert!(joined.contains("38;2;119;65;47"), "{rendered:?}");
    assert!(joined.contains("38;2;228;159;137"), "{rendered:?}");
    let plain = rendered
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert!(
        plain[0].starts_with('╭') && plain[0].ends_with('╮'),
        "{plain:?}"
    );
    assert!(plain
        .last()
        .is_some_and(|line| line.starts_with('╰') && line.ends_with('╯')));
    assert!(rendered
        .iter()
        .all(|line| sexy_tui_rs::visible_width(line) == 80));
}

#[test]
fn splash_model_adaptive_token_keeps_the_model_blended_gradient() {
    use std::time::Duration;

    let theme = crate::tui::theme::test_theme_from_source(
        r##"
            [metadata]
            name = "Adaptive splash fixture"
            adaptive = false
            [colors]
            splash = "#d97757"
            splash_model_adaptive = "true"
            model.use_lab_color = "true"
        "##,
    );
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    shell.set_identity("anthropic", "claude-sonnet-4", "high");
    // Settle past the sweep so only the gradient columns remain.
    shell.state.borrow_mut().startup_card_started_at =
        Some(Instant::now() - Duration::from_secs(10));
    let state = shell.state.borrow();
    let rendered = render_welcome_card(&state, 80, 10, Instant::now()).join("\n");
    let accent = state
        .theme
        .model_rgb(Some(crate::tui::theme::ModelLab::Anthropic))
        .expect("anthropic accent");
    let blended = |gradient: (u8, u8, u8)| {
        let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * 0.58) as u8;
        (
            mix(gradient.0, accent.0),
            mix(gradient.1, accent.1),
            mix(gradient.2, accent.2),
        )
    };
    let first = blended((0x4b, 0x8d, 0xff));
    // The byte-mark follows the model family, not the theme's splash hue.
    assert!(
        rendered.contains(&format!("38;2;{};{};{}", first.0, first.1, first.2)),
        "{rendered:?}"
    );
    assert!(!rendered.contains("38;2;119;65;47"), "{rendered:?}");
    // The splash text follows the model accent too: the theme's own
    // `splash` colour is not used anywhere in the splash.
    assert!(!rendered.contains("38;2;217;119;87"), "{rendered:?}");
    let wordmark = state
        .theme
        .model_rgb(Some(crate::tui::theme::ModelLab::Anthropic))
        .expect("anthropic accent");
    assert!(
        rendered.contains(&format!(
            "38;2;{};{};{}",
            wordmark.0, wordmark.1, wordmark.2
        )),
        "{rendered:?}"
    );
}

#[test]
fn splash_compact_token_selects_default_splash_geometry() {
    for (compact, logo_rows) in [(true, 2), (false, 6)] {
        let source = format!(
            r##"
                [metadata]
                name = "Compact fixture"
                adaptive = false
                [colors]
                splash = "#d97757"
                splash_compact = {compact}
            "##,
        );
        let shell = InteractiveShell::test_shell_with_theme(
            crate::tui::theme::test_theme_from_source(&source),
        );
        shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
        let rendered = render_welcome_card(&shell.state.borrow(), 80, 10, Instant::now());
        let marked = rendered
            .iter()
            .filter(|line| strip_terminal_sequences(line).contains('█'))
            .count();
        assert_eq!(marked, logo_rows, "compact={compact}: {rendered:?}");
    }
}

#[test]
fn still_keeps_the_compact_model_adaptive_splash() {
    // `Still` ships as a built-in, so its example file is the source of the
    // compiled selector. The startup mark must stay the compact 2-row byte
    // that follows the active model family, not the larger file-theme
    // presentation pinned to a fixed `splash` colour.
    const STILL: &str = include_str!("../../../../../examples/themes/Still.toml");
    let theme = crate::tui::theme::test_theme_from_source(STILL);
    assert_eq!(
        theme.resolve::<String>("splash_model_adaptive").as_deref(),
        Some("true")
    );
    assert_eq!(
        theme.resolve::<String>("splash_compact").as_deref(),
        Some("true")
    );
    let shell = InteractiveShell::test_shell_with_theme(theme);
    shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    let rendered = render_welcome_card(&shell.state.borrow(), 80, 10, Instant::now());
    let marked = rendered
        .iter()
        .filter(|line| strip_terminal_sequences(line).contains('█'))
        .count();
    assert_eq!(marked, 2, "Still should use the compact mark: {rendered:?}");

    // The mark is shaded into a column gradient across its byte glyphs, so
    // one row carries more than a single truecolor foreground.
    let gradients = rendered
        .iter()
        .filter(|line| {
            let mut shades = line
                .split("\x1b[38;2;")
                .skip(1)
                .filter_map(|rest| rest.split('m').next())
                .collect::<Vec<_>>();
            shades.dedup();
            shades.len() >= 2
        })
        .count();
    assert!(
        gradients >= 1,
        "the mark should shade into a gradient: {rendered:?}"
    );
}

#[test]
fn welcome_card_shows_access_mode_and_safe_mode_hint() {
    let shell = InteractiveShell::test_shell();
    shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    let rendered = render_welcome_card(&shell.state.borrow(), 80, 10, Instant::now()).join("\n");
    let rendered = strip_terminal_sequences(&rendered);
    assert!(rendered.contains("permissions: full access"), "{rendered}");
    assert!(
        rendered.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))),
        "{rendered}"
    );

    let safe_shell = InteractiveShell::test_shell();
    safe_shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    safe_shell.state.borrow_mut().safe_mode = true;
    let rendered =
        render_welcome_card(&safe_shell.state.borrow(), 80, 10, Instant::now()).join("\n");
    let rendered = strip_terminal_sequences(&rendered);
    assert!(rendered.contains("permissions: safe mode"), "{rendered}");
}

#[test]
fn welcome_card_uses_the_centered_reading_column() {
    let theme = crate::tui::theme::test_theme_from_source(
        "[colors]\ncontent_max_width = 112\n[layout]\ntranscript_inset = 2\ncomposer_padding = 1",
    );
    let shell = InteractiveShell::test_shell_with_theme(theme);
    shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    let state = shell.state.borrow();
    let width = 160;
    let layout = PresentationLayout::new(&state.theme, width);
    assert!(layout.inset > 0);
    let rows = render_welcome_card(&state, width, 10, Instant::now());
    for row in rows.iter().filter(|row| !row.is_empty()) {
        let plain = strip_terminal_sequences(row);
        assert!(plain.starts_with(&" ".repeat(usize::from(layout.inset))));
        assert!(sexy_tui_rs::visible_width(row) <= usize::from(width));
    }
    assert!(rows.iter().any(|row| row.contains("octet")));
}

#[test]
fn narrow_welcome_card_keeps_a_bounded_identity_fallback() {
    let shell = InteractiveShell::test_shell();
    shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    for width in [4, 8, 16, 23, 24] {
        let rendered = render_welcome_card(&shell.state.borrow(), width, 10, Instant::now());
        assert!(
            !rendered.is_empty(),
            "narrow welcome missing at width {width}"
        );
        assert!(rendered
            .iter()
            .all(|line| { sexy_tui_rs::visible_width(line) <= usize::from(width) }));
        assert!(rendered
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .any(|line| line.contains('o') || line.contains('#')));
    }
}
