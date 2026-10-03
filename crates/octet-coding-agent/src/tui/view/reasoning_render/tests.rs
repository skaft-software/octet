//! Test suite for the reasoning / activity render path in this module.
//!
//! Kept as a sibling file rather than an inline `mod tests` block because the
//! suite is large relative to the renderer it covers, and the render code is
//! what readers come here for: the assertions about collapsed vs. expanded
//! reasoning rows, shimmer geometry, and composer row reservation get their
//! own file so the geometry constants above stay readable on their own.

use std::time::{Duration, Instant};

use sexy_tui_rs::{strip_terminal_sequences, visible_width};

use super::*;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
use crate::tui::theme::{self, ModelLab};

fn classic_theme_for(
    background: TerminalBackground,
    capabilities: TerminalCapabilities,
) -> OctetTheme {
    theme::test_theme_for_shimmer(background, capabilities, ShimmerMode::Classic)
}

fn physical_theme_for(
    background: TerminalBackground,
    capabilities: TerminalCapabilities,
) -> OctetTheme {
    theme::test_theme_for_shimmer(background, capabilities, ShimmerMode::Physical)
}

fn render_reasoning(
    reasoning: &AssistantBlock,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    show_reasoning: bool,
) -> Vec<String> {
    render_reasoning_on_surface(reasoning, renderer, theme, width, show_reasoning, None, 0)
}

fn rendered_foregrounds(rendered: &str) -> Vec<Rgb> {
    rendered
        .split("\x1b[")
        .filter_map(|part| {
            let (sgr, _) = part.split_once('m')?;
            if let Some(rgb) = sgr.strip_prefix("38;2;") {
                let values = rgb
                    .split(';')
                    .map(|channel| channel.parse::<u8>().unwrap())
                    .collect::<Vec<_>>();
                return Some((values[0], values[1], values[2]));
            }
            let index = sgr.strip_prefix("38;5;")?.parse::<u8>().unwrap();
            if index >= 232 {
                let grey = 8 + (index - 232) * 10;
                Some((grey, grey, grey))
            } else {
                // Octet quantizes RGB into the fixed cube/grayscale entries,
                // not the terminal-customizable first sixteen colours.
                assert!(index >= 16);
                let levels = [0, 95, 135, 175, 215, 255];
                let cube = usize::from(index - 16);
                Some((levels[cube / 36], levels[cube / 6 % 6], levels[cube % 6]))
            }
        })
        .collect()
}

fn luminance(rgb: Rgb) -> f64 {
    activity_luminance(rgb)
}

/// Normalized absolute chroma: the sRGB channel spread in `[0, 1]`.
fn chroma(rgb: Rgb) -> f64 {
    let max = f64::from(rgb.0.max(rgb.1).max(rgb.2));
    let min = f64::from(rgb.0.min(rgb.1).min(rgb.2));
    (max - min) / 255.0
}

fn hue_degrees(rgb: Rgb) -> f64 {
    let channel = |value: u8| f64::from(value) / 255.0;
    let (red, green, blue) = (channel(rgb.0), channel(rgb.1), channel(rgb.2));
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let spread = max - min;
    if spread <= f64::EPSILON {
        return 0.0;
    }
    let sector = if max == red {
        ((green - blue) / spread).rem_euclid(6.0)
    } else if max == green {
        (blue - red) / spread + 2.0
    } else {
        (red - green) / spread + 4.0
    };
    (sector * 60.0).rem_euclid(360.0)
}

