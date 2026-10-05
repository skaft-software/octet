use sexy_tui_rs::{strip_terminal_sequences, visible_width, Color, RichRenderer};
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::assistant_block::AssistantBlock;
use super::{activity_elbow, finish_transcript_block, fit_line, subdued_text};
use crate::tui::terminal::ColorDepth;
use crate::tui::theme::{OctetTheme, ShimmerMode, TerminalBackground};

/// The status label starts two cells after its margin dot (`• `). Keeping the
/// dot in the same coordinate space makes the shimmer travel through it before
/// crossing the label.
const ACTIVITY_LABEL_OFFSET: isize = 2;
const ACTIVITY_MARKER_INDEX: isize = -ACTIVITY_LABEL_OFFSET;

/// How far the sweep reaches on either side of its centre. The highlight is
/// nine cells wide (see [`ACTIVITY_SWEEP_FALLOFF`]) and symmetric, so a cell is
/// lit exactly while `|index - center| <= ACTIVITY_SWEEP_HALF`.
const ACTIVITY_SWEEP_HALF: isize = 4;

/// Falloff of the sweep by distance from its centre, in percent of the sweep
/// colour. Five graded cells (rather than the original three: 100/78/48) so the
/// highlight reads as a soft travelling band instead of a stepping block. Every
/// step is strictly monotone in distance, so the centre stays the most distinct
/// cell.
const ACTIVITY_SWEEP_FALLOFF: [u16; 5] = [100, 84, 64, 40, 18];

/// First centre position of a cycle: far enough before the label that neither
/// the margin dot (the leftmost rendered cell, at [`ACTIVITY_MARKER_INDEX`]) nor
/// any label cell is lit.
const ACTIVITY_SWEEP_START: isize = -(ACTIVITY_SWEEP_HALF + ACTIVITY_LABEL_OFFSET + 1);

/// One full period of the shimmer, in frames.
///
/// The centre advances one cell per frame from [`ACTIVITY_SWEEP_START`] to
/// `label.width() + ACTIVITY_SWEEP_HALF`, one integer position per frame, so the
/// sweep enters from before the margin dot, crosses *every* label cell, and
/// exits past the trailing edge before the cycle repeats. The two end positions
/// leave every rendered cell - the dot included - at its resting colour: that
/// rest gap is what makes the loop read as "flow through, rest, flow again".
/// Without it the highlight teleported from the trailing edge straight back to
/// the leading edge at partial brightness, which read as "slides part way
/// through the letters then loops back".
fn activity_cycle(label: &str) -> usize {
    // The centres that can light a rendered cell - the margin dot at
    // [`ACTIVITY_MARKER_INDEX`] through the label's trailing cell - span
    // `width + 2 * ACTIVITY_SWEEP_HALF + ACTIVITY_LABEL_OFFSET` positions. The
    // cycle pads that lit span with the leading and the trailing frame at rest,
    // so the highlight brightens out of rest, crosses every label cell, dims
    // back to rest, and only then repeats.
    (label.width() as isize
        + 2 * ACTIVITY_SWEEP_HALF
        + ACTIVITY_LABEL_OFFSET
        + ACTIVITY_SWEEP_REST_FRAMES as isize) as usize
}

/// Frames per cycle on which every rendered cell shows the resting colour: the
/// first and the last position of the traverse, one pad at each end of the lit
/// span. [`activity_cycle`] spends exactly this many frames on the pads and the
/// smoothness test pins that count, so the cycle can never be shortened into a
/// loop with no rest gap.
const ACTIVITY_SWEEP_REST_FRAMES: usize = 2;

// Physical mode treats the shimmer as a small moving light field rather than
// as a sequence of palette steps. It is deliberately limited to known
// backgrounds and TrueColor/ANSI256; the classic renderer remains the
// compatibility path for ANSI16 and unknown profiles.
const ACTIVITY_PHYS_DARK_BASE: f64 = 0.55;
const ACTIVITY_PHYS_DARK_SWEEP: f64 = 0.99;
const ACTIVITY_PHYS_LIGHT_BASE: f64 = 0.0085;
const ACTIVITY_PHYS_LIGHT_SWEEP: f64 = 0.11;
const ACTIVITY_PHYS_H_FRONT: f64 = 2.5;
const ACTIVITY_PHYS_H_TRAIL: f64 = 5.0;
const ACTIVITY_PHYS_CORE_HALF: f64 = 1.2;
const ACTIVITY_PHYS_CORE_WEIGHT: f64 = 0.55;
const ACTIVITY_PHYS_CORE_LIFT: f64 = 0.10;
const ACTIVITY_PHYS_SPEED_TRUECOLOR: f64 = 1.6;
const ACTIVITY_PHYS_SPEED_ANSI256: f64 = 4.0 / 3.0;
const ACTIVITY_PHYS_REST_TICKS: usize = 2;

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
// Strengthen the sweep by moving the resting text toward the profile's contrast
// extreme, not by making the travelling band less readable. The previous
// 0.85/0.01 baselines left too little visible movement, especially after ANSI256
// quantization. The sweep endpoints (and the rainbow palette) stay unchanged.
//
// The resting extremes are deliberately eased off pure white and pure black:
// 0.98 resolved to about #fd on a neutral identity, which reads as glaring on a
// dark terminal, and 0.002 resolved to #00, which reads as a hard black on a
// light one. 0.95 (#f9) and 0.0085 (about #16) keep the same hue, the same
// monotone sweep and the same contrast behaviour while resting a step toward
// grey. How far the easing may go is bounded by two measured invariants, not by
// taste alone: on light the swept cell must stay at or above a 4.5:1 contrast
// ratio against the #e0e0e0 composite, and the margin dot's pulse must still
// quantize to a different neutral ANSI16 entry than its resting colour. A light
// resting luminance of 0.009 fails the pinned 1.7:1 worst-case cell separation
// for a blue identity (Meta) after ANSI256 quantization, so 0.0085 is the
// largest easing that holds. The light sweep ceiling moves with the base
// (0.095) so the sweep still travels at least the pinned 0.08 of relative
// luminance.
const ACTIVITY_DARK_BASE_LUMINANCE: f64 = 0.95;
const ACTIVITY_DARK_SWEEP_LUMINANCE: f64 = 0.50;
const ACTIVITY_LIGHT_BASE_LUMINANCE: f64 = 0.0085;
const ACTIVITY_LIGHT_SWEEP_LUMINANCE: f64 = 0.095;

