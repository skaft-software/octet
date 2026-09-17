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
/// whole hue span away from itself. That is this ramp's version of the reported
/// warm `Working` shimmer: the defect was a hue set applied *regardless* of the
/// identity, and a near-neutral identity is the one case where the ramp has no
/// chroma of its own to spend. Every compiled-in lab colour other than the exact
/// grey of `OpenAi` resolves to at least 0.37 (`Cohere`) on every terminal
/// profile, so no shipped model identity loses the treatment.
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
fn activity_linear_mix(source: Rgb, destination: Rgb, strength_percent: u16) -> Rgb {
    let strength = f64::from(strength_percent.min(100)) / 100.0;
    let channel = |source: u8, destination: u8| {
        activity_srgb_channel(
            activity_linear_channel(source)
                + (activity_linear_channel(destination) - activity_linear_channel(source))
                    * strength,
        )
    };
    (
        channel(source.0, destination.0),
        channel(source.1, destination.1),
        channel(source.2, destination.2),
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

#[allow(clippy::too_many_arguments)]
fn activity_shimmer_color(
    identity: ActivityIdentity,
    baseline: Rgb,
    sweep: Rgb,
    background: TerminalBackground,
    label: &str,
    index: isize,
    shimmer_frame: usize,
    rainbow_strength: u16,
) -> Rgb {
    // The label's own sweep depth: both statuses share the colour identity and
    // differ only in how far the highlight travels from the resting colour.
    let sweep = match ActivityRamp::of(label) {
        Some(ramp) => activity_blend(baseline, sweep, ramp.sweep_depth()),
        None => sweep,
    };
    // One terminal cell per tick. `activity_cycle` keeps the sweep inside the
    // label for the whole traverse and leaves the ends of the cycle at rest.
    let cycle = activity_cycle(label);
    let center = (shimmer_frame % cycle) as isize + ACTIVITY_SWEEP_START;
    // The falloff is deliberately wider than the old 100/78/48/0 ramp. With
    // the increased luminance separation one lit neighbour cell used to sit at
    // ~78% of a 0.04 move (invisible); now four trailing cells stay visibly
    // graded (84/64/40/18) so the highlight reads as a moving sweep rather than
    // a single blinking cell. Every step is monotone in distance, so the centre
    // remains the most distinct cell.
    let sweep_strength = match (index - center).unsigned_abs() {
        distance if distance < ACTIVITY_SWEEP_FALLOFF.len() => {
            ACTIVITY_SWEEP_FALLOFF[distance as usize]
        }
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
    // The status's own ramp travels with the sweep: resting cells (strength 0)
    // keep the plain foreground, so the label never becomes a flat wash of
    // colour. The ramp is a variation of the *model's* colour - the same
    // identity the resting label uses - and both status labels share it
    // exactly; only the sweep depth differs. Unknown backgrounds keep the
    // established neutral fallback and never take a label ramp.
    let tinted = match ActivityRamp::of(label) {
        Some(ramp) if background != TerminalBackground::Unknown && sweep_strength > 0 => {
            let step = ramp_index(&ACTIVITY_RAMP_HUE_STEPS, index, shimmer_frame);
            let accent = ramp.accent(identity, step, activity_luminance(normal));
            // Blended in linear light: both colours are pinned to the untinted
            // cell's own luminance, so the tint contributes hue and chroma only
            // and the cell's frame-to-frame luminance move stays exactly the
            // plain sweep's - one falloff step at most, whatever the ramp entry
            // does on that tick. At rest (`sweep_strength == 0`) the cell keeps
            // the plain foreground byte-for-byte, so the rest gap is exact.
            activity_linear_mix(normal, accent, sweep_strength)
        }
        _ => normal,
    };
    // The max/ultra emphasis is the one deliberate exception to "one model
    // family": for two seconds after a `max`/`ultra` run starts, `Working`
    // keeps the established whole-label rainbow, so the emphasis tints every
    // cell rather than only the travelling centre. The gate is the caller's
    // `rainbow_strength` (`status_rainbow_strength_at`), which is zero for every
    // other reasoning level, so `high` and below can never reach this branch.
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

fn activity_shimmer_foreground(theme: &OctetTheme, color: Rgb, text: &str) -> String {
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
                let theme = theme::test_theme_for(background, capabilities);
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
                                let rendered = activity_shimmer_label(
                                    &theme, &reasoning, label, frame, strength,
                                );
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
                            for frame in 0..activity_cycle(label) {
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
                let theme = theme::test_theme_for(
                    background,
                    TerminalCapabilities::test(true, true, depth),
                );
                for lab in [Some(ModelLab::OpenAi), Some(ModelLab::Alibaba), None] {
                    for label in ["Working", "Thinking", "Compacting context"] {
                        let reasoning = activity_reasoning(lab, label);
                        for marker in ["•", "*"] {
                            let render = |frame| {
                                activity_shimmer_marker(&theme, &reasoning, frame, 0, marker)
                            };
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
                                    assert_eq!(
                                        rendered, resting,
                                        "dot must rest outside the sweep"
                                    );
                                } else {
                                    assert_ne!(
                                        rendered, resting,
                                        "dot must pulse inside the sweep"
                                    );
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
                            assert_eq!(
                                centre_frame(0) - peak_frame,
                                ACTIVITY_LABEL_OFFSET as usize
                            );
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
                let theme = theme::test_theme_for(
                    background,
                    TerminalCapabilities::test(true, true, depth),
                );
                for lab in [
                    Some(ModelLab::OpenAi),
                    Some(ModelLab::Alibaba),
                    Some(ModelLab::Meta),
                    Some(ModelLab::Google),
                    None,
                ] {
                    for label in ["Working", "Thinking", "Compacting context"] {
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
        let ansi16 =
            theme::test_theme_with(TerminalCapabilities::test(true, true, ColorDepth::Ansi16));
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
            let theme = theme::test_theme_for(
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

        let lab = crate::tui::theme::classify_model_identity(
            "gpt-6-astra",
            "gpt-6-astra",
            "openai-codex",
        );
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
                let theme = theme::test_theme_for(
                    background,
                    TerminalCapabilities::test(true, true, depth),
                );
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
                            for step in 0..ACTIVITY_RAMP_ENTRIES {
                                let entry =
                                    ramp.accent(ActivityIdentity { color: None }, step, 0.5);
                                assert_eq!(
                                    chroma(entry),
                                    0.0,
                                    "{background:?}/{depth:?}/{label}: ramp entry {step} must be an \
                                     exact grey when the session has no model identity, got \
                                     {entry:?} for hue rotation {:.0} degrees",
                                    ACTIVITY_RAMP_HUE_SPAN * ACTIVITY_RAMP_HUE_STEPS[step]
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
            let theme = theme::test_theme_for(
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
    /// that is, stays nearly luminance-only. Nothing here reads a model id or a
    /// lab name: the fixture is a custom theme and the weight is read back from
    /// whatever colour that theme resolves.
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
                saturation > ACTIVITY_NEUTRAL_SATURATION
                    && saturation < ACTIVITY_CHROMATIC_SATURATION,
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
            let theme = theme::test_theme_for(
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
            let theme = theme::test_theme_for(
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
            let theme = theme::test_theme_for(
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
                        && activity_shimmer_marker(&theme, &reasoning, frame, 0, "•")
                            == resting_marker;
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
            let theme = theme::test_theme_for(
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
                                identity, baseline, sweep, background, label, index, frame, 0,
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
            let theme = theme::test_theme_for(
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
                                (luminance(mixed) - luminance(normal)).abs()
                                    <= ROUND_TRIP_ALLOWANCE,
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
        let reasoning =
            AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
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
        let reasoning =
            AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::Alibaba));
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
