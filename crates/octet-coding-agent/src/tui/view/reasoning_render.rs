use sexy_tui_rs::{strip_terminal_sequences, Color, RichRenderer};
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::assistant_block::AssistantBlock;
use super::{activity_elbow, finish_transcript_block, fit_line, subdued_text};
use crate::tui::terminal::ColorDepth;
use crate::tui::theme::{OctetTheme, TerminalBackground};

/// The status label starts two cells after its margin dot (`• `). Keeping the
/// dot in the same coordinate space makes the shimmer travel through it before
/// crossing the label.
const ACTIVITY_LABEL_OFFSET: isize = 2;
const ACTIVITY_MARKER_INDEX: isize = -ACTIVITY_LABEL_OFFSET;
type Rgb = (u8, u8, u8);
const ACTIVITY_RAINBOW: [Rgb; 7] = [
    (255, 96, 96),
    (255, 176, 72),
    (240, 224, 88),
    (104, 220, 128),
    (80, 200, 232),
    (120, 152, 255),
    (216, 120, 240),
];

// These margins leave room for ANSI256 quantization while keeping the status
// readable on representative composited surfaces (#404040 and #e0e0e0). The
// actual terminal background can still differ; the PTY fixture is not a probe
// of arbitrary transparency or a user's physical terminal.
//
// Contrast, not taste, sets the outer bounds. `nearest_ansi256` guarantees
// every emitted cell stays within a 1.2:1 contrast ratio of the requested
// colour, so a requested luminance `L` may reach `1.2 * (L + 0.05) - 0.05`
// after quantization. Against the composite surfaces the existing matrix pins
// (#404040, relative luminance ~0.051, and #e0e0e0, ~0.745):
//
//   dark  floor  0.50: (0.50 + 0.05) / 1.2 = 0.458 -> 0.508 / 0.101 = 5.0:1
//   light ceiling 0.09: 1.2 * 0.14 - 0.05 = 0.118 -> 0.795 / 0.168 = 4.7:1
//
// The *separation* between the two is what was reported as "too subtle". A
// resting foreground must move far enough that the travelling highlight is
// unmistakable, so both profiles now separate by at least 0.35 (dark) and
// 0.08 (light, which the light profile's contrast ceiling caps) of relative
// luminance instead of 0.23 and 0.04 (0.01 -> 0.05, essentially invisible).
const ACTIVITY_DARK_BASE_LUMINANCE: f64 = 0.85;
const ACTIVITY_DARK_SWEEP_LUMINANCE: f64 = 0.50;
const ACTIVITY_LIGHT_BASE_LUMINANCE: f64 = 0.01;
const ACTIVITY_LIGHT_SWEEP_LUMINANCE: f64 = 0.09;

/// The `Working` rainbow is clamped to its own readable band. These are
/// deliberately separate from the resting baselines above: retuning resting
/// contrast must never silently rewrite the established rainbow identity. The
/// dark floor sits inside the sweep band (so the rainbow centre stays a
/// visible darkening) and the light ceiling is the light-profile sweep band,
/// so a rainbow centre always moves at least as far from the resting colour as
/// the plain sweep does.
const ACTIVITY_RAINBOW_DARK_MIN_LUMINANCE: f64 = 0.62;
const ACTIVITY_RAINBOW_LIGHT_MAX_LUMINANCE: f64 = ACTIVITY_LIGHT_SWEEP_LUMINANCE;

/// Upper bound on a tint entry's channel spread. Hues differ in how much
/// chroma their luminance budget allows, so the cap keeps the travelling tint
/// from swinging between pastel and neon.
const ACTIVITY_TINT_MAX_SPREAD: f64 = 170.0;

/// Which chromatic ramp an activity label carries. A ramp is a list of HSV
/// hues; the `Working` and `Thinking` bands are deliberately disjoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityTint {
    /// Warm amber → coral (hue 0°..45°).
    Working,
    /// Cool cyan → violet (hue 190°..262°).
    Thinking,
}

const ACTIVITY_WORKING_HUES: [f64; 4] = [0.0, 14.0, 30.0, 45.0];
const ACTIVITY_THINKING_HUES: [f64; 4] = [190.0, 212.0, 236.0, 262.0];

impl ActivityTint {
    fn hues(self) -> &'static [f64] {
        match self {
            Self::Working => &ACTIVITY_WORKING_HUES,
            Self::Thinking => &ACTIVITY_THINKING_HUES,
        }
    }
}

fn activity_tint(label: &str) -> Option<ActivityTint> {
    match label {
        "Working" => Some(ActivityTint::Working),
        "Thinking" => Some(ActivityTint::Thinking),
        _ => None,
    }
}