fn contrast_ratio(first: f64, second: f64) -> f64 {
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

/// The colour the theme's own encoder emits for `color`, read back through
/// the same parser the render assertions use.
fn quantized(theme: &OctetTheme, color: Rgb) -> Rgb {
    rendered_foregrounds(&theme.rgb_fg(color, "x"))[0]
}

fn foreground_color_codes(rendered: &str) -> Vec<String> {
    rendered_foregrounds(rendered)
        .into_iter()
        .map(|(red, green, blue)| format!("{red};{green};{blue}"))
        .collect()
}

#[test]
fn verbose_reasoning_deltas_keep_complete_incremental_state() {
    let theme = theme::test_theme();
    let mut reasoning = AssistantBlock::streaming_reasoning("First complete thought.\n\n");
    let initial_revision = reasoning.markdown.tail_revision();

    for step in 0..256 {
        reasoning.append_reasoning(&format!("Thought {step} stays visible.\n\n"));
    }

    assert!(
        reasoning.markdown.tail_revision() >= initial_revision + 256,
        "ordinary deltas must extend one incremental Markdown stream"
    );
    reasoning.reasoning_expanded = true;
    let live = render_reasoning(&reasoning, &theme.reasoning_renderer(), &theme, 80, true)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(live.contains("First complete thought."), "{live}");
    assert!(live.contains("Thought 0 stays visible."), "{live}");
    assert!(live.contains("Thought 255 stays visible."), "{live}");

    reasoning.finish_reasoning();
    let finished = render_reasoning(&reasoning, &theme.reasoning_renderer(), &theme, 80, true)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(finished.contains("First complete thought."), "{finished}");
    assert!(finished.contains("Thought 0 stays visible."), "{finished}");
    assert!(
        finished.contains("Thought 255 stays visible."),
        "{finished}"
    );
}

#[test]
fn expanded_reasoning_has_no_first_line_bullet() {
    let theme = theme::test_theme();
    let mut reasoning =
        AssistantBlock::streaming_reasoning("First private thought.\n\nSecond private thought.");
    reasoning.reasoning_expanded = true;

    let lines = render_reasoning(&reasoning, &theme.reasoning_renderer(), &theme, 80, true)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    assert!(lines[0].starts_with("First private thought."), "{lines:?}");
    assert!(lines[1].starts_with("Second private thought."), "{lines:?}");
    assert!(
        lines
            .iter()
            .all(|line| !line.starts_with('•') && !line.starts_with('·')),
        "expanded reasoning must not look like a bulleted list: {lines:?}"
    );
}

#[test]
fn collapsed_reasoning_shimmers_thinking_and_moves_heading_to_detail() {
    let theme = theme::test_theme();
    let reasoning = AssistantBlock::streaming_reasoning("## Verifying `implementation`")
        .with_model_lab(Some(ModelLab::Alibaba));
    let first = collapsed_reasoning_lines_at(&theme, &reasoning, 2, 0);
    let next = collapsed_reasoning_lines_at(&theme, &reasoning, 3, 0);

    assert_eq!(strip_terminal_sequences(&first[0]), "Thinking");
    assert_eq!(
        strip_terminal_sequences(&first[1]),
        "└ Verifying implementation (ctrl+o to expand)"
    );
    assert!(first[0].contains("\x1b[1m"), "{first:?}");
    assert!(!first[1].contains("\x1b[3m"), "{first:?}");
    assert_eq!(strip_terminal_sequences(&next[0]), "Thinking");
    assert_eq!(first[1], next[1], "the detail must not shimmer");
    assert_ne!(
        first[0], next[0],
        "the activity label should sweep in foreground"
    );
    assert!(first[0].contains("38;2;"), "{first:?}");
    assert!(
        !first[0].contains(";48;2;"),
        "the Codex-style shimmer must never paint character backgrounds: {first:?}"
    );
}

#[test]
fn model_and_rainbow_shimmers_are_foreground_only() {
    let theme = theme::test_theme();
    let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
    let mut working = reasoning;
    working.reasoning_heading = Some("Working".into());
    working.show_reasoning_hint = false;

    let normal = collapsed_reasoning_lines_at(&theme, &working, 0, 0);
    assert!(normal[0].contains("38;2;"), "{normal:?}");
    assert!(!normal[0].contains(";48;2;"), "{normal:?}");

    let rainbow = collapsed_reasoning_lines_at(&theme, &working, 0, 100);
    assert!(rainbow[0].contains("38;2;"), "{rainbow:?}");
    assert!(!rainbow[0].contains(";48;2;"), "{rainbow:?}");
    assert_ne!(normal[0], rainbow[0]);

    let plain = theme::test_theme_with(TerminalCapabilities::test(false, false, ColorDepth::None));
    let no_color = collapsed_reasoning_lines_at(&plain, &working, 0, 100);
    assert_eq!(no_color, vec!["Working"]);
    assert!(!no_color[0].contains('\x1b'));
}

/// When [`activity_shimmer_palette`] declines to animate - no animation, no
/// interactivity, or no colour at all - the status must fall back to the
/// *static* model foreground: one `bold(model_fg)` run, byte-identical on
/// every frame, with no ramp, no rainbow and no background cell. The
/// emphasis level must not change it either: the fallback is what a
/// non-animating terminal shows, and the rainbow lives inside the palette
/// branch above it.
#[test]
fn a_terminal_that_cannot_animate_falls_back_to_the_static_model_foreground() {
    for capabilities in [
        // No interactivity at all, and no colour at all: the two states
        // `TerminalCapabilities::test` can produce which also switch
        // animation off (`animation = interactive && color != None`).
        TerminalCapabilities::test(false, true, ColorDepth::TrueColor),
        TerminalCapabilities::test(false, true, ColorDepth::Ansi256),
        TerminalCapabilities::test(true, true, ColorDepth::None),
        // And animation switched off on its own, with colour and
        // interactivity still available (`/animations off`-style profiles).
        TerminalCapabilities {
            animation: false,
            ..TerminalCapabilities::test(true, true, ColorDepth::TrueColor)
        },
        TerminalCapabilities {
            animation: false,
            ..TerminalCapabilities::test(true, true, ColorDepth::Ansi256)
        },
        TerminalCapabilities {
            animation: false,
            ..TerminalCapabilities::test(true, false, ColorDepth::Ansi16)
        },
    ] {
        for background in [
            TerminalBackground::Dark,
            TerminalBackground::Light,
            TerminalBackground::Unknown,
        ] {
            let theme = classic_theme_for(background, capabilities);
            for lab in [Some(ModelLab::OpenAi), Some(ModelLab::Alibaba), None] {
                for label in ["Working", "Thinking"] {
                    let reasoning = activity_reasoning(lab, label);
                    assert_eq!(
                        activity_shimmer_palette(&theme, &reasoning),
                        None,
                        "{capabilities:?}/{lab:?}: this theme must not animate"
                    );
                    let expected = theme.bold(&theme.model_fg(lab, label));
                    for frame in 0..=activity_cycle(label) {
                        for strength in [0, 100] {
                            let rendered =
                                activity_shimmer_label(&theme, &reasoning, label, frame, strength);
                            assert_eq!(
                                rendered, expected,
                                "{background:?}/{capabilities:?}/{lab:?}/{label}: \
                                 static fallback at frame {frame}, rainbow={strength}"
                            );
                            for marker in ["•", "*"] {
                                assert_eq!(
                                    activity_shimmer_marker(
                                        &theme, &reasoning, frame, strength, marker,
                                    ),
                                    theme.model_fg(lab, marker),
                                    "the dot must share the static fallback, including its peak"
                                );
                            }
                            if capabilities.color == ColorDepth::None {
                                assert_eq!(rendered, label);
                                assert!(!rendered.contains('\x1b'));
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn activity_shimmer_contrast_survives_light_and_dark_composite_surfaces() {
    for (background, surface) in [
        (TerminalBackground::Dark, (38, 38, 38)),
        (TerminalBackground::Dark, (64, 64, 64)),
        (TerminalBackground::Light, (245, 245, 245)),
        (TerminalBackground::Light, (224, 224, 224)),
    ] {
        for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
            let theme =
                classic_theme_for(background, TerminalCapabilities::test(true, true, depth));
            for lab in [None, Some(ModelLab::OpenAi), Some(ModelLab::Alibaba)] {
                let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(lab);
                for label in [
                    "Working",
                    "Thinking",
                    "Compacting context",
                    "Waiting for network",
                ] {
                    for strength in [0, 50, 100] {
                        for frame in 0..activity_cycle(label) {
                            let rendered =
                                activity_shimmer_label(&theme, &reasoning, label, frame, strength);
                            assert_eq!(strip_terminal_sequences(&rendered), label);
                            assert!(!rendered.contains("\x1b[48;"));
                            assert!(!rendered.contains("\x1b[2m"));
                            let colors = rendered_foregrounds(&rendered);
                            assert_eq!(colors.len(), label.chars().count());
                            for color in colors {
                                let foreground = luminance(color);
                                let background_luminance = luminance(surface);
                                let contrast = (foreground.max(background_luminance) + 0.05)
                                    / (foreground.min(background_luminance) + 0.05);
                                assert!(
                                    contrast >= 4.5,
                                    "{background:?}/{depth:?}/{lab:?} {label} frame={frame} rainbow={strength}: {color:?} on {surface:?} has contrast {contrast:.2}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn activity_shimmer_sweep_follows_terminal_profile() {
    let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
    let dark = classic_theme_for(
        TerminalBackground::Dark,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
    );
    let light = classic_theme_for(
        TerminalBackground::Light,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
    );
    let (dark_baseline, dark_sweep) =
        activity_shimmer_palette(&dark, &reasoning).expect("dark activity palette");
    let (light_baseline, light_sweep) =
        activity_shimmer_palette(&light, &reasoning).expect("light activity palette");
    assert!(activity_luminance(dark_baseline) > activity_luminance(dark_sweep));
    assert!(activity_luminance(light_baseline) < activity_luminance(light_sweep));
    // The maintainer read the previous separation as "too subtle": 0.23 for
    // dark and 0.04 for light (0.01 -> 0.05). Both profiles must now move at
    // least 0.08 of relative luminance, which is more than a single ANSI256
    // grayscale step (~0.02 at these levels) can explain.
    assert!(
        activity_luminance(dark_baseline) - activity_luminance(dark_sweep) >= 0.30,
        "dark sweep separation"
    );
    assert!(
        activity_luminance(light_sweep) - activity_luminance(light_baseline) >= 0.08,
        "light sweep separation"
    );
}

#[test]
fn physical_shimmer_uses_a_smooth_asymmetric_band_and_parked_rest() {
    assert_eq!(band_main(ACTIVITY_PHYS_H_FRONT), 0.0);
    assert_eq!(band_main(-ACTIVITY_PHYS_H_TRAIL), 0.0);
    assert!(band_main(-2.5) > band_main(2.5));
    assert_eq!(band_core(ACTIVITY_PHYS_CORE_HALF), 0.0);
    assert!((band_main(0.0) - 1.0).abs() < f64::EPSILON);

    for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
        let theme = physical_theme_for(
            TerminalBackground::Dark,
            TerminalCapabilities::test(true, true, depth),
        );
        let speed = physical_speed(&theme);
        let label = "Working";
        let motion = physical_motion_ticks(label, speed);
        let cycle = physical_cycle(label, speed);
        assert_eq!(cycle, motion + ACTIVITY_PHYS_REST_TICKS);

        let start = ACTIVITY_MARKER_INDEX as f64 - ACTIVITY_PHYS_H_FRONT;
        let end = label.width() as f64 - 1.0 + ACTIVITY_PHYS_H_TRAIL;
        assert!((phys_center(label, 0, speed) - start).abs() < 1e-9);
        assert!((phys_center(label, motion - 1, speed) - end).abs() < 1e-9);
        assert!((phys_center(label, motion, speed) - end).abs() < 1e-9);

        let reasoning = activity_reasoning(Some(ModelLab::Alibaba), label);
        let resting = activity_shimmer_label(&theme, &reasoning, label, 0, 0);
        let resting_marker = activity_shimmer_marker(&theme, &reasoning, 0, 0, "•");
        for frame in [motion, motion + 1, cycle] {
            assert_eq!(
                activity_shimmer_label(&theme, &reasoning, label, frame, 0),
                resting,
                "parked physical frame {frame} must be at rest"
            );
            assert_eq!(
                activity_shimmer_marker(&theme, &reasoning, frame, 0, "•"),
                resting_marker,
                "parked marker frame {frame} must be at rest"
            );
        }
    }
}

#[test]
fn physical_shimmer_moves_toward_the_profile_extreme_without_losing_contrast() {
    for (background, surface) in [
        (TerminalBackground::Dark, (64, 64, 64)),
        (TerminalBackground::Light, (224, 224, 224)),
    ] {
        for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
            let theme =
                physical_theme_for(background, TerminalCapabilities::test(true, true, depth));
            let reasoning = activity_reasoning(Some(ModelLab::Alibaba), "Working");
            let (baseline, sweep) =
                activity_shimmer_palette(&theme, &reasoning).expect("physical palette");
            let baseline_luminance = luminance(baseline);
            let sweep_luminance = luminance(sweep);
            assert!(
                sweep_luminance > baseline_luminance,
                "physical mode brightens both known profiles"
            );
            if background == TerminalBackground::Dark {
                assert!(baseline_luminance >= 0.50);
            } else {
                assert!(baseline_luminance <= 0.02);
                assert!(sweep_luminance >= 0.10);
            }

            let label = "Working";
            let speed = physical_speed(&theme);
            let motion = physical_motion_ticks(label, speed);
            let cycle = physical_cycle(label, speed);
            let peak_frame = (0..motion)
                .min_by(|left, right| {
                    let left_distance = (phys_center(label, *left, speed) - 0.0).abs();
                    let right_distance = (phys_center(label, *right, speed) - 0.0).abs();
                    left_distance.total_cmp(&right_distance)
                })
                .expect("physical motion frames");
            let resting =
                rendered_foregrounds(&activity_shimmer_label(&theme, &reasoning, label, 0, 0));
            let peak = rendered_foregrounds(&activity_shimmer_label(
                &theme, &reasoning, label, peak_frame, 0,
            ));
            assert_ne!(peak[0], resting[0]);
            assert!(luminance(peak[0]) > luminance(resting[0]));

            for frame in 0..cycle {
                let colors = rendered_foregrounds(&activity_shimmer_label(
                    &theme, &reasoning, label, frame, 0,
                ));
                for color in colors {
                    assert!(
                        contrast_ratio(luminance(color), luminance(surface)) >= 4.5,
                        "{background:?}/{depth:?} emitted {color:?} with insufficient contrast"
                    );
                }
            }
        }
    }
}

/// The frame at which cell `index` is the centre of the sweep.
fn centre_frame(index: isize) -> usize {
    (index - ACTIVITY_SWEEP_START) as usize
}

/// A collapsed activity block whose own label - the string the margin dot
/// shimmers with (`activity_label`) - is `label`, so the dot and the label
/// share one cycle.
fn activity_reasoning(lab: Option<ModelLab>, label: &str) -> AssistantBlock {
    let mut reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(lab);
    reasoning.reasoning_heading = Some(label.to_owned());
    reasoning.show_reasoning_hint = false;
    reasoning
}

/// The activity palette's resting colour, as the renderer encodes it.
fn resting_colour(theme: &OctetTheme, reasoning: &AssistantBlock) -> Rgb {
    let (baseline, _) = activity_shimmer_palette(theme, reasoning).expect("activity palette");
    quantized(theme, baseline)
}

#[test]
fn activity_dot_has_a_visible_sweep_correlated_pulse_and_returns_to_rest() {
    for (background, surface) in [
        (TerminalBackground::Dark, (64, 64, 64)),
        (TerminalBackground::Light, (224, 224, 224)),
    ] {
        for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
            let theme =
                classic_theme_for(background, TerminalCapabilities::test(true, true, depth));
            for lab in [Some(ModelLab::OpenAi), Some(ModelLab::Alibaba), None] {
                for label in ["Working", "Thinking", "Compacting context"] {
                    let reasoning = activity_reasoning(lab, label);
                    for marker in ["•", "*"] {
                        let render =
                            |frame| activity_shimmer_marker(&theme, &reasoning, frame, 0, marker);
                        let resting = render(0);
                        let baseline = rendered_foregrounds(&resting)[0];
                        let peak_frame = centre_frame(ACTIVITY_MARKER_INDEX);
                        let peak = rendered_foregrounds(&render(peak_frame))[0];
                        let baseline_luminance = luminance(baseline);
                        let peak_luminance = luminance(peak);
                        // A tiny dot needs substantially more than a merely
                        // different RGB value, including after quantization.
                        assert!(
                            contrast_ratio(baseline_luminance, peak_luminance) >= 2.0,
                            "{background:?}/{depth:?}/{lab:?}/{label}: dot {baseline:?} -> {peak:?}"
                        );
                        assert_eq!(
                            peak_luminance > baseline_luminance,
                            background == TerminalBackground::Dark,
                            "the pulse must gain contrast against the terminal surface"
                        );
                        let cycle = activity_cycle(label);
                        for frame in 0..cycle {
                            let rendered = render(frame);
                            assert_eq!(strip_terminal_sequences(&rendered), marker);
                            assert_eq!(visible_width(&rendered), 1);
                            assert!(!rendered.contains("\x1b[48;"));
                            assert!(!rendered.contains("\x1b[2m"));
                            let colors = rendered_foregrounds(&rendered);
                            assert_eq!(colors.len(), 1);
                            let current = luminance(colors[0]);
                            assert!(contrast_ratio(current, luminance(surface)) >= 2.5);
                            if lab == Some(ModelLab::OpenAi) {
                                assert_eq!(chroma(colors[0]), 0.0, "{rendered:?}");
                            }
                            if frame.abs_diff(peak_frame) > ACTIVITY_SWEEP_HALF as usize {
                                assert_eq!(rendered, resting, "dot must rest outside the sweep");
                            } else {
                                assert_ne!(rendered, resting, "dot must pulse inside the sweep");
                                assert!(
                                    (current - baseline_luminance).abs()
                                        <= (peak_luminance - baseline_luminance).abs(),
                                    "{background:?}/{depth:?}/{lab:?}/{label}: frame {frame} \
                                     must not outshine the dot's centre frame"
                                );
                            }
                        }
                        assert_eq!(render(cycle - 1), resting);
                        assert_eq!(render(cycle), resting);
                        // The dot peaks before the first letter, not on an
                        // independent spinner/breathing clock.
                        assert_eq!(centre_frame(0) - peak_frame, ACTIVITY_LABEL_OFFSET as usize);
                    }
                }
            }
        }
    }
}

#[test]
fn rendered_activity_text_has_strong_sweep_separation_without_losing_contrast() {
    for (background, surface, minimum_separation) in [
        (TerminalBackground::Dark, (64, 64, 64), 1.5),
        (TerminalBackground::Light, (224, 224, 224), 1.7),
    ] {
        for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
            let theme =
                classic_theme_for(background, TerminalCapabilities::test(true, true, depth));
            for lab in [
                Some(ModelLab::OpenAi),
                Some(ModelLab::Alibaba),
                Some(ModelLab::Meta),
                Some(ModelLab::Google),
                None,
            ] {
                for label in ["Working"] {
                    let reasoning = activity_reasoning(lab, label);
                    let rest = collapsed_reasoning_lines_at(&theme, &reasoning, 0, 0);
                    let resting = rendered_foregrounds(&rest[0])[0];
                    for frame in 0..activity_cycle(label) {
                        let rows = collapsed_reasoning_lines_at(&theme, &reasoning, frame, 0);
                        assert_eq!(rows.len(), 1);
                        assert_eq!(strip_terminal_sequences(&rows[0]), label);
                        assert!(rows[0].contains("\x1b[1m"));
                        assert!(!rows[0].contains("\x1b[48;"));
                        let colors = rendered_foregrounds(&rows[0]);
                        assert_eq!(colors.len(), label.len());
                        for (cell, color) in colors.into_iter().enumerate() {
                            assert!(
                                contrast_ratio(luminance(color), luminance(surface)) >= 4.5,
                                "{background:?}/{depth:?}/{lab:?}/{label}: frame {frame}, cell {cell}"
                            );
                            if frame == centre_frame(cell as isize) {
                                assert!(
                                    contrast_ratio(luminance(color), luminance(resting))
                                        >= minimum_separation,
                                    "{background:?}/{depth:?}/{lab:?}/{label}: \
                                     cell {cell} {resting:?} -> {color:?} must visibly sweep"
                                );
                            }
                        }
                    }
                    assert_eq!(
                        collapsed_reasoning_lines_at(
                            &theme,
                            &reasoning,
                            activity_cycle(label) - 1,
                            0,
                        ),
                        rest
                    );
                }
            }
        }
    }
}

#[test]
fn activity_foreground_keeps_ansi16_greys_neutral_without_recolouring_other_cells() {
    let ansi16 = theme::test_theme_with(TerminalCapabilities::test(true, true, ColorDepth::Ansi16));
    // The four nominal neutral entries, including the #a7a7a7 interval
    // that the unrestricted RGB-nearest encoder maps to bright magenta.
    for (start, end, code) in [
        (0u8, 51u8, 30),
        (52, 165, 90),
        (166, 242, 37),
        (243, 255, 97),
    ] {
        for grey in start..=end {
            assert_eq!(
                activity_shimmer_foreground(&ansi16, (grey, grey, grey), "*"),
                format!("\x1b[{code}m*\x1b[39m"),
                "neutral grey {grey} must stay in the ANSI16 neutral ramp"
            );
        }
    }
    for depth in [
        ColorDepth::None,
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
    ] {
        let theme = theme::test_theme_with(TerminalCapabilities::test(true, true, depth));
        if depth != ColorDepth::Ansi16 {
            for grey in 0..=u8::MAX {
                let color = (grey, grey, grey);
                assert_eq!(
                    activity_shimmer_foreground(&theme, color, "x"),
                    theme.rgb_fg(color, "x")
                );
            }
        }
        for color in ACTIVITY_RAINBOW
            .into_iter()
            .chain([(166, 167, 167), (31, 32, 31)])
        {
            assert_eq!(
                activity_shimmer_foreground(&theme, color, "x"),
                theme.rgb_fg(color, "x"),
                "{depth:?}: chromatic activity/rainbow encoding must be unchanged"
            );
        }
    }
}

#[test]
fn ansi16_activity_pulse_keeps_supported_styles_and_neutral_identity() {
    // ANSI16 colours are terminal-customizable: assert the actual supported
    // foreground codes change, not a fictitious physical RGB contrast ratio.
    let codes = |rendered: &str| {
        rendered
            .split("\x1b[")
            .filter_map(|part| part.split_once('m')?.0.parse::<u8>().ok())
            .filter(|code| matches!(code, 30..=37 | 90..=97))
            .collect::<Vec<_>>()
    };
    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = classic_theme_for(
            background,
            TerminalCapabilities::test(true, false, ColorDepth::Ansi16),
        );
        for label in ["Working", "Thinking"] {
            let reasoning = activity_reasoning(Some(ModelLab::OpenAi), label);
            let rest_dot = activity_shimmer_marker(&theme, &reasoning, 0, 0, "*");
            let rest_text = activity_shimmer_label(&theme, &reasoning, label, 0, 0);
            for frame in 0..activity_cycle(label) {
                let dot = activity_shimmer_marker(&theme, &reasoning, frame, 0, "*");
                let text = activity_shimmer_label(&theme, &reasoning, label, frame, 0);
                assert_eq!(strip_terminal_sequences(&dot), "*");
                assert_eq!(strip_terminal_sequences(&text), label);
                for rendered in [&dot, &text] {
                    assert!(!rendered.contains("38;"));
                    assert!(!rendered.contains("48;"));
                    let foregrounds = codes(rendered);
                    assert!(
                        foregrounds
                            .iter()
                            .all(|code| matches!(code, 30 | 37 | 90 | 97)),
                        "{background:?}/{label} frame {frame}: neutral activity emitted \
                         non-neutral ANSI16 codes {foregrounds:?} in {rendered:?}"
                    );
                }
                assert_eq!(codes(&dot).len(), 1);
                assert_eq!(codes(&text).len(), label.len());
                if frame == centre_frame(ACTIVITY_MARKER_INDEX) {
                    assert_ne!(
                        codes(&dot),
                        codes(&rest_dot),
                        "{background:?}/{label} frame {frame}: the dot must change code at \
                         its centre frame"
                    );
                }
                if frame == centre_frame(0) {
                    assert_ne!(
                        codes(&text)[0],
                        codes(&rest_text)[0],
                        "{background:?}/{label} frame {frame}: the first label cell must \
                         change code at its centre frame"
                    );
                }
                if frame + 1 == activity_cycle(label) {
                    assert_eq!(dot, rest_dot);
                    assert_eq!(text, rest_text);
                }
            }
        }
    }
}

fn shimmer_cell_luminances(
    theme: &OctetTheme,
    reasoning: &AssistantBlock,
    label: &str,
    frame: usize,
    rainbow_strength: u16,
) -> Vec<f64> {
    rendered_foregrounds(&activity_shimmer_label(
        theme,
        reasoning,
        label,
        frame,
        rainbow_strength,
    ))
    .into_iter()
    .map(luminance)
    .collect()
}

/// The largest luminance move one sweep step can produce: the falloff
/// ladder's biggest adjacent step - `0 -> 18 -> 40 -> 64 -> 84 -> 100` as the
/// centre arrives, and back down as it leaves - measured through exactly the
/// channel blend the renderer uses for an untinted cell. Luminance is not
/// linear in those channels, so this is the honest bound for "the cell moved
/// by one position and nothing else".
fn one_sweep_step_luminance(baseline: Rgb, sweep: Rgb) -> f64 {
    let mut ladder = vec![0u16];
    ladder.extend(ACTIVITY_SWEEP_FALLOFF.iter().rev().copied());
    let luminances = ladder
        .iter()
        .map(|strength| {
            luminance((
                mix_channel(baseline.0, sweep.0, *strength),
                mix_channel(baseline.1, sweep.1, *strength),
                mix_channel(baseline.2, sweep.2, *strength),
            ))
        })
        .collect::<Vec<_>>();
    luminances
        .windows(2)
        .map(|pair| (pair[0] - pair[1]).abs())
        .fold(0.0, f64::max)
}

/// The untinted colour of a cell holding `strength` percent of the sweep.
fn swept_colour(baseline: Rgb, sweep: Rgb, strength: u16) -> Rgb {
    (
        mix_channel(baseline.0, sweep.0, strength),
        mix_channel(baseline.1, sweep.1, strength),
        mix_channel(baseline.2, sweep.2, strength),
    )
}

/// P0 acceptance (maintainer report, `gpt-6-astra` at `high`): the sweep
/// must be a variation of the *model's own* colour, so a neutral model
/// identity stays neutral. The reported label shimmered "orange-yellow"
/// because a fixed warm hue set (0..45 degrees) replaced the model colour
/// on `Working` while a fixed cool set replaced it on `Thinking`.
///
/// `gpt-6-astra` classifies through `classify_model_identity` to
/// `ModelLab::OpenAi` (the `gpt-` prefix), whose `source_color()` is
/// `#1f1f1f`: an exact grey. So the whole shimmer - both labels, every cell,
/// both profiles, both encoders - must be achromatic, and the only movement
/// left is luminance.
#[test]
fn neutral_model_identities_shimmer_without_any_hue() {
    // Tolerance: the ramp forces an exact grey whenever the identity's HSV
    // saturation is at or below `ACTIVITY_NEUTRAL_SATURATION`, so an
    // achromatic identity renders exactly zero channel spread on true
    // colour and on ANSI256 (equal channels quantize to an equal-channel
    // cube/grayscale entry). 0.02 leaves room for the encoder only; a hue
    // rotation - the defect - moves chroma by at least 40/255 = 0.157 (one
    // ANSI256 cube step), an order of magnitude above this bound.
    const NEUTRAL_CHROMA_TOLERANCE: f64 = 0.02;
    // The plain profile separation is >= 0.30 (dark) / >= 0.08 (light), see
    // `activity_shimmer_sweep_follows_terminal_profile`; the ramp's own
    // brightness envelope moves up to 0.16 of it, so a lit centre must move
    // at least 0.30 * 0.84 = 0.25 of that separation even after the encoder.
    const MIN_LUMINANCE_FRACTION_OF_SEPARATION: f64 = 0.20;

    let lab =
        crate::tui::theme::classify_model_identity("gpt-6-astra", "gpt-6-astra", "openai-codex");
    assert_eq!(
        lab,
        ModelLab::OpenAi,
        "gpt-6-astra must classify to the OpenAI lab; the identity, not the id, drives the fix"
    );

    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
            let theme =
                classic_theme_for(background, TerminalCapabilities::test(true, true, depth));
            let identity = theme
                .model_rgb(Some(lab))
                .expect("the OpenAI lab has a source colour");
            assert_eq!(
                chroma(identity),
                0.0,
                "the lab's identity colour must itself be neutral: {identity:?}"
            );

            // `Unknown`/`None` have no model identity at all: the theme's
            // chrome accent stands in for the resting colour, and the ramp
            // must still not invent a hue from it.
            for lab in [Some(lab), Some(ModelLab::Unknown), None] {
                let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(lab);
                let (baseline, sweep) =
                    activity_shimmer_palette(&theme, &reasoning).expect("activity palette");
                let separation = (luminance(baseline) - luminance(sweep)).abs();
                let resting = quantized(&theme, baseline);
                let resting_chroma = chroma(resting);
                if lab == Some(ModelLab::OpenAi) {
                    assert_eq!(
                        resting_chroma, 0.0,
                        "{background:?}/{depth:?}: a neutral identity must rest neutral: {resting:?}"
                    );
                }

                // The most chromatic colour the plain (untinted) palette can
                // show at any cell. An unknown identity's placeholder accent
                // may be chromatic: the ramp is still forbidden to *add* any.
                let palette_chroma =
                    chroma(quantized(&theme, baseline)).max(chroma(quantized(&theme, sweep)));

                for label in ["Working", "Thinking"] {
                    let rendered =
                        activity_shimmer_label(&theme, &reasoning, label, centre_frame(0), 0);
                    let colors = rendered_foregrounds(&rendered);
                    assert_eq!(colors.len(), label.chars().count(), "{rendered:?}");
                    if lab == Some(ModelLab::OpenAi) {
                        for (index, color) in colors.iter().enumerate() {
                            assert!(
                                chroma(*color) <= resting_chroma + NEUTRAL_CHROMA_TOLERANCE,
                                "{background:?}/{depth:?}/{lab:?} {label} cell {index}: \
                                 {color:?} must not gain chroma (resting {resting:?}); a neutral \
                                 identity may only move in luminance"
                            );
                        }
                        // The whole animation, not just the centre frame: the
                        // report was `Working` shimmering orange-yellow at
                        // `high` *while it ran*, and a single frame cannot see
                        // a tint that only appears on some ticks. Every cell of
                        // every frame of the cycle must stay exactly as
                        // achromatic as the resting colour - and meanwhile the
                        // label must still move, otherwise the fix would have
                        // bought neutrality by freezing the shimmer.
                        let mut moved = false;
                        for frame in 0..activity_cycle(label) {
                            let frame_colors = rendered_foregrounds(&activity_shimmer_label(
                                &theme, &reasoning, label, frame, 0,
                            ));
                            assert_eq!(frame_colors.len(), label.chars().count());
                            for (index, color) in frame_colors.iter().enumerate() {
                                assert_eq!(
                                    chroma(*color),
                                    resting_chroma,
                                    "{background:?}/{depth:?}/{lab:?} {label} frame {frame} cell \
                                     {index}: {color:?} is chromatic; a neutral identity may \
                                     never gain hue while it shimmers"
                                );
                                if *color != resting {
                                    moved = true;
                                }
                            }
                        }
                        assert!(
                            moved,
                            "{background:?}/{depth:?}/{label}: the neutral shimmer must still \
                             move in luminance"
                        );
                    } else {
                        // No identity: every ramp entry must be an exact grey,
                        // whatever the hue rotation, so the highlight can only
                        // take chroma away from the theme's placeholder accent.
                        let ramp = ActivityRamp::of(label).expect("status ramp");
                        for (step, hue_step) in ACTIVITY_RAMP_HUE_STEPS.iter().enumerate() {
                            let entry = ramp.accent(ActivityIdentity { color: None }, step, 0.5);
                            assert_eq!(
                                chroma(entry),
                                0.0,
                                "{background:?}/{depth:?}/{label}: ramp entry {step} must be an \
                                 exact grey when the session has no model identity, got \
                                 {entry:?} for hue rotation {:.0} degrees",
                                ACTIVITY_RAMP_HUE_SPAN * hue_step
                            );
                        }
                        let most_chromatic = colors
                            .iter()
                            .map(|color| chroma(*color))
                            .fold(0.0, f64::max);
                        assert!(
                            most_chromatic <= palette_chroma + NEUTRAL_CHROMA_TOLERANCE,
                            "{background:?}/{depth:?}/{lab:?} {label}: the ramp may only \
                             desaturate the placeholder accent; {most_chromatic:.3} of chroma \
                             exceeds the palette's own {palette_chroma:.3}"
                        );
                    }
                    // The centre sits on the first grapheme at frame 7
                    // (`centre_frame(0)`); the last grapheme of "Working" /
                    // "Thinking" is seven cells away, so the same frame shows
                    // the lit highlight and an unlit cell side by side.
                    let far = *colors.last().expect("trailing grapheme");
                    if background != TerminalBackground::Unknown {
                        assert_eq!(
                            far, resting,
                            "{background:?}/{depth:?}/{lab:?} {label}: a cell outside the \
                             sweep window must show the resting colour"
                        );
                    } else {
                        // An unknown background keeps the established constant
                        // fallback glow, so far cells show the resting colour
                        // moved 28% toward the identity. Either way they are
                        // achromatic, asserted above.
                        assert_eq!(
                            far,
                            colors[colors.len() - 2],
                            "{label}: the fallback glow is constant outside the window"
                        );
                    }
                    let moved = (luminance(colors[0]) - luminance(far)).abs();
                    assert!(
                        moved >= MIN_LUMINANCE_FRACTION_OF_SEPARATION * separation,
                        "{background:?}/{depth:?}/{lab:?} {label}: the centre {colors:?} moves \
                         only {moved:.3} of luminance from the resting colour {resting:?} \
                         (separation {separation:.3})"
                    );
                }
            }
        }
    }
}

/// Both status labels must read as **one** colour identity per model: the
/// same hue family as the resting label and as each other. The states are
/// separated by a non-chromatic cue instead - the sweep's luminance range
/// (documented on [`ActivityRamp::sweep_depth`]) - because hue is what made
/// the reported `Working`/`Thinking` pair read as two unrelated palettes.
#[test]
fn working_and_thinking_share_one_hue_family_and_differ_by_brightness() {
    // Same justification as the neutral test's chroma tolerance: one
    // ANSI256 cube step is 40/255 = 0.157, so 12 degrees of hue tolerance
    // only absorbs quantization, not a different palette.
    const HUE_TOLERANCE_DEGREES: f64 = 12.0;
    // The documented cue: `Working` travels the full proven separation and
    // `Thinking` `ACTIVITY_THINKING_SWEEP_DEPTH` of it, so their centres
    // differ by `(1 - 0.80) = 0.20` of the separation. Asserting 0.15 keeps
    // room for the tint's own rounding while staying far above encoder
    // noise.
    const MIN_STATE_LUMINANCE_FRACTION: f64 = 0.15;

    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = classic_theme_for(
            background,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        );
        for lab in [
            Some(ModelLab::Alibaba),
            Some(ModelLab::Meta),
            Some(ModelLab::Google),
            Some(ModelLab::Microsoft),
            Some(ModelLab::Anthropic),
        ] {
            let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(lab);
            let (baseline, sweep) =
                activity_shimmer_palette(&theme, &reasoning).expect("activity palette");
            let separation = (luminance(baseline) - luminance(sweep)).abs();
            let identity = theme.model_rgb(lab).expect("lab colour");
            let model_hue = hue_degrees(identity);

            let mut centres = Vec::new();
            for label in ["Working", "Thinking"] {
                let rendered = activity_shimmer_label(&theme, &reasoning, label, 7, 0);
                let center = rendered_foregrounds(&rendered)[0];
                let hue = hue_degrees(center);
                let delta = (hue - model_hue).abs().min(360.0 - (hue - model_hue).abs());
                assert!(
                    delta <= ACTIVITY_RAMP_HUE_SPAN + HUE_TOLERANCE_DEGREES,
                    "{background:?}/{lab:?} {label}: centre {center:?} has hue {hue:.1}, \
                     {delta:.1} degrees from the model hue {model_hue:.1}; the sweep may only \
                     move inside the model's own colour family"
                );
                centres.push(center);
            }
            let first_hue = hue_degrees(centres[0]);
            let second_hue = hue_degrees(centres[1]);
            let between = (first_hue - second_hue)
                .abs()
                .min(360.0 - (first_hue - second_hue).abs());
            assert!(
                between <= HUE_TOLERANCE_DEGREES,
                "{background:?}/{lab:?}: the two status labels must share one hue family - the \
                 *same* rotation applied to the model hue, not a mirror of it - got \
                 {first_hue:.1} and {second_hue:.1}, {between:.1} degrees apart"
            );
            let state_delta = (luminance(centres[0]) - luminance(centres[1])).abs();
            assert!(
                state_delta >= MIN_STATE_LUMINANCE_FRACTION * separation,
                "{background:?}/{lab:?}: Working {centres:?} and Thinking must still be \
                 distinguishable by the documented brightness cue; they differ by only \
                 {state_delta:.4} of luminance (separation {separation:.3})"
            );
        }
    }
}

/// The non-chromatic cue, stated as an invariant: at the *same* luminance
/// both status ramps must resolve to the exact same colour. Hue is therefore
/// never a cue between the two statuses - the only thing that separates them
/// is how far [`ActivityRamp::sweep_depth`] carries the highlight from the
/// resting colour (its luminance range), so a chromatic model's `Working`
/// and `Thinking` share one hue family and a neutral model's stay grey.
///
/// Before this invariant held, each label applied the ramp rotation with its
/// own sign (`Working` +, `Thinking` -), up to `2 * ACTIVITY_RAMP_HUE_SPAN`
/// = 48 degrees apart at the outermost ramp step: two shimmer colours out of
/// one model identity, which is precisely the "different colored shimmers"
/// report.
#[test]
fn both_status_ramps_apply_the_same_hue_rotation() {
    // The cue that *does* separate the statuses is non-chromatic and lives
    // in the sweep, never in the accent above: `Working` travels the
    // profile's full proven separation, `Thinking` a shallower one of the
    // same colour, so the two remain distinguishable even when the identity
    // is a pure grey that has no hue to differ by.
    assert_eq!(ActivityRamp::Working.sweep_depth(), 1.0);
    assert_eq!(
        ActivityRamp::Thinking.sweep_depth(),
        ACTIVITY_THINKING_SWEEP_DEPTH
    );
    assert!(ActivityRamp::Thinking.sweep_depth() < ActivityRamp::Working.sweep_depth());

    let theme = theme::test_theme();
    for lab in [
        Some(ModelLab::Alibaba),
        Some(ModelLab::Meta),
        Some(ModelLab::Google),
        Some(ModelLab::Microsoft),
        Some(ModelLab::Anthropic),
    ] {
        let identity = ActivityIdentity::for_model(
            &theme,
            &AssistantBlock::streaming_reasoning("").with_model_lab(lab),
        );
        let model = identity.color.expect("chromatic lab identity");
        for normal in [0.25, 0.5, 0.85] {
            for step in 0..ACTIVITY_RAMP_ENTRIES {
                assert_eq!(
                    ActivityRamp::Working.accent(identity, step, normal),
                    ActivityRamp::Thinking.accent(identity, step, normal),
                    "{lab:?} step {step} at luminance {normal}: both status ramps must apply the \
                     *same* rotation to the model hue {model:?} (hue {:.1}), not mirrored \
                     rotations",
                    hue_degrees(model)
                );
            }
            assert_ne!(
                ActivityRamp::Working.accent(identity, ACTIVITY_RAMP_ENTRIES - 1, normal),
                ActivityRamp::Working.accent(identity, 0, normal),
                "{lab:?}: the ramp must still travel inside the model's own hue family"
            );
        }
    }
}

/// The chroma-weight half of the same rule: the rotation and saturation
/// boost are scaled by the **model colour's own** HSV saturation, so an
/// identity that is only just not grey gets only a fraction of the hue span
/// - and a themed model colour that is nearly grey, or a future lab colour
///   that is, stays nearly luminance-only. Nothing here reads a model id or a
///   lab name: the fixture is a custom theme and the weight is read back from
///   whatever colour that theme resolves.
#[test]
fn near_neutral_model_colours_rotate_only_as_far_as_their_own_chroma_allows() {
    // Equal red/green with a hint of blue: not grey (HSV saturation 0.08 on
    // the dark profile, 0.13 on the light one) but far from the point where
    // a full `ACTIVITY_RAMP_HUE_SPAN` rotation is the model's own hue.
    const NEAR_NEUTRAL_OVERRIDE: &str = "[model]\nopenai = \"#6a6a7a\"\n";
    // One 8-bit channel step on a near-grey colour is worth several degrees
    // of measured hue, so the allowance absorbs the encoder only. It is far
    // below the 24-degree span the defect applied at every ramp entry.
    const HUE_TOLERANCE_DEGREES: f64 = 5.0;

    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = theme::test_theme_source_with(
            NEAR_NEUTRAL_OVERRIDE,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            background,
        );
        let reasoning =
            AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::OpenAi));
        let model = theme
            .model_rgb(Some(ModelLab::OpenAi))
            .expect("the custom theme resolves the lab colour");
        let saturation = activity_hsv(model).saturation;
        assert!(
            saturation > ACTIVITY_NEUTRAL_SATURATION && saturation < ACTIVITY_CHROMATIC_SATURATION,
            "{background:?}: the fixture must be near-neutral, got {model:?} with HSV \
             saturation {saturation:.3}"
        );
        let weight = activity_chromatic_weight(saturation);
        assert!(
            weight < 0.5,
            "{background:?}: a near-neutral colour must carry a small chroma weight, got \
             {weight:.3}"
        );

        let identity = ActivityIdentity::for_model(&theme, &reasoning);
        assert_eq!(
            identity.color,
            Some(model),
            "the weight must be derived from the colour the session's model resolves to"
        );
        let model_hue = hue_degrees(model);
        let bound = weight * ACTIVITY_RAMP_HUE_SPAN;
        for label in ["Working", "Thinking"] {
            let ramp = ActivityRamp::of(label).expect("status ramp");
            for normal in [0.35, 0.6] {
                for step in 0..ACTIVITY_RAMP_ENTRIES {
                    let accent = ramp.accent(identity, step, normal);
                    let hue = hue_degrees(accent);
                    let delta = (hue - model_hue).abs().min(360.0 - (hue - model_hue).abs());
                    assert!(
                        delta <= bound + HUE_TOLERANCE_DEGREES,
                        "{background:?}/{label} step {step} at luminance {normal}: {accent:?} \
                         has hue {hue:.1}, {delta:.1} degrees from the model hue \
                         {model_hue:.1}; this identity's chroma weight is {weight:.3}, so it \
                         may rotate by {bound:.1} of the {} degrees the ramp can reach",
                        ACTIVITY_RAMP_HUE_SPAN
                    );
                }
            }
        }
    }
}

/// The max/ultra rainbow is the one deliberate exception to "one colour
/// family per model", and it stays gated to that emphasis level only.
#[test]
fn max_and_ultra_rainbow_stays_gated_to_that_emphasis_level_only() {
    // The gate itself: `status_rainbow_strength_at` is the only producer of
    // a non-zero strength, and it returns zero for every level other than
    // `max`/`ultra` and outside the two-second window.
    for level in ["off", "minimal", "low", "medium", "high"] {
        assert_eq!(
            crate::tui::view::status_rainbow_strength_at(Some(level), Some(Duration::ZERO)),
            0,
            "{level} must never reach the rainbow branch"
        );
    }
    assert_eq!(
        crate::tui::view::status_rainbow_strength_at(Some("max"), Some(Duration::ZERO)),
        100
    );

    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = classic_theme_for(
            background,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        );
        // A chromatic *and* a neutral identity: the gate must not depend on
        // the model. `Working` is the label the emphasis branch targets.
        for lab in [Some(ModelLab::OpenAi), Some(ModelLab::Alibaba)] {
            let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(lab);
            let at_high = activity_shimmer_label(&theme, &reasoning, "Working", 7, 0);
            let at_max = activity_shimmer_label(&theme, &reasoning, "Working", 7, 100);
            assert_ne!(at_high, at_max);
            // The emphasis is scoped to `Working`: `Thinking` renders its
            // own model ramp identically with and without the emphasis, so
            // the pair still reads as one identity at `max`/`ultra`.
            assert_eq!(
                activity_shimmer_label(&theme, &reasoning, "Thinking", 7, 0),
                activity_shimmer_label(&theme, &reasoning, "Thinking", 7, 100),
                "{background:?}/{lab:?}: the rainbow must never reach Thinking"
            );

            let rainbow = activity_rainbow_color(background, 0, 7);
            assert_eq!(
                rendered_foregrounds(&at_max)[0],
                rainbow,
                "{background:?}/{lab:?}: at max/ultra the whole-label rainbow must be intact"
            );
            // At `high` (strength 0) no rendered cell may carry the
            // rainbow's chroma.
            for color in rendered_foregrounds(&at_high) {
                assert!(
                    chroma(color) <= 0.02 || lab == Some(ModelLab::Alibaba),
                    "{background:?}/{lab:?}: {color:?} carries chroma at a non-emphasis level"
                );
            }
            // The literal defect: an orange-yellow rainbow cell on a neutral
            // model at `high`.
            if lab == Some(ModelLab::OpenAi) {
                for color in rendered_foregrounds(&at_high) {
                    assert_eq!(
                        chroma(color),
                        0.0,
                        "{background:?}: {color:?} is chromatic on a neutral identity at high"
                    );
                }
            }
        }
    }
}

/// The sweep must enter from before the margin dot, cross every label cell
/// in order, and leave past the trailing edge before the cycle repeats.
#[test]
fn the_sweep_traverses_every_label_cell_in_order_before_it_loops() {
    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = classic_theme_for(
            background,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        );
        for label in ["Working", "Thinking", "Compacting context"] {
            let reasoning = activity_reasoning(Some(ModelLab::Alibaba), label);
            let resting = resting_colour(&theme, &reasoning);
            let cycle = activity_cycle(label);
            // The traverse starts before the leading edge and ends past the
            // trailing edge, with `ACTIVITY_SWEEP_HALF` cells of slack at
            // both ends for the band to fall off, so the highlight crosses
            // the whole label instead of "sliding part way through the
            // letters" and back.
            let first_center = ACTIVITY_SWEEP_START;
            let last_center = ACTIVITY_SWEEP_START + cycle as isize - 1;
            assert!(
                first_center + ACTIVITY_SWEEP_HALF < 0,
                "{label}: the sweep must begin before the leading edge, got {first_center}"
            );
            assert!(
                last_center - ACTIVITY_SWEEP_HALF >= label.width() as isize,
                "{label}: the sweep must end past the trailing edge, got {last_center}"
            );
            assert!(
                cycle
                    >= label.width()
                        + 2 * ACTIVITY_SWEEP_HALF as usize
                        + ACTIVITY_SWEEP_REST_FRAMES,
                "{label}: a {cycle}-frame cycle leaves no rest gap"
            );
            let mut peaks = Vec::new();
            for index in 0..label.width() as isize {
                let frame = centre_frame(index);
                assert!(
                    frame < cycle,
                    "{label}: cell {index} peaks outside its own cycle"
                );
                peaks.push(frame);

                let luminances = shimmer_cell_luminances(&theme, &reasoning, label, frame, 0);
                let center = luminances[index as usize];
                let farthest = luminances
                    .iter()
                    .enumerate()
                    .map(|(cell, value)| (cell as isize, (value - luminance(resting)).abs()))
                    .max_by(|left, right| left.1.total_cmp(&right.1))
                    .map(|(cell, _)| cell)
                    .expect("cells");
                assert_eq!(
                    farthest, index,
                    "{background:?}/{label}: at frame {frame} cell {farthest} is the most lit, \
                     not cell {index}"
                );
                assert!(
                    (center - luminance(resting)).abs() > 0.0,
                    "{background:?}/{label}: cell {index} is never lit"
                );
            }
            // One cell per frame, in order, left to right.
            assert!(
                peaks.windows(2).all(|window| window[0] < window[1]),
                "{label}: the sweep must visit the label monotonically, got {peaks:?}"
            );
            assert_eq!(
                peaks[1] - peaks[0],
                1,
                "{label}: the highlight must advance exactly one cell per frame"
            );
        }
    }
}

/// The reported artefact: "it slides part way through the letters then loops
/// back". Every cell used to stay partly lit at the end of a cycle, so the
/// next cycle started with the leading edge already at ~half brightness.
/// The loop must now rest for the cycle's gap frames and never jump.
#[test]
fn the_sweep_rests_between_cycles_and_never_teleports() {
    // A cell may only ever move by *one* sweep step: the highlight arriving
    // one position closer, or leaving one position further behind. The bound
    // is the falloff ladder's own largest adjacent step, measured through the
    // very blend an untinted cell uses, so it needs no hand-tuned slack. The
    // reported teleport - a trailing cell snapped from 64% lit straight to
    // rest, 0.64 of the separation - is several times this bound, and the
    // ramp's own per-tick advance cannot add to it: every entry is pinned to
    // the untinted cell's luminance and blended in linear light.
    //
    // What is left over is rounding: the tint's linear-light blend rounds each
    // channel to 8 bits, and one channel step is ~0.0036 of relative luminance
    // at this band. 0.005 covers the round trip with nothing to spare.
    const TINT_ROUNDING_ALLOWANCE: f64 = 0.005;

    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = classic_theme_for(
            background,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        );
        for label in ["Working", "Thinking", "Compacting context"] {
            let reasoning = activity_reasoning(Some(ModelLab::Alibaba), label);
            let (baseline, sweep) =
                activity_shimmer_palette(&theme, &reasoning).expect("activity palette");
            let separation = (luminance(baseline) - luminance(sweep)).abs();
            let bound = one_sweep_step_luminance(baseline, sweep);
            let resting = resting_colour(&theme, &reasoning);
            let resting_marker = activity_shimmer_marker(&theme, &reasoning, 0, 0, "•");
            let cycle = activity_cycle(label);
            let mut rest_frames = 0;
            let mut first_frame_at_rest = false;
            let mut last_frame_at_rest = false;
            let mut previous: Option<Vec<f64>> = None;
            let mut first = Vec::new();
            let mut largest_step: f64 = 0.0;

            for frame in 0..cycle {
                let colors: Vec<Rgb> = rendered_foregrounds(&activity_shimmer_label(
                    &theme, &reasoning, label, frame, 0,
                ));
                let luminances = colors.iter().copied().map(luminance).collect::<Vec<_>>();
                let all_rest = colors.iter().all(|color| *color == resting)
                    && activity_shimmer_marker(&theme, &reasoning, frame, 0, "•") == resting_marker;
                if all_rest {
                    rest_frames += 1;
                }
                if frame == 0 {
                    first_frame_at_rest = all_rest;
                }
                if frame + 1 == cycle {
                    last_frame_at_rest = all_rest;
                }
                // Nothing outside the label's own cells may be lit: only
                // configured cells are rendered at all, and a lit one must
                // sit inside the falloff window of the current centre. (The
                // margin dot is the deliberate exception: it shares the
                // shimmer's coordinate space and is asserted at rest here
                // with the label.)
                let center = (frame % cycle) as isize + ACTIVITY_SWEEP_START;
                for (cell, color) in colors.iter().enumerate() {
                    if *color != resting {
                        let distance = (cell as isize - center).abs();
                        assert!(
                            distance < ACTIVITY_SWEEP_FALLOFF.len() as isize,
                            "{background:?}/{label} frame {frame}: cell {cell} is lit \
                             {distance} cells from the centre {center}"
                        );
                    }
                }
                if let Some(previous) = previous.as_deref() {
                    for (before, after) in previous.iter().zip(&luminances) {
                        largest_step = largest_step.max((before - after).abs());
                    }
                } else {
                    first = luminances.clone();
                }
                previous = Some(luminances);
            }
            // The loop seam: the last frame back to the first.
            if let Some(previous) = previous.as_deref() {
                for (before, after) in previous.iter().zip(&first) {
                    largest_step = largest_step.max((before - after).abs());
                }
            }

            assert!(
                rest_frames >= ACTIVITY_SWEEP_REST_FRAMES,
                "{background:?}/{label}: only {rest_frames} of {cycle} frames show the resting \
                 colour everywhere; the loop needs a rest gap"
            );
            assert!(
                first_frame_at_rest && last_frame_at_rest,
                "{background:?}/{label}: the cycle must open and close with every cell at \
                 rest, got {first_frame_at_rest} and {last_frame_at_rest}"
            );
            assert!(
                largest_step <= bound + TINT_ROUNDING_ALLOWANCE,
                "{background:?}/{label}: one cell changes by {largest_step:.4} of luminance \
                 between frames, more than the largest single sweep step {bound:.4} (plus \
                 {TINT_ROUNDING_ALLOWANCE:.3} of 8-bit rounding); the highlight is jumping, \
                 not travelling (separation {separation:.4})"
            );
            assert_eq!(
                activity_shimmer_label(&theme, &reasoning, label, 0, 0),
                activity_shimmer_label(&theme, &reasoning, label, cycle, 0),
                "{background:?}/{label}: the period must equal the cycle length"
            );
        }
    }
}

/// "No cell outside the label is lit" in its literal form: the only lit cells
/// are rendered cells inside the falloff window. Every index the sweep cannot
/// reach - including the indices before the margin dot and past the trailing
/// label cell, which are never rendered at all - resolves to the resting
/// foreground byte for byte, and the frames on which the whole label is at
/// rest are frames on which the dot (the one rendered cell outside the label)
/// is at rest too.
#[test]
fn the_sweep_lights_no_cell_outside_the_label() {
    // A band wider than the label and its margin dot together on each side,
    // so the assertion covers cells that do not exist on screen as well.
    const OUTSIDE_MARGIN: isize = 12;

    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = classic_theme_for(
            background,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        );
        for lab in [Some(ModelLab::OpenAi), Some(ModelLab::Alibaba)] {
            for label in ["Working", "Thinking", "Compacting context"] {
                let reasoning = activity_reasoning(lab, label);
                let (baseline, sweep) =
                    activity_shimmer_palette(&theme, &reasoning).expect("activity palette");
                let identity = ActivityIdentity::for_model(&theme, &reasoning);
                let resting = resting_colour(&theme, &reasoning);
                let resting_marker = activity_shimmer_marker(&theme, &reasoning, 0, 0, "•");
                let cycle = activity_cycle(label);
                let mut rest_frames = Vec::new();

                for frame in 0..cycle {
                    let center = (frame % cycle) as isize + ACTIVITY_SWEEP_START;
                    for index in (ACTIVITY_MARKER_INDEX - OUTSIDE_MARGIN)
                        ..=(label.width() as isize + OUTSIDE_MARGIN)
                    {
                        let distance = (index - center).abs();
                        if distance < ACTIVITY_SWEEP_FALLOFF.len() as isize {
                            continue;
                        }
                        let color = activity_shimmer_color(
                            &theme, identity, baseline, sweep, background, label, index, frame, 0,
                        );
                        assert_eq!(
                            color, baseline,
                            "{background:?}/{lab:?}/{label} frame {frame}: index {index} is \
                             {distance} cells from the centre {center} and must be at rest"
                        );
                    }

                    // The rendered row: the margin dot and every label cell.
                    let lit = rendered_foregrounds(&activity_shimmer_label(
                        &theme, &reasoning, label, frame, 0,
                    ))
                    .iter()
                    .any(|color| *color != resting)
                        || activity_shimmer_marker(&theme, &reasoning, frame, 0, "•")
                            != resting_marker;
                    if !lit {
                        rest_frames.push(frame);
                    }
                }

                assert_eq!(
                    rest_frames,
                    vec![0, cycle - 1],
                    "{background:?}/{lab:?}/{label}: the cycle must begin and end with nothing \
                     at all lit, out of {cycle} frames"
                );
            }
        }
    }
}

/// The ramp contributes **hue and chroma only** - the property every accent
/// is built on. That only holds if the blend which applies it cannot move a
/// cell's luminance; otherwise the ramp's own per-tick advance adds a second,
/// unswept motion on top of the highlight's (measured before the linear-light
/// blend landed: 0.031 of the dark profile's separation on `Working`, a small
/// echo of the reported jump).
#[test]
fn the_tint_blend_is_luminance_exact() {
    // The blend rounds each channel to 8 bits and one channel step is ~0.0036
    // of relative luminance at this band; the measured worst case over every
    // ramp entry and falloff step is 0.0041 (dark) / 0.0011 (light).
    const ROUND_TRIP_ALLOWANCE: f64 = 0.005;

    for value in 0..=u8::MAX {
        assert_eq!(
            activity_srgb_channel(activity_linear_channel(value)),
            value,
            "the sRGB <-> linear conversions must round-trip exactly at channel {value}"
        );
    }

    for background in [TerminalBackground::Dark, TerminalBackground::Light] {
        let theme = classic_theme_for(
            background,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        );
        for lab in [Some(ModelLab::OpenAi), Some(ModelLab::Alibaba)] {
            for label in ["Working", "Thinking"] {
                let reasoning = activity_reasoning(lab, label);
                let (baseline, sweep) =
                    activity_shimmer_palette(&theme, &reasoning).expect("activity palette");
                let identity = ActivityIdentity::for_model(&theme, &reasoning);
                let ramp = ActivityRamp::of(label).expect("status ramp");
                for strength in [0u16, 18, 40, 64, 84, 100] {
                    let normal = swept_colour(baseline, sweep, strength);
                    for step in 0..ACTIVITY_RAMP_ENTRIES {
                        let accent = ramp.accent(identity, step, luminance(normal));
                        let mixed = activity_linear_mix(normal, accent, strength);
                        assert!(
                            (luminance(mixed) - luminance(normal)).abs() <= ROUND_TRIP_ALLOWANCE,
                            "{background:?}/{lab:?}/{label}: ramp entry {step} at {strength}% of \
                             the sweep moved a {normal:?} cell ({:.4}) to {mixed:?} ({:.4}); \
                             the tint may carry hue and chroma only",
                            luminance(normal),
                            luminance(mixed)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn model_shimmer_keeps_a_muted_baseline_behind_a_moving_highlight() {
    let theme = theme::test_theme();
    let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
    let model = theme
        .model_rgb(Some(ModelLab::Alibaba))
        .expect("model colour");
    let shadow = theme.composer_idle_rgb(model);
    // `Working` travels the profile's full separation, so its highlight
    // reaches the model's own colour exactly; `Thinking` travels
    // `ACTIVITY_THINKING_SWEEP_DEPTH` of it, i.e. the documented non-chromatic
    // difference between the two statuses.
    let rendered = activity_shimmer_label(&theme, &reasoning, "Working", centre_frame(0), 0);
    let colors = foreground_color_codes(&rendered);
    assert_eq!(colors.len(), "Working".chars().count(), "{rendered:?}");
    assert_eq!(colors[0], format!("{};{};{}", model.0, model.1, model.2));
    assert_ne!(colors[0], format!("{};{};{}", shadow.0, shadow.1, shadow.2));
    assert_ne!(&colors[0], colors.last().expect("last shimmer colour"));
    assert!(!rendered.contains("\x1b[48;"), "{rendered:?}");

    let thinking = activity_shimmer_label(&theme, &reasoning, "Thinking", centre_frame(0), 0);
    let thinking_colors = foreground_color_codes(&thinking);
    let expected = activity_blend(shadow, model, ACTIVITY_THINKING_SWEEP_DEPTH);
    assert_eq!(
        thinking_colors[0],
        format!("{};{};{}", expected.0, expected.1, expected.2),
        "Thinking must reach the same colour identity at a shallower depth"
    );
    assert_ne!(
        thinking_colors[0], colors[0],
        "the two statuses must remain distinguishable"
    );
}

#[test]
fn shimmer_reaches_every_grapheme_and_loops_at_the_label_width() {
    let theme = theme::test_theme();
    let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
    for label in [
        "",
        "A",
        "Working",
        "Thinking",
        "Compacting context",
        "A considerably longer activity label",
        "压缩 e\u{301} 👩‍💻 context",
    ] {
        let cycle = activity_cycle(label);
        // Every grapheme must reach the *same* highlight colour - the
        // label's own sweep endpoint - when the centre arrives on it.
        let peak = foreground_color_codes(&activity_shimmer_label(
            &theme,
            &reasoning,
            label,
            centre_frame(0),
            0,
        ))
        .into_iter()
        .next();
        let mut cell = 0;
        for (index, grapheme) in label.graphemes(true).enumerate() {
            let frame = centre_frame(cell as isize);
            let rendered = activity_shimmer_label(&theme, &reasoning, label, frame, 0);
            assert_eq!(strip_terminal_sequences(&rendered), label);
            let reached = &foreground_color_codes(&rendered)[index];
            if let Some(peak) = peak.as_deref() {
                assert_eq!(reached, peak, "{label}: {index}");
            }
            assert!(!rendered.contains("\x1b[48;"));
            cell += grapheme.width();
        }
        assert_eq!(
            activity_shimmer_label(&theme, &reasoning, label, 0, 0),
            activity_shimmer_label(&theme, &reasoning, label, cycle, 0),
        );
    }
}

#[test]
fn max_rainbow_shimmer_moves_right_at_one_status_frame_per_step() {
    let theme = theme::test_theme();
    let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
    let frame_zero = foreground_color_codes(&activity_shimmer_label(
        &theme, &reasoning, "Working", 0, 100,
    ));
    let frame_one = foreground_color_codes(&activity_shimmer_label(
        &theme, &reasoning, "Working", 1, 100,
    ));
    let frame_two = foreground_color_codes(&activity_shimmer_label(
        &theme, &reasoning, "Working", 2, 100,
    ));

    assert_eq!(frame_zero.len(), "Working".chars().count());
    assert_eq!(&frame_one[1..], &frame_zero[..6]);
    assert_eq!(&frame_two[2..], &frame_zero[..5]);
}

#[test]
fn activity_marker_shares_the_status_shimmer_phase() {
    let theme = theme::test_theme();
    let reasoning =
        AssistantBlock::streaming_reasoning("private").with_model_lab(Some(ModelLab::Alibaba));
    let first = activity_shimmer_marker(&theme, &reasoning, 0, 0, "•");
    let next = activity_shimmer_marker(&theme, &reasoning, 1, 0, "•");

    assert_ne!(first, next);
    assert!(first.contains("\x1b[38;2;"), "{first:?}");
    assert!(!first.contains("\x1b[48;"), "{first:?}");
    let model = theme
        .model_rgb(Some(ModelLab::Alibaba))
        .expect("model colour");
    let shadow = theme.composer_idle_rgb(model);
    let expected = theme.rgb_fg(
        activity_shimmer_color(
            &theme,
            ActivityIdentity::for_model(&theme, &reasoning),
            shadow,
            model,
            TerminalBackground::Unknown,
            "Thinking",
            ACTIVITY_MARKER_INDEX,
            0,
            0,
        ),
        "•",
    );
    assert_eq!(first, expected);
}

#[test]
fn retry_marker_uses_the_displayed_label_shimmer_cycle() {
    use super::super::assistant_block::RetryActivity;
    let theme = theme::test_theme();
    let mut reasoning =
        AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
    reasoning.retry_activity = Some(RetryActivity {
        operation: None,
        attempt: 12,
        max_attempts: None,
        delay: Duration::ZERO,
        observed_at: Instant::now(),
    });
    let label = reasoning
        .retry_activity
        .as_ref()
        .unwrap()
        .label_at(Instant::now());
    let model = theme.model_rgb(Some(ModelLab::Alibaba)).unwrap();
    let shadow = theme.composer_idle_rgb(model);
    for frame in 0..64 {
        assert_eq!(
            activity_shimmer_marker(&theme, &reasoning, frame, 0, "•"),
            theme.rgb_fg(
                activity_shimmer_color(
                    &theme,
                    ActivityIdentity::for_model(&theme, &reasoning),
                    shadow,
                    model,
                    TerminalBackground::Unknown,
                    &label,
                    ACTIVITY_MARKER_INDEX,
                    frame,
                    0,
                ),
                "•"
            ),
        );
    }
}

#[test]
fn activity_durations_use_compact_clock_units() {
    assert_eq!(format_activity_duration(28), "28s");
    assert_eq!(format_activity_duration(376), "6m16s");
    assert_eq!(format_activity_duration(3661), "1h01m01s");
}

#[test]
fn working_status_reports_root_run_elapsed_time_and_interrupt_hint() {
    let theme = theme::test_theme_with(TerminalCapabilities::test(false, true, ColorDepth::None));
    let mut working = AssistantBlock::streaming_reasoning("")
        .with_model_lab(Some(ModelLab::OpenAi))
        .with_activity_started_at(Some(Instant::now() - Duration::from_millis(28_100)));
    working.reasoning_heading = Some("Working".into());
    working.show_reasoning_hint = false;

    let rendered = collapsed_reasoning_lines_at(&theme, &working, 0, 0);
    assert_eq!(rendered, vec!["Working (28s • esc to interrupt)"]);
}

#[test]
fn retry_status_keeps_interrupt_hint_without_run_elapsed() {
    use super::super::assistant_block::RetryActivity;

    let theme = theme::test_theme_with(TerminalCapabilities::test(false, true, ColorDepth::None));
    let now = Instant::now();
    let mut working = AssistantBlock::streaming_reasoning("");
    working.reasoning_heading = Some("Working".into());
    working.show_reasoning_hint = false;
    working.retry_activity = Some(RetryActivity {
        operation: None,
        attempt: 2,
        max_attempts: Some(3),
        delay: Duration::from_secs(5),
        observed_at: now,
    });
    for started_at in [None, Some(now - Duration::from_secs(28))] {
        working.activity_started_at = started_at;
        for elapsed in [0, 6] {
            let label = working
                .retry_activity
                .as_ref()
                .unwrap()
                .label_at(now + Duration::from_secs(elapsed));
            let expected = if elapsed == 0 {
                "Retrying 2/3 in 5s (esc to interrupt)"
            } else {
                "Retrying 2/3 (esc to interrupt)"
            };
            assert_eq!(
                activity_status_line(&theme, &working, &label, 0, 0),
                expected
            );
        }
    }
}

#[test]
fn collapsed_reasoning_without_a_heading_has_one_inline_hint_in_the_compiled_theme() {
    let theme = theme::test_theme();
    let renderer = theme.reasoning_renderer();
    let mut reasoning =
        AssistantBlock::streaming_reasoning("private").with_model_lab(Some(ModelLab::Alibaba));
    let live = render_reasoning(&reasoning, &renderer, &theme, 80, false);
    assert_eq!(live.len(), 1, "{live:?}");
    assert_eq!(
        strip_terminal_sequences(&live[0]),
        "Thinking · Ctrl+O expand"
    );
    assert!(live[0].contains("\x1b[1m"), "{live:?}");

    let custom = theme::test_theme_from_source("");
    let legacy = render_reasoning(&reasoning, &custom.reasoning_renderer(), &custom, 80, false);
    assert_eq!(legacy.len(), 2, "{legacy:?}");
    assert_eq!(strip_terminal_sequences(&legacy[1]), "└ (ctrl+o to expand)");

    reasoning.reasoning_elapsed = Some(Duration::from_millis(13_700));
    reasoning.finish_reasoning();
    let settled = render_reasoning(&reasoning, &renderer, &theme, 80, false);
    assert!(
        settled.is_empty(),
        "finished reasoning leaves no trace when collapsed"
    );
}

#[test]
fn collapsed_reasoning_has_ascii_fallback_and_width_bounded_rows() {
    let theme = theme::test_theme_with(TerminalCapabilities::test(false, false, ColorDepth::None));
    let reasoning = AssistantBlock::streaming_reasoning(
        "## A heading that is intentionally much wider than the viewport\n",
    );
    let lines = render_reasoning(&reasoning, &theme.reasoning_renderer(), &theme, 16, false);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0], "Thinking", "{lines:?}");
    assert!(lines[1].starts_with("`- A heading"), "{lines:?}");
    assert!(lines.iter().all(|line| visible_width(line) <= 16));
    assert!(lines.iter().all(|line| !line.contains('\x1b')));
}

#[test]
fn two_line_thinking_reserves_its_detail_row_while_working() {
    let still =
        theme::test_theme_from_source(include_str!("../../../../../../examples/themes/Still.toml"));
    let mut activity = AssistantBlock::streaming_reasoning("");
    activity.reasoning_heading = Some("Working".into());
    activity.show_reasoning_hint = false;
    for width in [48, 160] {
        let working =
            render_reasoning(&activity, &still.reasoning_renderer(), &still, width, false);
        assert_eq!(working.len(), 2, "{width}: {working:?}");
        assert!(strip_terminal_sequences(&working[0]).starts_with("Working"));
        assert!(
            working[1].is_empty(),
            "reserved row must be blank: {working:?}"
        );

        activity.reasoning_heading = None;
        activity.show_reasoning_hint = true;
        activity.append_reasoning("private detail");
        let thinking =
            render_reasoning(&activity, &still.reasoning_renderer(), &still, width, false);
        assert_eq!(
            thinking.len(),
            working.len(),
            "composer would shift at {width}"
        );
        assert!(strip_terminal_sequences(&thinking[0]).starts_with("Thinking"));
        assert!(strip_terminal_sequences(&thinking[1]).contains("ctrl+o to expand"));
        activity = AssistantBlock::streaming_reasoning("");
        activity.reasoning_heading = Some("Working".into());
        activity.show_reasoning_hint = false;
    }
}

#[test]
fn non_expandable_activity_is_one_truthful_row() {
    let theme = theme::test_theme();
    let mut activity = AssistantBlock::streaming_reasoning("");
    activity.reasoning_heading = Some("Working".into());
    activity.show_reasoning_hint = false;

    let lines = render_reasoning(&activity, &theme.reasoning_renderer(), &theme, 80, false);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(strip_terminal_sequences(&lines[0]), "Working");
    assert!(lines[0].contains("\x1b[1m"), "{lines:?}");

    let verbose = render_reasoning(&activity, &theme.reasoning_renderer(), &theme, 80, true);
    assert_eq!(verbose.len(), 1, "{verbose:?}");
    assert_eq!(strip_terminal_sequences(&verbose[0]), "Working");
}