// A single dot needs a larger pulse than a bold word. It rests at a quieter
// model foreground and gains contrast when the same sweep crosses it: brighter
// on dark terminals, darker on light ones. No size, glyph, or background change.
const ACTIVITY_MARKER_DARK_BASE_LUMINANCE: f64 = 0.30;
const ACTIVITY_MARKER_LIGHT_BASE_LUMINANCE: f64 = 0.18;

/// The `Working` rainbow is clamped to its own readable band. These are
/// deliberately separate from the resting baselines above: retuning resting
/// contrast must never silently rewrite the established rainbow identity. The
/// dark floor sits inside the sweep band (so the rainbow centre stays a
/// visible darkening) and the light ceiling is the light-profile sweep band,
/// so a rainbow centre always moves at least as far from the resting colour as
/// the plain sweep does.
const ACTIVITY_RAINBOW_DARK_MIN_LUMINANCE: f64 = 0.62;
const ACTIVITY_RAINBOW_LIGHT_MAX_LUMINANCE: f64 = ACTIVITY_LIGHT_SWEEP_LUMINANCE;

fn physical_shimmer(theme: &OctetTheme) -> bool {
    theme.shimmer_mode() == ShimmerMode::Physical
        && matches!(
            theme.background(),
            TerminalBackground::Dark | TerminalBackground::Light
        )
        && matches!(
            theme.capabilities().color,
            ColorDepth::TrueColor | ColorDepth::Ansi256
        )
}

fn physical_speed(theme: &OctetTheme) -> f64 {
    match theme.capabilities().color {
        ColorDepth::TrueColor => ACTIVITY_PHYS_SPEED_TRUECOLOR,
        ColorDepth::Ansi256 => ACTIVITY_PHYS_SPEED_ANSI256,
        ColorDepth::Ansi16 | ColorDepth::None => ACTIVITY_PHYS_SPEED_TRUECOLOR,
    }
}

fn physical_motion_ticks(label: &str, speed: f64) -> usize {
    let traverse = (label.width() as f64 - 1.0)
        + ACTIVITY_PHYS_H_TRAIL
        + (-ACTIVITY_MARKER_INDEX as f64)
        + ACTIVITY_PHYS_H_FRONT;
    // The endpoints are both rendered positions, so at least two motion
    // samples are required even for a very short label.
    (traverse / speed).round().max(2.0) as usize
}

#[cfg(test)]
fn physical_cycle(label: &str, speed: f64) -> usize {
    physical_motion_ticks(label, speed) + ACTIVITY_PHYS_REST_TICKS
}

/// Map a physical shimmer tick to its continuous centre position.
///
/// The first motion sample starts exactly at the leading support boundary and
/// the final motion sample ends exactly at the trailing support boundary. The
/// nominal speed determines the number of samples; the short endpoint
/// interpolation prevents a visible jump into the explicit parked rest gap.
fn phys_center(label: &str, frame: usize, speed: f64) -> f64 {
    let motion_ticks = physical_motion_ticks(label, speed);
    let cycle = motion_ticks + ACTIVITY_PHYS_REST_TICKS;
    let phase = frame % cycle;
    let start = ACTIVITY_MARKER_INDEX as f64 - ACTIVITY_PHYS_H_FRONT;
    let end = label.width() as f64 - 1.0 + ACTIVITY_PHYS_H_TRAIL;
    if phase >= motion_ticks {
        return end;
    }
    start + (end - start) * (phase as f64 / (motion_ticks - 1) as f64)
}

fn raised_cosine(distance: f64, half_width: f64) -> f64 {
    let distance = distance.abs();
    if distance >= half_width {
        0.0
    } else {
        0.5 * (1.0 + (std::f64::consts::PI * distance / half_width).cos())
    }
}

/// Main light envelope. The leading edge is short and the trailing tail is
/// longer, which makes the highlight read as light moving through the text.
fn band_main(distance: f64) -> f64 {
    let half_width = if distance < 0.0 {
        ACTIVITY_PHYS_H_TRAIL
    } else {
        ACTIVITY_PHYS_H_FRONT
    };
    raised_cosine(distance, half_width)
}