/// The channel spread of `hue`'s colour family offset by one grey amount.
///
/// Adding the same amount to all three channels moves luminance monotonically
/// and leaves both the spread and the hue untouched, which is exactly the
/// property the quantized-offset construction below depends on.
fn activity_hue_family(hue: f64, spread: f64, offset: f64) -> Rgb {
    let sector = hue.rem_euclid(360.0) / 60.0;
    let fraction = sector - sector.floor();
    let pure = match sector as usize % 6 {
        0 => (1.0, fraction, 0.0),
        1 => (1.0 - fraction, 1.0, 0.0),
        2 => (0.0, 1.0, fraction),
        3 => (0.0, 1.0 - fraction, 1.0),
        4 => (fraction, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - fraction),
    };
    let channel = |value: f64| (value + offset).round().clamp(0.0, 255.0) as u8;
    (
        channel(pure.0 * spread),
        channel(pure.1 * spread),
        channel(pure.2 * spread),
    )
}

/// Whether `hue`'s `spread`-wide family can contain `target` luminance: its
/// dark end is at or below the target and its bright end at or above it. Both
/// ends move away from the target as the spread grows, so this is monotone.
fn activity_spread_reaches(hue: f64, spread: f64, target: f64) -> bool {
    let dark = activity_hue_family(hue, spread, 0.0);
    if activity_luminance(dark) > target {
        return false;
    }
    let bright = activity_hue_family(hue, spread, 255.0 - spread);
    activity_luminance(bright) >= target
}

/// The widest family of `hue` that still reaches `target` luminance, capped at
/// [`ACTIVITY_TINT_MAX_SPREAD`].
///
/// Hues do not share one luminance budget: at the dark profile's sweep
/// luminance a red entry can only hold a modest spread while a cyan entry
/// holds a large one, and the light profile's low ceiling pushes every entry
/// into the dark end of its family. Taking the most chroma each hue can afford
/// (instead of one nominal value some hues could not reach without leaving the
/// contrast band) keeps the tint inside the palette's promise.
fn activity_tint_spread(hue: f64, target: f64) -> f64 {
    let (mut low, mut high) = (0.0, ACTIVITY_TINT_MAX_SPREAD);
    for _ in 0..18 {
        let spread = (low + high) / 2.0;
        if activity_spread_reaches(hue, spread, target) {
            low = spread;
        } else {
            high = spread;
        }
    }
    low.round()
}

/// Build one tint entry: HSV `hue` degrees, the widest affordable spread, and
/// a relative luminance as close to `target` as the family allows.
fn activity_hue_color(hue: f64, target: f64) -> Rgb {
    let spread = activity_tint_spread(hue, target);
    let (mut low, mut high) = (0.0, 255.0 - spread);
    for _ in 0..24 {
        let amount = (low + high) / 2.0;
        if activity_luminance(activity_hue_family(hue, spread, amount)) < target {
            low = amount;
        } else {
            high = amount;
        }
    }
    activity_hue_family(hue, spread, high)
}

/// Resolve one ramp entry for the current cell.
///
/// The entry is built at the cell's *own* resting luminance, so the tint adds
/// hue and chroma only: it can never move a cell outside the luminance band the
/// plain palette already proved contrast-safe, and the luminance falloff stays
/// the plain one. Pinning to the profile-wide sweep luminance instead would let
/// a tinted neighbour overshoot the centre (measured), which is exactly the
/// "centre is the most distinct cell" property this must keep.
fn activity_tint_color(tint: ActivityTint, luminance: f64, index: isize, shimmer_frame: usize) -> Rgb {
    let hues = tint.hues();
    let hue = hues[ramp_index(hues, index, shimmer_frame)];
    activity_hue_color(hue, luminance)
}

/// The max/ultra emphasis keeps the established whole-label rainbow. It is
/// clamped to its own readable band, separate from the resting baselines, so
/// retuning resting contrast never silently rewrites the rainbow identity. The
/// dark floor sits inside the sweep band and the light ceiling matches the
/// light-profile sweep band, so a rainbow centre still moves at least as far
/// from the resting colour as the plain sweep does.
fn activity_rainbow_color(
    background: TerminalBackground,
    index: isize,
    shimmer_frame: usize,
) -> Rgb {
    let color = ACTIVITY_RAINBOW[ramp_index(&ACTIVITY_RAINBOW, index, shimmer_frame)];
    match background {
        TerminalBackground::Dark => {
            activity_color_at_least(color, ACTIVITY_RAINBOW_DARK_MIN_LUMINANCE)
        }
        TerminalBackground::Light => {
            activity_color_at_most(color, ACTIVITY_RAINBOW_LIGHT_MAX_LUMINANCE)
        }
        TerminalBackground::Unknown => color,
    }
}

fn mix_channel(base: u8, accent: u8, strength_percent: u16) -> u8 {
    let base = u32::from(base);
    let accent = u32::from(accent);
    let strength = u32::from(strength_percent.min(100));
    ((base * (100 - strength) + accent * strength + 50) / 100) as u8
}

fn activity_linear_channel(channel: u8) -> f64 {
    let channel = f64::from(channel) / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn activity_luminance(color: Rgb) -> f64 {
    0.2126 * activity_linear_channel(color.0)
        + 0.7152 * activity_linear_channel(color.1)
        + 0.0722 * activity_linear_channel(color.2)
}

fn activity_blend(source: Rgb, destination: Rgb, amount: f64) -> Rgb {
    let channel = |source: u8, destination: u8| {
        (f64::from(source) + (f64::from(destination) - f64::from(source)) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (
        channel(source.0, destination.0),
        channel(source.1, destination.1),
        channel(source.2, destination.2),
    )
}

fn activity_color_to_luminance(color: Rgb, target: f64, toward_white: bool) -> Rgb {
    let reached = |candidate: Rgb| {
        if toward_white {
            activity_luminance(candidate) >= target
        } else {
            activity_luminance(candidate) <= target
        }
    };
    if reached(color) {
        return color;
    }

    let destination = if toward_white {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    };
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..20 {
        let amount = (low + high) / 2.0;
        if reached(activity_blend(color, destination, amount)) {
            high = amount;
        } else {
            low = amount;
        }
    }
    activity_blend(color, destination, high)
}

fn activity_color_at_least(color: Rgb, target: f64) -> Rgb {
    activity_color_to_luminance(color, target, true)
}

fn activity_color_at_most(color: Rgb, target: f64) -> Rgb {
    activity_color_to_luminance(color, target, false)
}

/// Return a readable foreground baseline and the narrow moving sweep. Both
/// colors are foreground-only; no status character paints a background cell.
/// Known light/dark profiles use conservative luminance margins because a
/// transparent terminal composites these cells over a surface this code cannot see.
fn activity_shimmer_palette(theme: &OctetTheme, reasoning: &AssistantBlock) -> Option<(Rgb, Rgb)> {
    let capabilities = theme.capabilities();
    if !capabilities.animation
        || !capabilities.interactive
        || capabilities.color == ColorDepth::None
    {
        return None;
    }
    let model = theme.model_rgb(reasoning.model_lab)?;
    let palette = match theme.background() {
        TerminalBackground::Dark => {
            let baseline = activity_color_at_least(model, ACTIVITY_DARK_BASE_LUMINANCE);
            let sweep = activity_color_at_most(baseline, ACTIVITY_DARK_SWEEP_LUMINANCE);
            (baseline, sweep)
        }
        TerminalBackground::Light => {
            let baseline = activity_color_at_most(model, ACTIVITY_LIGHT_BASE_LUMINANCE);
            let sweep = activity_color_at_least(baseline, ACTIVITY_LIGHT_SWEEP_LUMINANCE);
            (baseline, sweep)
        }
        // Unknown backgrounds cannot promise contrast against both black and
        // white. Retain the established neutral fallback until the terminal
        // appearance is known, rather than inventing a background fill.
        TerminalBackground::Unknown => (theme.composer_idle_rgb(model), model),
    };
    Some(palette)
}

/// Index into one ramp so it advances one entry per shimmer tick and wraps at
/// the ramp's own length.
fn ramp_index<T>(ramp: &[T], index: isize, shimmer_frame: usize) -> usize {
    (index - (shimmer_frame % ramp.len()) as isize).rem_euclid(ramp.len() as isize) as usize
}

fn activity_shimmer_color(
    baseline: Rgb,
    sweep: Rgb,
    background: TerminalBackground,
    label: &str,
    index: isize,
    shimmer_frame: usize,
    rainbow_strength: u16,
) -> Rgb {
    // Sweep at one terminal cell per tick, including the marker and enough
    // trailing space for the highlight to leave the entire label before looping.
    let cycle = label.width() + ACTIVITY_LABEL_OFFSET as usize + 2;
    let center = (shimmer_frame % cycle) as isize - ACTIVITY_LABEL_OFFSET;
    // The falloff is deliberately wider than the old 100/78/48/0 ramp. With
    // the increased luminance separation one lit neighbour cell used to sit at
    // ~78% of a 0.04 move (invisible); now four trailing cells stay visibly
    // graded (84/64/40/18) so the highlight reads as a moving sweep rather than
    // a single blinking cell. Every step is monotone in distance, so the centre
    // remains the most distinct cell.
    let sweep_strength = match (index - center).unsigned_abs() {
        0 => 100,
        1 => 84,
        2 => 64,
        3 => 40,
        4 => 18,
        _ => match background {
            TerminalBackground::Unknown => 28,
            TerminalBackground::Dark | TerminalBackground::Light => 0,
        },
    };
    // Known profiles keep a readable foreground at rest and move a narrow
    // darker sweep through light text on dark profiles. Light profiles invert
    // the relationship: the sweep is lighter, but remains a dark readable color.
    let normal = (
        mix_channel(baseline.0, sweep.0, sweep_strength),
        mix_channel(baseline.1, sweep.1, sweep_strength),
        mix_channel(baseline.2, sweep.2, sweep_strength),
    );
    // The status's own chromatic ramp travels with the sweep: resting cells
    // (strength 0) keep the plain foreground, so the label never becomes a
    // flat wash of hue. Unknown backgrounds keep the established neutral
    // fallback and never take a label tint.
    let tinted = match activity_tint(label) {
        Some(tint) if background != TerminalBackground::Unknown => {
            let accent =
                activity_tint_color(tint, activity_luminance(normal), index, shimmer_frame);
            (
                mix_channel(normal.0, accent.0, sweep_strength),
                mix_channel(normal.1, accent.1, sweep_strength),
                mix_channel(normal.2, accent.2, sweep_strength),
            )
        }
        _ => normal,
    };
    // `Working` keeps the established whole-label rainbow: the max/ultra level
    // emphasis tints every cell, not only the travelling centre.
    if label == "Working" && rainbow_strength > 0 {
        let rainbow = activity_rainbow_color(background, index, shimmer_frame);
        (
            mix_channel(tinted.0, rainbow.0, rainbow_strength),
            mix_channel(tinted.1, rainbow.1, rainbow_strength),
            mix_channel(tinted.2, rainbow.2, rainbow_strength),
        )
    } else {
        tinted
    }
}

fn activity_shimmer_label(
    theme: &OctetTheme,
    reasoning: &AssistantBlock,
    label: &str,
    shimmer_frame: usize,
    rainbow_strength: u16,
) -> String {
    let static_label = || theme.bold(&theme.model_fg(reasoning.model_lab, label));
    let Some((baseline, sweep)) = activity_shimmer_palette(theme, reasoning) else {
        return static_label();
    };
    let background = theme.background();
    let mut rendered = String::with_capacity(label.len().saturating_mul(20));
    let mut index = 0;
    for grapheme in label.graphemes(true) {
        let color = activity_shimmer_color(
            baseline,
            sweep,
            background,
            label,
            index as isize,
            shimmer_frame,
            rainbow_strength,
        );
        rendered.push_str(&theme.rgb_fg(color, grapheme));
        index += grapheme.width();
    }
    theme.bold(&rendered)
}

fn activity_label(reasoning: &AssistantBlock) -> &str {
    if reasoning.is_working_activity() {
        "Working"
    } else if reasoning.text.is_empty() && !reasoning.show_reasoning_hint {
        reasoning.reasoning_heading.as_deref().unwrap_or("Thinking")
    } else {
        "Thinking"
    }
}

/// Render the margin dot in the same shimmer coordinate space as the status
/// label. Every style is a foreground style applied only to the dot glyph.
pub(super) fn activity_shimmer_marker(
    theme: &OctetTheme,
    reasoning: &AssistantBlock,
    shimmer_frame: usize,
    rainbow_strength: u16,
    marker: &str,
) -> String {
    let static_marker = || theme.model_fg(reasoning.model_lab, marker);
    let Some((baseline, sweep)) = activity_shimmer_palette(theme, reasoning) else {
        return static_marker();
    };
    let retry_label = reasoning
        .retry_activity
        .as_ref()
        .map(|retry| retry.label_at(Instant::now()));
    let color = activity_shimmer_color(
        baseline,
        sweep,
        theme.background(),
        retry_label
            .as_deref()
            .unwrap_or_else(|| activity_label(reasoning)),
        ACTIVITY_MARKER_INDEX,
        shimmer_frame,
        rainbow_strength,
    );
    theme.rgb_fg(color, marker)
}

fn reasoning_detail_line(theme: &OctetTheme, reasoning: &AssistantBlock) -> String {
    let elbow = activity_elbow(theme);
    let hint = "(ctrl+o to expand)";
    let detail = reasoning.reasoning_heading.as_deref().map_or_else(
        || format!("{elbow} {hint}"),
        |heading| format!("{elbow} {heading} {hint}"),
    );
    subdued_text(theme, &detail)
}

fn format_activity_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}h{:02}m{:02}s",
            seconds / 3_600,
            (seconds / 60) % 60,
            seconds % 60
        )
    }
}