fn band_core(distance: f64) -> f64 {
    raised_cosine(distance, ACTIVITY_PHYS_CORE_HALF)
}

/// Which ramp an activity label carries.
///
/// Both ramps are variations of **one** colour identity per model: the identity
/// is the model's own colour (the same `theme.model_rgb` the resting label
/// uses), and a ramp only rotates that hue inside a narrow band and steps its
/// brightness. There is deliberately no per-label hue set: a maintainer-reported
/// regression gave `Working` a fixed warm (orange-yellow) hue set and `Thinking`
/// a fixed cool one, so a neutral model (`gpt-6-astra` -> lab `OpenAi`, source
/// colour `#1f1f1f`) shimmered in two unrelated hue families instead of its own
/// grey identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityRamp {
    Working,
    Thinking,
}

/// Ramp length: one entry per shimmer tick before the ramp wraps.
const ACTIVITY_RAMP_ENTRIES: usize = 4;

/// Hue rotation each ramp entry applies to the model's own hue, as a fraction of
/// [`ACTIVITY_RAMP_HUE_SPAN`]. The first entry is the model colour itself, so
/// the resting identity is always reachable and the ramp reads as a brighter,
/// more saturated variation of it rather than as a second palette.
const ACTIVITY_RAMP_HUE_STEPS: [f64; ACTIVITY_RAMP_ENTRIES] = [0.0, 0.34, 0.68, 1.0];

/// Saturation multiplier each ramp entry applies to the model colour's own HSV
/// saturation. Multiplying (never replacing) the saturation is what keeps a
/// neutral identity neutral: zero chroma times any multiplier is still zero.
const ACTIVITY_RAMP_SATURATION_STEPS: [f64; ACTIVITY_RAMP_ENTRIES] = [1.0, 1.25, 1.5, 1.75];

/// Hue rotation a ramp can reach around the model's own hue, in degrees. Narrow
/// by design: wide enough that the sweep reads as movement inside the model's
/// colour family, far too narrow to reach an unrelated hue set. Both labels
/// share this band exactly; hue is never the cue that separates them.
const ACTIVITY_RAMP_HUE_SPAN: f64 = 24.0;

/// How far `Thinking` travels from the resting colour, as a fraction of the
/// `Working` sweep. This is the documented non-chromatic cue that keeps the two
/// statuses distinguishable while they share one colour identity: `Working`
/// uses the profile's full proven separation and `Thinking` a shallower one of
/// the same colour, so they differ in *luminance range* - a difference that
/// survives an achromatic identity, where no hue can differ - and never in hue.
///
/// `0.80` keeps Thinking quieter without changing its hue. Both labels benefit
/// from the stronger resting-text contrast, and the dot uses this same depth
/// with its own higher-contrast pulse palette. Every per-label invariant is
/// untouched: each label's falloff is still strictly monotone (the
/// centre stays the most distinct cell of its own label), both endpoints stay
/// inside the luminance band the profile already proved contrast-safe, and the
/// cycle length stays identical.
const ACTIVITY_THINKING_SWEEP_DEPTH: f64 = 0.80;

/// HSV saturation at or below which an identity counts as achromatic. At or
/// below it the ramp is forced to an exact profile grey: no hue rotation can
/// introduce chroma where the model colour has none.
const ACTIVITY_NEUTRAL_SATURATION: f64 = 0.06;

/// HSV saturation at or above which an identity carries the ramp's **full**
/// chroma treatment: the whole `ACTIVITY_RAMP_HUE_SPAN` rotation and the whole
/// `ACTIVITY_RAMP_SATURATION_STEPS` boost. Between this and
/// [`ACTIVITY_NEUTRAL_SATURATION`] a single kernel - [`activity_chromatic_weight`]
/// - scales both, so a colour that is only just not grey can never be pushed a
///   whole hue span away from itself. That is this ramp's version of the reported
///   warm `Working` shimmer: the defect was a hue set applied *regardless* of the
///   identity, and a near-neutral identity is the one case where the ramp has no
///   chroma of its own to spend. Every compiled-in lab colour other than the exact
///   grey of `OpenAi` resolves to at least 0.37 (`Cohere`) on every terminal
///   profile, so no shipped model identity loses the treatment.
const ACTIVITY_CHROMATIC_SATURATION: f64 = 0.25;

impl ActivityRamp {
    fn of(label: &str) -> Option<Self> {
        match label {
            "Working" => Some(Self::Working),
            "Thinking" => Some(Self::Thinking),
            _ => None,
        }
    }

    /// How far this label travels from the resting colour, as a fraction of the
    /// profile's proven `baseline` -> `sweep` separation. See
    /// [`ACTIVITY_THINKING_SWEEP_DEPTH`]: the two statuses share one colour and
    /// differ by luminance range.
    fn sweep_depth(self) -> f64 {
        match self {
            Self::Working => 1.0,
            Self::Thinking => ACTIVITY_THINKING_SWEEP_DEPTH,
        }
    }