fn activity_status_line(
    theme: &OctetTheme,
    reasoning: &AssistantBlock,
    label: &str,
    shimmer_frame: usize,
    rainbow_strength: u16,
) -> String {
    let label = activity_shimmer_label(theme, reasoning, label, shimmer_frame, rainbow_strength);
    let Some(started_at) = reasoning.activity_started_at else {
        return label;
    };
    let elapsed = format_activity_duration(started_at.elapsed().as_secs());
    let detail = subdued_text(theme, &format!("({elapsed} • esc to interrupt)"));
    format!("{label} {detail}")
}

fn collapsed_reasoning_lines_at(
    theme: &OctetTheme,
    reasoning: &AssistantBlock,
    shimmer_frame: usize,
    rainbow_strength: u16,
) -> Vec<String> {
    if reasoning.finished {
        return Vec::new();
    }
    if reasoning.is_working_activity() {
        let label = reasoning
            .retry_activity
            .as_ref()
            .map(|retry| retry.label_at(Instant::now()));
        return vec![activity_status_line(
            theme,
            reasoning,
            label.as_deref().unwrap_or("Working"),
            shimmer_frame,
            rainbow_strength,
        )];
    }
    if reasoning.text.is_empty() && !reasoning.show_reasoning_hint {
        let retry_label = reasoning
            .retry_activity
            .as_ref()
            .map(|retry| retry.label_at(Instant::now()));
        let label = retry_label
            .as_deref()
            .unwrap_or_else(|| reasoning.reasoning_heading.as_deref().unwrap_or("Thinking"));
        return vec![activity_status_line(
            theme,
            reasoning,
            label,
            shimmer_frame,
            0,
        )];
    }

    let mut lines = vec![activity_status_line(
        theme,
        reasoning,
        "Thinking",
        shimmer_frame,
        0,
    )];
    if reasoning.show_reasoning_hint {
        lines.push(reasoning_detail_line(theme, reasoning));
    }
    lines
}