    /// One ramp entry as the colour it must render for a cell whose plain
    /// (untinted) colour has relative luminance `normal`.
    ///
    /// The entry is built *at the cell's own luminance*, so the tint adds hue
    /// and chroma only: it can never move a cell outside the luminance band the
    /// plain palette already proved contrast-safe, and the luminance falloff -
    /// including the centre-is-the-most-distinct-cell property - stays exactly
    /// the plain one.
    ///
    /// Hue rotation and saturation scale are **identical for both labels**: hue
    /// is deliberately never a cue between the statuses. A per-label direction
    /// (`Working` rotating one way, `Thinking` the other) put the two ramps up
    /// to `2 * ACTIVITY_RAMP_HUE_SPAN` apart at the outermost step, so a
    /// chromatic model shimmered in two hue families - the reported "different
    /// colored shimmers". The one cue that separates the statuses is the sweep's
    /// luminance range, [`Self::sweep_depth`]. How much of the rotation and
    /// boost an identity can carry is decided by its own chroma weight, so a
    /// near-neutral model colour is rotated by nearly nothing and an exactly
    /// grey one not at all.
    fn accent(self, identity: ActivityIdentity, step: usize, normal: f64) -> Rgb {
        // `self` is deliberately unused: the accent must not depend on which
        // label asked for it, only on the model identity (and, above this
        // function, on the label's sweep depth). `ActivityRamp::of` is still the
        // one place that maps a rendered label to its ramp, and the invariant
        // test pins the two labels to byte-identical accents.
        activity_accent_color(
            identity,
            ACTIVITY_RAMP_HUE_SPAN * ACTIVITY_RAMP_HUE_STEPS[step],
            ACTIVITY_RAMP_SATURATION_STEPS[step],
            normal,
        )
    }
}

/// The colour identity an activity ramp is allowed to use.
///
/// `Some(color)` is a genuine model identity: exactly the colour the resting
/// label renders (`theme.model_rgb`). `None` means the session has no model
/// identity at all - an unclassified lab (`ModelLab::Unknown`) or no lab
/// (`None`) - in which case the ramp must not invent one out of the theme's
/// fallback chrome accent. That distinction is what makes a neutral shimmer
/// possible for a model the catalog cannot classify.
#[derive(Clone, Copy, Debug)]
struct ActivityIdentity {
    color: Option<Rgb>,
}

impl ActivityIdentity {
    /// The identity of the active model: its own colour, or nothing when the lab
    /// is unknown/unset.
    fn for_model(theme: &OctetTheme, reasoning: &AssistantBlock) -> Self {
        let known = matches!(
            reasoning.model_lab,
            Some(lab) if lab != crate::tui::theme::ModelLab::Unknown
        );
        Self {
            color: if known {
                theme.model_rgb(reasoning.model_lab)
            } else {
                None
            },
        }
    }
}

/// HSV view of a colour. Saturation is the standard `(max - min) / max`, so an
/// achromatic colour has exactly zero saturation and no hue rotation can make it
/// chromatic.
#[derive(Clone, Copy, Debug)]
struct ActivityHsv {
    hue: f64,
    saturation: f64,
}

fn activity_hsv(color: Rgb) -> ActivityHsv {
    let channel = |value: u8| f64::from(value) / 255.0;
    let (red, green, blue) = (channel(color.0), channel(color.1), channel(color.2));
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let spread = max - min;
    let sector = if spread <= f64::EPSILON {
        0.0
    } else if max == red {
        ((green - blue) / spread).rem_euclid(6.0)
    } else if max == green {
        (blue - red) / spread + 2.0
    } else {
        (red - green) / spread + 4.0
    };
    ActivityHsv {
        hue: (sector * 60.0).rem_euclid(360.0),
        saturation: if max <= f64::EPSILON {
            0.0
        } else {
            spread / max
        },
    }
}

/// HSV -> RGB, the exact inverse of [`activity_hsv`] for hue and saturation.
fn activity_hsv_color(hue: f64, saturation: f64, value: f64) -> Rgb {
    let chroma = value * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let secondary = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match sector as usize % 6 {
        0 => (chroma, secondary, 0.0),
        1 => (secondary, chroma, 0.0),
        2 => (0.0, chroma, secondary),
        3 => (0.0, secondary, chroma),
        4 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    let floor = value - chroma;
    let channel = |value: f64| ((value + floor) * 255.0).round().clamp(0.0, 255.0) as u8;
    (channel(red), channel(green), channel(blue))
}

/// The highest relative luminance a `hue`/`saturation` family can reach (at
/// value 1).
fn activity_family_luminance(hue: f64, saturation: f64) -> f64 {
    activity_luminance(activity_hsv_color(hue, saturation, 1.0))
}

/// The largest saturation at or below `maximum` whose family still reaches
/// `target` luminance at value 1.
///
/// Desaturating a hue moves it toward white, which raises luminance for every
/// hue, so the reachable set is monotone and the split is exact enough for a
/// colour. Without this an identity such as a deep blue could not be rendered at
/// the dark profile's bright resting luminance at all.
fn activity_reachable_saturation(hue: f64, maximum: f64, target: f64) -> f64 {
    if activity_family_luminance(hue, maximum) >= target {
        return maximum;
    }
    let (mut low, mut high) = (0.0, maximum);
    for _ in 0..16 {
        let saturation = (low + high) / 2.0;
        if activity_family_luminance(hue, saturation) >= target {
            low = saturation;
        } else {
            high = saturation;
        }
    }
    low
}

/// The smallest value whose colour reaches `target` luminance. Luminance is
/// monotone in value for a fixed hue and saturation, and value 0 is black, so
/// every non-negative target is reachable.
fn activity_reachable_value(hue: f64, saturation: f64, target: f64) -> f64 {
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..20 {
        let value = (low + high) / 2.0;
        if activity_luminance(activity_hsv_color(hue, saturation, value)) >= target {
            high = value;
        } else {
            low = value;
        }
    }
    high
}

/// An exact profile grey at `target` relative luminance: white, black, or a
/// blend of the two. Blending toward white adds the same amount to all three
/// channels, so the result is exactly neutral for every target.
fn activity_grey_at(target: f64) -> Rgb {
    activity_color_at_least((0, 0, 0), target)
}

/// How much of the ramp's chroma treatment - hue rotation and saturation
/// boost - an identity with HSV saturation `saturation` may carry, in `[0, 1]`.
///
/// The weight comes from the **model colour itself**: whatever
/// `theme.model_rgb(reasoning.model_lab)` resolved to for this session, never
/// the model id, the lab name, or the status label. A model switched to a lab
/// whose theme colour is nearly grey therefore shimmers in nearly pure
/// luminance, exactly like the compiled-in `OpenAi` grey, without any id being
/// special-cased.
///
/// The scale is linear from zero at [`ACTIVITY_NEUTRAL_SATURATION`] to one at
/// [`ACTIVITY_CHROMATIC_SATURATION`] so the treatment is continuous at the
/// neutral boundary: a colour one HSV step above the exact-grey branch rotates
/// by a fraction of `ACTIVITY_RAMP_HUE_SPAN` instead of jumping to all of it.
fn activity_chromatic_weight(saturation: f64) -> f64 {
    if saturation <= ACTIVITY_NEUTRAL_SATURATION {
        return 0.0;
    }
    ((saturation - ACTIVITY_NEUTRAL_SATURATION)
        / (ACTIVITY_CHROMATIC_SATURATION - ACTIVITY_NEUTRAL_SATURATION))
        .clamp(0.0, 1.0)
}

/// The accent for one ramp entry: the model colour's own hue rotated by
/// `hue_offset`, its own saturation scaled by `saturation_scale`, rendered at
/// `luminance`.
///
/// Hue and saturation both come from the model colour, so the ramp can only ever
/// move *inside the model's colour family*. An achromatic identity (HSV
/// saturation at or below [`ACTIVITY_NEUTRAL_SATURATION`] - the `OpenAi` lab's
/// `#1f1f1f`, and therefore `gpt-6-astra`, or any theme that gives a model a grey
/// accent) or an unknown lab (`ActivityIdentity { color: None }`) can only
/// produce a profile grey: no hue can be introduced where there is none, whatever
/// the rotation. A *nearly* achromatic identity takes the fraction of the
/// rotation and boost its own chroma earns ([`activity_chromatic_weight`]), so
/// it moves in luminance far more than in hue.
fn activity_accent_color(
    identity: ActivityIdentity,
    hue_offset: f64,
    saturation_scale: f64,
    luminance: f64,
) -> Rgb {
    let Some(color) = identity.color else {
        return activity_grey_at(luminance);
    };
    let base = activity_hsv(color);
    if base.saturation <= ACTIVITY_NEUTRAL_SATURATION {
        return activity_grey_at(luminance);
    }
    let weight = activity_chromatic_weight(base.saturation);
    let hue = (base.hue + hue_offset * weight).rem_euclid(360.0);
    let saturation = base.saturation * (1.0 + (saturation_scale - 1.0) * weight);
    let saturation = activity_reachable_saturation(hue, saturation, luminance);
    activity_hsv_color(
        hue,
        saturation,
        activity_reachable_value(hue, saturation, luminance),
    )
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

/// `activity_linear_channel`'s inverse: linear light back to an sRGB channel.
fn activity_srgb_channel(linear: f64) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let channel = if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (channel * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Mix two colours by `strength_percent` in **linear light** rather than in
/// sRGB channels.
///
/// Relative luminance is linear in linear-light RGB, so blending there keeps a
/// blend of two colours that share a luminance at that same luminance, while a
/// channel-space blend of the same two colours comes out slightly darker (the
/// sRGB transfer function is convex). The activity ramp pins every entry - hue
/// and chroma only - to the untinted cell's own luminance; without this the
/// per-tick advance of the ramp entry would wobble a lit cell's luminance by up
/// to 3% of the profile's separation, a small echo of the reported
/// "slides part way ... then loops back" jump.
fn activity_linear_mix_amount(source: Rgb, destination: Rgb, amount: f64) -> Rgb {
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.0 {
        return source;
    }
    if amount >= 1.0 {
        return destination;
    }
    let channel = |source: u8, destination: u8| {
        activity_srgb_channel(
            activity_linear_channel(source)
                + (activity_linear_channel(destination) - activity_linear_channel(source)) * amount,
        )
    };
    (
        channel(source.0, destination.0),
        channel(source.1, destination.1),
        channel(source.2, destination.2),
    )
}

fn activity_linear_mix(source: Rgb, destination: Rgb, strength_percent: u16) -> Rgb {
    activity_linear_mix_amount(
        source,
        destination,
        f64::from(strength_percent.min(100)) / 100.0,
    )
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

fn activity_hsl(color: Rgb) -> (f64, f64, f64) {
    let channel = |value: u8| f64::from(value) / 255.0;
    let red = channel(color.0);
    let green = channel(color.1);
    let blue = channel(color.2);
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let spread = max - min;
    let lightness = (max + min) / 2.0;
    if spread <= f64::EPSILON {
        return (0.0, 0.0, lightness);
    }
    let saturation = spread / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if max == red {
        ((green - blue) / spread).rem_euclid(6.0)
    } else if max == green {
        (blue - red) / spread + 2.0
    } else {
        (red - green) / spread + 4.0
    } * 60.0;
    (hue.rem_euclid(360.0), saturation, lightness)
}

fn activity_hsl_color(hue: f64, saturation: f64, lightness: f64) -> Rgb {
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let secondary = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match sector as usize % 6 {
        0 => (chroma, secondary, 0.0),
        1 => (secondary, chroma, 0.0),
        2 => (0.0, chroma, secondary),
        3 => (0.0, secondary, chroma),
        4 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    let floor = lightness - chroma / 2.0;
    let channel = |value: f64| ((value + floor) * 255.0).round().clamp(0.0, 255.0) as u8;
    (channel(red), channel(green), channel(blue))
}

/// Rotate and saturate a model identity in HSL, then put it back at the cell's
/// physical luminance. The rotation is supplied by the distance envelope, not
/// by a discrete frame-indexed palette.
fn activity_physical_accent(
    identity: ActivityIdentity,
    distance_strength: f64,
    luminance: f64,
) -> Rgb {
    let Some(color) = identity.color else {
        return activity_grey_at(luminance);
    };
    let base = activity_hsv(color);
    if base.saturation <= ACTIVITY_NEUTRAL_SATURATION {
        return activity_grey_at(luminance);
    }
    let chromatic_weight = activity_chromatic_weight(base.saturation);
    let (hue, saturation, lightness) = activity_hsl(color);
    let rotation = ACTIVITY_RAMP_HUE_SPAN * distance_strength * chromatic_weight;
    let saturation_scale = 1.0
        + (ACTIVITY_RAMP_SATURATION_STEPS[ACTIVITY_RAMP_ENTRIES - 1] - 1.0)
            * distance_strength
            * chromatic_weight;
    let shifted = activity_hsl_color(hue + rotation, saturation * saturation_scale, lightness);
    if activity_luminance(shifted) < luminance {
        activity_color_at_least(shifted, luminance)
    } else {
        activity_color_at_most(shifted, luminance)
    }
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
    let palette = if physical_shimmer(theme) {
        match theme.background() {
            TerminalBackground::Dark => {
                let baseline = activity_color_at_least(model, ACTIVITY_PHYS_DARK_BASE);
                let sweep = activity_color_at_least(baseline, ACTIVITY_PHYS_DARK_SWEEP);
                (baseline, sweep)
            }
            TerminalBackground::Light => {
                let baseline = activity_color_at_most(model, ACTIVITY_PHYS_LIGHT_BASE);
                let sweep = activity_color_at_least(baseline, ACTIVITY_PHYS_LIGHT_SWEEP);
                (baseline, sweep)
            }
            TerminalBackground::Unknown => {
                unreachable!("physical shimmer requires a known background")
            }
        }
    } else {
        match theme.background() {
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
        }
    };
    Some(palette)
}

/// Index into one ramp so it advances one entry per shimmer tick and wraps at
/// the ramp's own length.
fn ramp_index<T>(ramp: &[T], index: isize, shimmer_frame: usize) -> usize {
    (index - (shimmer_frame % ramp.len()) as isize).rem_euclid(ramp.len() as isize) as usize
}

fn activity_physical_core(baseline: Rgb, sweep: Rgb) -> Rgb {
    let baseline_luminance = activity_luminance(baseline);
    let sweep_luminance = activity_luminance(sweep);
    let separation = (sweep_luminance - baseline_luminance).abs();
    if sweep_luminance >= baseline_luminance {
        activity_color_at_least(
            sweep,
            (sweep_luminance + ACTIVITY_PHYS_CORE_LIFT * separation).min(1.0),
        )
    } else {
        activity_color_at_most(
            sweep,
            (sweep_luminance - ACTIVITY_PHYS_CORE_LIFT * separation).max(0.0),
        )
    }
}

fn activity_physical_shimmer_color(
    identity: ActivityIdentity,
    baseline: Rgb,
    sweep: Rgb,
    label: &str,
    index: isize,
    shimmer_frame: usize,
    speed: f64,
) -> Rgb {
    let sweep = match ActivityRamp::of(label) {
        Some(ramp) => activity_linear_mix_amount(baseline, sweep, ramp.sweep_depth()),
        None => sweep,
    };
    let center = phys_center(label, shimmer_frame, speed);
    let distance = index as f64 - center;
    let main = band_main(distance);
    let mut normal = activity_linear_mix_amount(baseline, sweep, main);
    let core = activity_physical_core(baseline, sweep);
    normal = activity_linear_mix_amount(
        normal,
        core,
        band_core(distance) * ACTIVITY_PHYS_CORE_WEIGHT,
    );

    // Hue and saturation follow the same continuous distance envelope as the
    // light field. At rest this is exactly the baseline; at the centre the
    // model's own HSL family gets the full narrow rotation.
    if ActivityRamp::of(label).is_some() && main > 0.0 {
        let accent = activity_physical_accent(identity, main, activity_luminance(normal));
        normal = activity_linear_mix_amount(normal, accent, main);
    }
    normal
}

fn activity_classic_shimmer_color(
    identity: ActivityIdentity,
    baseline: Rgb,
    sweep: Rgb,
    background: TerminalBackground,
    label: &str,
    index: isize,
    shimmer_frame: usize,
) -> Rgb {
    let sweep = match ActivityRamp::of(label) {
        Some(ramp) => activity_blend(baseline, sweep, ramp.sweep_depth()),
        None => sweep,
    };
    let cycle = activity_cycle(label);
    let center = (shimmer_frame % cycle) as isize + ACTIVITY_SWEEP_START;
    let sweep_strength = match (index - center).unsigned_abs() {
        distance if distance < ACTIVITY_SWEEP_FALLOFF.len() => ACTIVITY_SWEEP_FALLOFF[distance],
        _ => match background {
            TerminalBackground::Unknown => 28,
            TerminalBackground::Dark | TerminalBackground::Light => 0,
        },
    };
    let normal = (
        mix_channel(baseline.0, sweep.0, sweep_strength),
        mix_channel(baseline.1, sweep.1, sweep_strength),
        mix_channel(baseline.2, sweep.2, sweep_strength),
    );
    match ActivityRamp::of(label) {
        Some(ramp) if background != TerminalBackground::Unknown && sweep_strength > 0 => {
            let step = ramp_index(&ACTIVITY_RAMP_HUE_STEPS, index, shimmer_frame);
            let accent = ramp.accent(identity, step, activity_luminance(normal));
            activity_linear_mix(normal, accent, sweep_strength)
        }
        _ => normal,
    }
}

#[allow(clippy::too_many_arguments)]
fn activity_shimmer_color(
    theme: &OctetTheme,
    identity: ActivityIdentity,
    baseline: Rgb,
    sweep: Rgb,
    background: TerminalBackground,
    label: &str,
    index: isize,
    shimmer_frame: usize,
    rainbow_strength: u16,
) -> Rgb {
    let tinted = if physical_shimmer(theme) {
        activity_physical_shimmer_color(
            identity,
            baseline,
            sweep,
            label,
            index,
            shimmer_frame,
            physical_speed(theme),
        )
    } else {
        activity_classic_shimmer_color(
            identity,
            baseline,
            sweep,
            background,
            label,
            index,
            shimmer_frame,
        )
    };

    // The max/ultra emphasis is the one deliberate exception to "one model
    // family": for two seconds after a `max`/`ultra` run starts, `Working`
    // keeps the established whole-label rainbow. The gate is the caller's
    // `rainbow_strength`, which is zero for every other reasoning level.
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

fn activity_ansi256_rgb(index: u8) -> Rgb {
    if index >= 232 {
        let grey = 8 + (index - 232) * 10;
        return (grey, grey, grey);
    }
    if index >= 16 {
        let levels = [0, 95, 135, 175, 215, 255];
        let cube = usize::from(index - 16);
        return (levels[cube / 36], levels[cube / 6 % 6], levels[cube % 6]);
    }
    // Physical mode never selects ANSI16, but retaining a conservative value
    // here keeps this helper total if the palette implementation changes.
    (0, 0, 0)
}

fn activity_light_ansi256_safe(color: Rgb) -> Rgb {
    const SURFACE: Rgb = (224, 224, 224);
    let contrast = |candidate: Rgb| {
        let index =
            sexy_tui_rs::theme::palette::nearest_ansi256(candidate.0, candidate.1, candidate.2);
        let quantized = activity_ansi256_rgb(index);
        let first = activity_luminance(quantized);
        let second = activity_luminance(SURFACE);
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    };
    if contrast(color) >= 4.5 {
        return color;
    }

    // ANSI256 chooses by RGB distance, not by luminance. Darken only the
    // physical light-profile colour until the selected palette entry clears
    // the same representative-surface guarantee as the requested colour.
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..20 {
        let amount = (low + high) / 2.0;
        let candidate = activity_blend(color, (0, 0, 0), amount);
        if contrast(candidate) >= 4.5 {
            high = amount;
        } else {
            low = amount;
        }
    }
    activity_blend(color, (0, 0, 0), high)
}

fn activity_shimmer_foreground(theme: &OctetTheme, color: Rgb, text: &str) -> String {
    let color = if physical_shimmer(theme)
        && theme.background() == TerminalBackground::Light
        && theme.capabilities().color == ColorDepth::Ansi256
    {
        activity_light_ansi256_safe(color)
    } else {
        color
    };
    if theme.capabilities().color == ColorDepth::Ansi16 && color.0 == color.1 && color.1 == color.2
    {
        // RGB-nearest across all sixteen entries can turn grey into magenta:
        // #a7a7a7 is closer to the nominal bright-magenta entry than either
        // neighbouring grey. A neutral activity must use only neutral entries.
        // Match the theme encoder's nominal greys; physical ANSI16 colours are
        // still terminal-customizable. Chromatic/rainbow colours use the normal
        // encoder unchanged.
        let (_, index) = [(0u8, 0u8), (102, 8), (229, 7), (255, 15)]
            .into_iter()
            .min_by_key(|(grey, _)| grey.abs_diff(color.0))
            .expect("ANSI16 has neutral entries");
        return sexy_tui_rs::theme::palette::apply_foreground(
            Color::Ansi16(index),
            sexy_tui_rs::ColorDepth::Ansi16,
            text,
        );
    }
    theme.rgb_fg(color, text)
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
    let identity = ActivityIdentity::for_model(theme, reasoning);
    let mut rendered = String::with_capacity(label.len().saturating_mul(20));
    let mut index = 0;
    for grapheme in label.graphemes(true) {
        let color = activity_shimmer_color(
            theme,
            identity,
            baseline,
            sweep,
            background,
            label,
            index as isize,
            shimmer_frame,
            rainbow_strength,
        );
        rendered.push_str(&activity_shimmer_foreground(theme, color, grapheme));
        index += grapheme.width();
    }
    theme.bold(&rendered)
}

fn activity_label(reasoning: &AssistantBlock) -> &str {
    reasoning.activity_label()
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
    // Reuse the label's phase/falloff, but not its already-bright resting dot.
    // An unknown background keeps the established fallback palette unchanged.
    let (baseline, sweep) = match theme.background() {
        TerminalBackground::Dark => (
            activity_color_at_most(baseline, ACTIVITY_MARKER_DARK_BASE_LUMINANCE),
            baseline,
        ),
        TerminalBackground::Light => (
            activity_color_at_least(baseline, ACTIVITY_MARKER_LIGHT_BASE_LUMINANCE),
            baseline,
        ),
        TerminalBackground::Unknown => (baseline, sweep),
    };
    let retry_label = reasoning
        .retry_activity
        .as_ref()
        .map(|retry| retry.label_at(Instant::now()));
    let identity = ActivityIdentity::for_model(theme, reasoning);
    let color = activity_shimmer_color(
        theme,
        identity,
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
    activity_shimmer_foreground(theme, color, marker)
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
    if reasoning.retry_activity.is_some() {
        return format!("{label} {}", subdued_text(theme, "(esc to interrupt)"));
    }
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
    collapsed_reasoning_lines_sized(theme, reasoning, shimmer_frame, rainbow_strength, u16::MAX)
}

fn reasoning_inline_hint(theme: &OctetTheme) -> &'static str {
    if theme.unicode() {
        " · Ctrl+O expand"
    } else {
        " - Ctrl+O expand"
    }
}

fn thinking_hint_fits_inline(theme: &OctetTheme, status: &str, width: u16) -> bool {
    theme.is_compiled_default()
        && visible_width(status) + visible_width(reasoning_inline_hint(theme)) <= usize::from(width)
}

fn collapsed_reasoning_lines_sized(
    theme: &OctetTheme,
    reasoning: &AssistantBlock,
    shimmer_frame: usize,
    rainbow_strength: u16,
    width: u16,
) -> Vec<String> {
    if reasoning.finished {
        return reasoning.hidden_thinking_label.as_ref().filter(|_| !reasoning.text.is_empty())
            .map_or_else(Vec::new, |label| vec![theme.fg("muted", label)]);
    }
    if let Some(working) = reasoning.extension_working.as_ref().filter(|working| working.message.is_some() || working.visible.is_some() || working.frames.is_some() || working.interval_ms.is_some()) {
        let hidden = || reasoning.hidden_thinking_label.as_ref().filter(|_| !reasoning.text.is_empty()).map(|label| theme.fg("muted", label));
        if working.visible == Some(false) { return hidden().into_iter().collect(); }
        let label = working.message.as_deref().unwrap_or(if reasoning.is_working_activity() { "Working" } else { "Thinking" });
        let mut lines = vec![activity_status_line(theme, reasoning, label, shimmer_frame, rainbow_strength)];
        lines.extend(hidden());
        return lines;
    }
    if reasoning.is_working_activity() {
        if reasoning.extension_working.as_ref().is_some_and(|working| working.visible == Some(false)) { return Vec::new(); }
        let label = reasoning
            .retry_activity
            .as_ref()
            .map(|retry| retry.label_at(Instant::now()));
        let status = activity_status_line(
            theme,
            reasoning,
            label.as_deref().unwrap_or_else(|| reasoning.extension_working.as_ref().and_then(|working| working.message.as_deref()).unwrap_or("Working")),
            shimmer_frame,
            rainbow_strength,
        );
        let mut lines = vec![status];
        // File themes render the collapsed Thinking hint on a second row.
        // Reserve that row while Working is live so promotion does not shift
        // the composer. The compiled default keeps its usual one-row status.
        if !theme.is_compiled_default() {
            lines.push(String::new());
        }
        return lines;
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
        reasoning.hidden_thinking_label.as_deref().unwrap_or("Thinking"),
        shimmer_frame,
        0,
    )];
    if reasoning.show_reasoning_hint {
        if reasoning.reasoning_heading.is_none()
            && thinking_hint_fits_inline(theme, &lines[0], width)
        {
            lines[0].push_str(&subdued_text(theme, reasoning_inline_hint(theme)));
        } else {
            lines.push(reasoning_detail_line(theme, reasoning));
        }
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
        // The transcript now holds status rows, not this Markdown prefix.
        // Re-expansion must establish a full body before applying tail updates.
        reasoning.invalidate_layout();
        return collapsed_reasoning_lines_sized(
            theme,
            reasoning,
            shimmer_frame,
            rainbow_strength,
            width,
        )
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
mod tests;