pub(super) fn collapsed_reasoning_lines(
    theme: &OctetTheme,
    reasoning: &AssistantBlock,
) -> Vec<String> {
    collapsed_reasoning_lines_at(theme, reasoning, 0, 0)
}

#[cfg(test)]
pub(super) fn render_reasoning_on_surface(
    reasoning: &AssistantBlock,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    show_reasoning: bool,
    background: Option<Color>,
    shimmer_frame: usize,
) -> Vec<String> {
    render_reasoning_on_surface_with_rainbow(
        reasoning,
        renderer,
        theme,
        width,
        show_reasoning,
        background,
        shimmer_frame,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_reasoning_on_surface_with_rainbow(
    reasoning: &AssistantBlock,
    renderer: &RichRenderer,
    theme: &OctetTheme,
    width: u16,
    show_reasoning: bool,
    background: Option<Color>,
    shimmer_frame: usize,
    rainbow_strength: u16,
) -> Vec<String> {
    let non_expandable_activity = reasoning.text.is_empty() && !reasoning.show_reasoning_hint;
    if non_expandable_activity || (!reasoning.reasoning_expanded && !show_reasoning) {
        return collapsed_reasoning_lines_at(theme, reasoning, shimmer_frame, rainbow_strength)
            .into_iter()
            .map(|line| {
                let line = fit_line(&line, width);
                if theme.capabilities().color == ColorDepth::None {
                    strip_terminal_sequences(&line)
                } else {
                    line
                }
            })
            .collect();
    }

    // Expanded reasoning already owns a distinct transcript inset and muted
    // prose style. Do not turn its first row into a one-item bulleted list;
    // every Markdown row starts from the same reasoning content gutter.
    finish_transcript_block(reasoning.render_on_surface(renderer, theme, width, background))
        .into_iter()
        .map(|line| fit_line(&line, width))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use sexy_tui_rs::{strip_terminal_sequences, visible_width};

    use super::*;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::{self, ModelLab};

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
        let mut reasoning = AssistantBlock::streaming_reasoning(
            "First private thought.\n\nSecond private thought.",
        );
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
    fn collapsed_reasoning_uses_shimmering_thinking_and_moves_heading_to_detail() {
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
        assert_ne!(first[0], next[0], "the Thinking shimmer must move");
        assert!(first[0].contains("38;2;"), "{first:?}");
        assert!(
            !first[0].contains(";48;2;"),
            "the Codex-style shimmer must never paint character backgrounds: {first:?}"
        );
    }

    #[test]
    fn model_and_rainbow_shimmers_are_foreground_only() {
        let theme = theme::test_theme();
        let reasoning =
            AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
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

        let plain =
            theme::test_theme_with(TerminalCapabilities::test(false, false, ColorDepth::None));
        let no_color = collapsed_reasoning_lines_at(&plain, &working, 0, 100);
        assert_eq!(no_color, vec!["Working"]);
        assert!(!no_color[0].contains('\x1b'));
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
                let theme = theme::test_theme_for(
                    background,
                    TerminalCapabilities::test(true, true, depth),
                );
                for lab in [None, Some(ModelLab::OpenAi), Some(ModelLab::Alibaba)] {
                    let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(lab);
                    for label in [
                        "Working",
                        "Thinking",
                        "Compacting context",
                        "Waiting for network",
                    ] {
                        for strength in [0, 50, 100] {
                            for frame in 0..label.width() + ACTIVITY_LABEL_OFFSET as usize + 2 {
                                let rendered = activity_shimmer_label(
                                    &theme, &reasoning, label, frame, strength,
                                );
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
        let reasoning =
            AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
        let dark = theme::test_theme_for(
            TerminalBackground::Dark,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        );
        let light = theme::test_theme_for(
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

    /// The maintainer's acceptance criterion for the activity shimmer: the
    /// travelling highlight must be *measurably* visible on both known terminal
    /// profiles, for several modelled accents, while the resting colour keeps
    /// its contrast.
    ///
    /// Thresholds, and why each cannot be satisfied by encoder rounding alone:
    ///
    /// * `MIN_LUMINANCE_DELTA = 0.06` relative luminance for true colour. The
    ///   rejected light profile moved 0.04 (0.01 -> 0.05), which the maintainer
    ///   reported as invisible. The ANSI256 grayscale tail (indices 232..255)
    ///   steps in 10 sRGB units, i.e. ~0.02 relative luminance near the light
    ///   profile's target band, so 0.06 is at least three quantization steps.
    /// * ANSI256 cells are bounded by `nearest_ansi256`'s 1.2:1 contrast-ratio
    ///   filter around the requested colour, which is the only reason a lower
    ///   `MIN_LUMINANCE_DELTA_ANSI256 = 0.045` threshold is admissible: a 0.08
    ///   requested separation can compress to
    ///   `(0.09 + 0.05) / 1.2 - ((0.01 + 0.05) * 1.2 - 0.05) = 0.045` on the
    ///   bound. The encoder is the limiter here, not the palette.
    /// * `MIN_CHROMA_DELTA = 0.08` normalized channel spread. The fixed ANSI256
    ///   color cube only moves in `40/255 = 0.157` chroma steps (levels
    ///   `0,95,135,175,215,255`), so a 0.08 move is a real hue change rather
    ///   than a rounding artifact.
    /// * `MIN_RESTING_CONTRAST = 7.0`, WCAG AAA for body text, against the
    ///   darkest and lightest representative composite surfaces. The resting
    ///   colour is what the label shows for most of each cycle, so the visible
    ///   highlight must not have been bought with a dimmer baseline.
    #[test]
    fn activity_shimmer_highlight_is_measurably_visible_on_both_profiles() {
        const MIN_LUMINANCE_DELTA: f64 = 0.06;
        const MIN_LUMINANCE_DELTA_ANSI256: f64 = 0.045;
        const MIN_CHROMA_DELTA: f64 = 0.08;
        const MIN_RESTING_CONTRAST: f64 = 7.0;
        for (background, surfaces) in [
            (TerminalBackground::Dark, [(38, 38, 38), (64, 64, 64)]),
            (TerminalBackground::Light, [(245, 245, 245), (224, 224, 224)]),
        ] {
            for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
                let theme = theme::test_theme_for(
                    background,
                    TerminalCapabilities::test(true, true, depth),
                );
                let min_luminance_delta = match depth {
                    ColorDepth::TrueColor => MIN_LUMINANCE_DELTA,
                    _ => MIN_LUMINANCE_DELTA_ANSI256,
                };
                // Every claim below holds at both depths. True colour is the
                // exact encoder; ANSI256 additionally passes each cell through
                // `nearest_ansi256`, so the strict cell *ordering* is asserted
                // exactly there and with that encoder's own documented
                // luminance bound at ANSI256 depth.
                let exact_colour = depth == ColorDepth::TrueColor;
                for lab in [
                    None,
                    Some(ModelLab::OpenAi),
                    Some(ModelLab::Alibaba),
                    Some(ModelLab::Meta),
                    Some(ModelLab::Google),
                ] {
                    let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(lab);
                    let (palette_baseline, palette_sweep) =
                        activity_shimmer_palette(&theme, &reasoning).expect("activity palette");
                    // The lifecycle row is asserted against the palette itself:
                    // it must render exactly the plain palette colours and gain
                    // no tint at all. The two status rows must move at least as
                    // far as that palette does *and* carry chroma.
                    for (label, tinted) in [
                        ("Working", true),
                        ("Thinking", true),
                        ("Compacting context", false),
                    ] {
                        // `ACTIVITY_LABEL_OFFSET` puts the sweep centre on the
                        // first grapheme while the final grapheme is more than
                        // four cells away, so one render shows the lit centre and
                        // the resting colour side by side.
                        let rendered =
                            activity_shimmer_label(&theme, &reasoning, label, 2, 0);
                        let colors = rendered_foregrounds(&rendered);
                        assert_eq!(colors.len(), label.chars().count(), "{rendered:?}");
                        let center = colors[0];
                        let resting = *colors.last().expect("resting grapheme");
                        assert_ne!(center, resting, "{background:?} {lab:?} {label}");

                        let luminance_delta = (luminance(center) - luminance(resting)).abs();
                        assert!(
                            luminance_delta >= min_luminance_delta,
                            "{background:?}/{depth:?}/{lab:?} {label}: centre {center:?} moves only {luminance_delta:.3} \
                             of relative luminance from resting {resting:?}"
                        );
                        let chroma_delta = (chroma(center) - chroma(resting)).abs();
                        if tinted {
                            assert!(
                                chroma_delta >= MIN_CHROMA_DELTA,
                                "{background:?}/{depth:?}/{lab:?} {label}: centre {center:?} moves only \
                                 {chroma_delta:.3} of chroma from resting {resting:?}"
                            );
                        }
                        if !tinted {
                            assert_eq!(
                                center,
                                quantized(&theme, palette_sweep),
                                "{background:?}/{depth:?}/{lab:?} {label}: a lifecycle row must render \
                                 the plain palette sweep, never a tint"
                            );
                            assert_eq!(
                                resting,
                                quantized(&theme, palette_baseline),
                                "{background:?}/{depth:?}/{lab:?} {label}: a lifecycle row must rest on \
                                 the plain palette baseline"
                            );
                        }

                        // The centre is the most distinct cell, measured on the
                        // luminance channel: every tint is pinned to the sweep
                        // luminance and the falloff is monotone in distance, so
                        // no neighbour may move further from the resting colour.
                        // (Chroma is deliberately not part of this ordering: it
                        // is carried per ramp entry, and each hue can afford a
                        // different amount of it at one luminance.)
                        let distinctness =
                            |color: Rgb| (luminance(color) - luminance(resting)).abs();
                        let center_distinctness = distinctness(center);
                        // True colour is exact: the centre is strictly the cell
                        // furthest from the resting colour. ANSI256 runs every
                        // cell through `nearest_ansi256`, whose documented 1.2:1
                        // contrast-ratio filter can lift one cell's luminance by
                        // up to `1.2 * (L + 0.05) - 0.05 - L`; the same bound is
                        // allowed here (0.10 for the dark profile, 0.03 for the
                        // light one) and is still a small fraction of the
                        // >= 0.35 / >= 0.08 move the centre must make.
                        let ordering_tolerance = if exact_colour {
                            0.0
                        } else {
                            let sweep_luminance = luminance(palette_sweep);
                            1.2 * (sweep_luminance + 0.05) - 0.05 - sweep_luminance
                        };
                        for (index, color) in colors.iter().enumerate().skip(1) {
                            assert!(
                                distinctness(*color)
                                    <= center_distinctness + ordering_tolerance + 1e-9,
                                "{background:?}/{depth:?}/{lab:?} {label}: cell {index} {color:?} is \
                                 at least as far from the resting colour as the centre {center:?}"
                            );
                        }

                        for surface in surfaces {
                            let contrast = contrast_ratio(luminance(resting), luminance(surface));
                            assert!(
                                contrast >= MIN_RESTING_CONTRAST,
                                "{background:?}/{depth:?}/{lab:?} {label}: resting {resting:?} has only \
                                 {contrast:.2}:1 against {surface:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// `Working` and `Thinking` must never read as the same wash. Their ramps
    /// are separated by more than 60 degrees of hue after the profile clamp, so
    /// the two statuses stay distinguishable on both known backgrounds.
    #[test]
    fn working_and_thinking_sweeps_keep_disjoint_hue_bands() {
        for (background, working_band, thinking_band) in [
            (TerminalBackground::Dark, (0.0, 90.0), (150.0, 300.0)),
            (TerminalBackground::Light, (0.0, 90.0), (150.0, 300.0)),
        ] {
            let theme = theme::test_theme_for(
                background,
                TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            );
            for lab in [None, Some(ModelLab::Alibaba), Some(ModelLab::Meta)] {
                let reasoning = AssistantBlock::streaming_reasoning("").with_model_lab(lab);
                for (label, band) in [("Working", working_band), ("Thinking", thinking_band)] {
                    let rendered = activity_shimmer_label(&theme, &reasoning, label, 2, 0);
                    let center = rendered_foregrounds(&rendered)[0];
                    let hue = hue_degrees(center);
                    assert!(
                        band.0 <= hue && hue <= band.1,
                        "{background:?}/{lab:?} {label}: centre {center:?} has hue {hue:.1}, outside {band:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn model_shimmer_keeps_a_muted_baseline_behind_a_moving_highlight() {
        let theme = theme::test_theme();
        let reasoning =
            AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
        let model = theme
            .model_rgb(Some(ModelLab::Alibaba))
            .expect("model colour");
        let shadow = theme.composer_idle_rgb(model);
        let rendered = activity_shimmer_label(&theme, &reasoning, "Thinking", 2, 0);
        let colors = foreground_color_codes(&rendered);
        assert_eq!(colors.len(), "Thinking".chars().count(), "{rendered:?}");
        assert_eq!(colors[0], format!("{};{};{}", model.0, model.1, model.2));
        assert_ne!(colors[0], format!("{};{};{}", shadow.0, shadow.1, shadow.2));
        assert_ne!(&colors[0], colors.last().expect("last shimmer colour"));
        assert!(!rendered.contains("\x1b[48;"), "{rendered:?}");
    }

    #[test]
    fn shimmer_reaches_every_grapheme_and_loops_at_the_label_width() {
        let theme = theme::test_theme();
        let reasoning =
            AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
        let model = theme.model_rgb(Some(ModelLab::Alibaba)).unwrap();
        let peak = format!("{};{};{}", model.0, model.1, model.2);
        for label in [
            "",
            "A",
            "Working",
            "Thinking",
            "Compacting context",
            "A considerably longer activity label",
            "压缩 e\u{301} 👩‍💻 context",
        ] {
            let cycle = label.width() + ACTIVITY_LABEL_OFFSET as usize + 2;
            let mut cell = 0;
            for (index, grapheme) in label.graphemes(true).enumerate() {
                let frame = cell + ACTIVITY_LABEL_OFFSET as usize;
                let rendered = activity_shimmer_label(&theme, &reasoning, label, frame, 0);
                assert_eq!(strip_terminal_sequences(&rendered), label);
                assert_eq!(
                    foreground_color_codes(&rendered)[index],
                    peak,
                    "{label}: {index}"
                );
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
        let reasoning =
            AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
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
        let theme =
            theme::test_theme_with(TerminalCapabilities::test(false, true, ColorDepth::None));
        let mut working = AssistantBlock::streaming_reasoning("")
            .with_model_lab(Some(ModelLab::OpenAi))
            .with_activity_started_at(Some(Instant::now() - Duration::from_millis(28_100)));
        working.reasoning_heading = Some("Working".into());
        working.show_reasoning_hint = false;

        let rendered = collapsed_reasoning_lines_at(&theme, &working, 0, 0);
        assert_eq!(rendered, vec!["Working (28s • esc to interrupt)"]);
    }

    #[test]
    fn collapsed_reasoning_without_a_heading_keeps_the_hint_on_the_detail_row() {
        let theme = theme::test_theme();
        let renderer = theme.reasoning_renderer();
        let mut reasoning =
            AssistantBlock::streaming_reasoning("private").with_model_lab(Some(ModelLab::Alibaba));
        let live = render_reasoning(&reasoning, &renderer, &theme, 80, false);
        assert_eq!(live.len(), 2, "{live:?}");
        assert_eq!(strip_terminal_sequences(&live[0]), "Thinking");
        assert_eq!(strip_terminal_sequences(&live[1]), "└ (ctrl+o to expand)");
        assert!(live[0].contains("\x1b[1m"), "{live:?}");
        assert!(!live[1].contains("\x1b[3m"), "{live:?}");

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
        let theme =
            theme::test_theme_with(TerminalCapabilities::test(false, false, ColorDepth::None));
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
}
