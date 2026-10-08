//! What the attached terminal can actually display, and the colour science
//! that turns a theme's declared roles into pixels.
//!
//! Why this is a separate module: every rule here is a function of the
//! *process's* terminal, not of the theme. Colour depth, truecolor support,
//! and the light/dark background all arrive from the environment at startup and
//! can change when a user reloads, so none of it belongs in a theme definition
//! or in the theme's own API. Keeping it apart also means the two halves of the
//! problem stay legible: the parent module owns *what a role means*, this one
//! owns *how that role is rendered on this terminal*.
//!
//! Four concerns live here, and they are the four ways a theme can be wrong on
//! a real terminal:
//!
//! - **Capability bridging** - [`rich_capabilities`] and [`sexy_tier`] map
//!   octet's detected [`TerminalCapabilities`] onto the renderer's own view, so
//!   a plain, non-unicode, 16-colour terminal gets a plain theme instead of a
//!   theme full of escapes it cannot draw.
//! - **Background detection** - [`terminal_background`] decides light vs dark
//!   from the terminal's own answer to OSC 11, `COLORFGBG`, or an explicit
//!   override, because contrast balancing is only correct relative to what is
//!   already on screen.
//! - **Palette approximation** - [`named_color`], the ANSI-16 table, and the
//!   256-colour cube reduce a truecolor theme to the nearest colour the
//!   terminal can show, so a theme degrades instead of breaking.
//! - **Contrast balancing** - [`relative_luminance`], [`blend`],
//!   [`balance_foreground`] and [`balance_background`] keep semantic UI signals
//!   legible against the detected background, and the standard palette tables
//!   decide which tokens are balanced at all and which stay verbatim.

use sexy_tui_rs::theme::capability::CapabilityTier;
use sexy_tui_rs::{CapabilityOverrides, SupportLevel};

use super::{OctetTheme, Rgb, TerminalBackground, TerminalCapabilities};
use crate::tui::terminal::ColorDepth;

pub(super) fn rich_capabilities(
    capabilities: TerminalCapabilities,
) -> sexy_tui_rs::TerminalCapabilities {
    if !capabilities.interactive {
        return sexy_tui_rs::TerminalCapabilities::plain();
    }
    let color_depth = match capabilities.color {
        ColorDepth::None => sexy_tui_rs::ColorDepth::None,
        ColorDepth::Ansi16 => sexy_tui_rs::ColorDepth::Ansi16,
        ColorDepth::Ansi256 => sexy_tui_rs::ColorDepth::Ansi256,
        ColorDepth::TrueColor => sexy_tui_rs::ColorDepth::TrueColor,
    };
    sexy_tui_rs::TerminalCapabilities::interactive(color_depth, capabilities.unicode)
        .with_overrides(&CapabilityOverrides {
            italics: Some(if capabilities.italics {
                SupportLevel::Supported
            } else {
                SupportLevel::Unsupported
            }),
            hyperlinks: Some(capabilities.hyperlinks),
            animation: Some(capabilities.animation),
            ..CapabilityOverrides::default()
        })
}

pub(super) fn named_color(value: &str) -> Option<Rgb> {
    let (red, green, blue) = match value.trim().to_ascii_lowercase().as_str() {
        "black" => (0, 0, 0),
        "red" => (205, 49, 49),
        "green" => (13, 188, 121),
        "yellow" => (229, 229, 16),
        "blue" => (36, 114, 200),
        "magenta" | "purple" => (188, 63, 188),
        "cyan" => (17, 168, 205),
        "white" => (229, 229, 229),
        "gray" | "grey" => (102, 102, 102),
        _ => return None,
    };
    Some(Rgb { red, green, blue })
}

pub(super) const ANSI16: [(Rgb, u8); 16] = [
    (
        Rgb {
            red: 0,
            green: 0,
            blue: 0,
        },
        30,
    ),
    (
        Rgb {
            red: 205,
            green: 49,
            blue: 49,
        },
        31,
    ),
    (
        Rgb {
            red: 13,
            green: 188,
            blue: 121,
        },
        32,
    ),
    (
        Rgb {
            red: 229,
            green: 229,
            blue: 16,
        },
        33,
    ),
    (
        Rgb {
            red: 36,
            green: 114,
            blue: 200,
        },
        34,
    ),
    (
        Rgb {
            red: 188,
            green: 63,
            blue: 188,
        },
        35,
    ),
    (
        Rgb {
            red: 17,
            green: 168,
            blue: 205,
        },
        36,
    ),
    (
        Rgb {
            red: 229,
            green: 229,
            blue: 229,
        },
        37,
    ),
    (
        Rgb {
            red: 102,
            green: 102,
            blue: 102,
        },
        90,
    ),
    (
        Rgb {
            red: 241,
            green: 76,
            blue: 76,
        },
        91,
    ),
    (
        Rgb {
            red: 35,
            green: 209,
            blue: 139,
        },
        92,
    ),
    (
        Rgb {
            red: 245,
            green: 245,
            blue: 67,
        },
        93,
    ),
    (
        Rgb {
            red: 59,
            green: 142,
            blue: 234,
        },
        94,
    ),
    (
        Rgb {
            red: 214,
            green: 112,
            blue: 214,
        },
        95,
    ),
    (
        Rgb {
            red: 41,
            green: 184,
            blue: 219,
        },
        96,
    ),
    (
        Rgb {
            red: 255,
            green: 255,
            blue: 255,
        },
        97,
    ),
];

fn color_distance(left: Rgb, right: Rgb) -> u32 {
    let red = i32::from(left.red) - i32::from(right.red);
    let green = i32::from(left.green) - i32::from(right.green);
    let blue = i32::from(left.blue) - i32::from(right.blue);
    (red * red + green * green + blue * blue) as u32
}

pub(super) fn nearest_ansi16_code(color: Rgb) -> u8 {
    ANSI16
        .iter()
        .min_by_key(|(candidate, _)| color_distance(color, *candidate))
        .map_or(37, |(_, code)| *code)
}

#[cfg(test)]
pub(super) fn ansi256_rgb(index: u8) -> Rgb {
    if index < 16 {
        return ANSI16[usize::from(index)].0;
    }
    if index < 232 {
        let value = index - 16;
        let component = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
        return Rgb {
            red: component(value / 36),
            green: component((value % 36) / 6),
            blue: component(value % 6),
        };
    }
    let gray = 8 + (index - 232) * 10;
    Rgb {
        red: gray,
        green: gray,
        blue: gray,
    }
}

pub(super) fn nearest_ansi256(color: Rgb) -> u8 {
    sexy_tui_rs::theme::palette::nearest_ansi256(color.red, color.green, color.blue)
}

pub(super) const DEFAULT_ACCENT: &str = "#16876d";

// Context reports are a legend, not a status list. Keep each category on its
// own visual channel so adjacent slices remain distinguishable even when two
// categories happen to carry the same semantic status (for example free space
// and tool schemas both used to resolve to the terminal foreground).
//
// Hex values are balanced in `OctetTheme::new`; aliases continue to follow the
// active theme and can be overridden by a theme's `[colors]` table.
pub(super) const CONTEXT_COLOR_DEFAULTS: &[(&str, &str)] = &[
    ("context_system", "#4aa8c7"),
    ("context_skills", "#d19a35"),
    ("context_tools", "#7f9fd4"),
    ("context_messages", "#d8dee8"),
    ("context_pending", "#d47d3f"),
    ("context_framing", "#73808f"),
    ("context_adjustment", "#a978c5"),
    ("context_tokenizer_adjustment", "#c36f99"),
    ("context_output", "#df6f7c"),
    ("context_free", "#52c878"),
    ("context_buffer", "#8678ba"),
];

// 0.27 gives ~5.6:1 against the test-dark reference (and ~5:1 against a
// typical #1e1e1e terminal).  We stay well below the old AAA target of
// 0.32 so foreground colours keep their saturation instead of washing out.
pub(super) const DARK_TARGET_LUMINANCE: f64 = 0.27;
pub(super) const LIGHT_TARGET_LUMINANCE: f64 = 0.11;
// Symmetric midpoint: ~4.58:1 against both pure black and pure white.
// Light-terminal users can set OCTET_COLOR_SCHEME=light for a 0.11 target.
pub(super) const UNIVERSAL_TARGET_LUMINANCE: f64 = 0.179;

// Tokens that receive terminal-background-aware luminance balancing.
// These are semantic UI signals (errors, warnings, model accent) whose
// source colours may be unreadable on dark or light terminals without
// adjustment. The compiled default additionally receives the standard
// technical code/diff palette below; user file themes keep their configured
// code colours unless they opt into their own role overrides.
pub(super) const BALANCED_FOREGROUNDS: &[(&str, &str)] = &[
    ("muted", "#777777"),
    ("dim", "#777777"),
    ("accent", DEFAULT_ACCENT),
    ("error", "#c74747"),
    ("warning", "#9a6700"),
    ("border_focused", DEFAULT_ACCENT),
];

/// Foreground tokens applied verbatim — no luminance balancing.
/// "default" means the terminal's own foreground colour.
pub(super) const VERBATIM_FOREGROUNDS: &[(&str, &str)] = &[
    ("foreground", "default"),
    ("success", "default"),
    ("info", "default"),
    ("border", "default"),
    ("border_idle", "default"),
    ("user_msg_text", "default"),
    ("assistant_msg_text", "default"),
    ("tool_title", "default"),
    ("tool_output", "default"),
    // Diff semantics are carried by row surfaces. Source text keeps its normal
    // syntax foregrounds (or the terminal foreground when no syntax applies).
    ("diff_added", "default"),
    ("diff_removed", "default"),
    ("diff_context", "default"),
    // --- Markdown chrome ------------------------------------------------
    ("md_heading", "default"),
    ("md_link", "default"),
    ("md_code", "#78a9b0"),
    ("md_code_block", "default"),
    ("md_code_border", "default"),
    ("md_quote", "default"),
    ("md_quote_border", "default"),
    ("md_hr", "default"),
    ("md_list_bullet", "default"),
    // --- syntax highlighting --------------------------------------------
    ("syntax_comment", "default"),
    ("syntax_keyword", "#815ac0"),
    ("syntax_function", "#287fb8"),
    ("syntax_variable", "#68737d"),
    ("syntax_string", "#00b847"),
    ("syntax_number", "#b26a00"),
    ("syntax_type", "#9b6500"),
    ("syntax_operator", "#b14d7d"),
    ("syntax_punctuation", "#68737d"),
];

/// Subtle terminal-background-aware surfaces. These retain their semantic hue
/// without replacing syntax foregrounds or looking like terminal selection.
pub(super) const DEFAULT_BACKGROUNDS: &[(&str, &str)] = &[("user_msg_bg", DEFAULT_ACCENT)];

// Standard technical palette for the compiled default. Source code uses one
// predictable language-neutral grammar: syntax owns foregrounds, diff owns
// quiet row surfaces, and the +/- marker carries the high-salience hue.
pub(super) const STANDARD_SYNTAX_COLORS: &[(&str, &str, &str)] = &[
    ("syntax_comment", "#9da8b5", "#505c68"),
    ("syntax_keyword", "#f29e74", "#813d00"),
    ("syntax_type", "#76c7c0", "#005c5e"),
    ("syntax_function", "#a8c7fa", "#2456a6"),
    ("syntax_variable", "#d6dee8", "#1f2933"),
    ("syntax_string", "#a8d279", "#335e00"),
    ("syntax_number", "#d6a6e8", "#7d3c98"),
    ("syntax_operator", "#aab4c0", "#4d5966"),
    ("syntax_punctuation", "#aab4c0", "#4d5966"),
    ("diff_hunk", "#8ab4f8", "#355f9e"),
];

pub(super) const STANDARD_DIFF_COLORS: &[(&str, &str, &str)] = &[
    // Preserve green hue and readable contrast after fixed-palette quantization.
    ("diff_added_marker", "#67d391", "#08652d"),
    ("diff_removed_marker", "#ff7d8a", "#b4233a"),
];

pub(super) const STANDARD_DIFF_SURFACES: &[(&str, &str, &str)] = &[
    ("diff_added_bg", "#10261e", "#e8f6ee"),
    ("diff_removed_bg", "#2a171b", "#fcebed"),
];

fn standard_foreground(dark: &str, light: &str, background: TerminalBackground) -> String {
    match background {
        TerminalBackground::Dark => dark.to_owned(),
        TerminalBackground::Light => light.to_owned(),
        TerminalBackground::Unknown => balance_foreground(light, TerminalBackground::Unknown),
    }
}

pub(super) fn standard_surface(dark: &str, light: &str, background: TerminalBackground) -> String {
    match background {
        TerminalBackground::Dark => dark.to_owned(),
        TerminalBackground::Light => light.to_owned(),
        // Unknown terminal backgrounds cannot safely receive absolute RGB row
        // surfaces. Preserve diff semantics through +/- text and marker colour.
        TerminalBackground::Unknown => "default".to_owned(),
    }
}

pub(super) fn apply_standard_technical_palette(
    theme: &mut OctetTheme,
    background: TerminalBackground,
) {
    theme.override_token("diff_added", "default");
    theme.override_token("diff_removed", "default");
    theme.override_token("diff_context", "default");
    for &(token, dark, light) in STANDARD_SYNTAX_COLORS {
        theme.override_token(token, &standard_foreground(dark, light, background));
    }
    for &(token, dark, light) in STANDARD_DIFF_COLORS {
        theme.override_token(token, &standard_foreground(dark, light, background));
    }
    for &(token, dark, light) in STANDARD_DIFF_SURFACES {
        theme.override_token(token, &standard_surface(dark, light, background));
    }
}

pub(super) fn parse_hex_color(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(Rgb {
        red: u8::from_str_radix(&hex[0..2], 16).ok()?,
        green: u8::from_str_radix(&hex[2..4], 16).ok()?,
        blue: u8::from_str_radix(&hex[4..6], 16).ok()?,
    })
}

fn hex_color(color: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue)
}

fn linear_channel(channel: u8) -> f64 {
    let channel = f64::from(channel) / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn relative_luminance(color: Rgb) -> f64 {
    0.2126 * linear_channel(color.red)
        + 0.7152 * linear_channel(color.green)
        + 0.0722 * linear_channel(color.blue)
}

fn blend_channel(source: u8, destination: u8, amount: f64) -> u8 {
    (f64::from(source) + (f64::from(destination) - f64::from(source)) * amount)
        .round()
        .clamp(0.0, 255.0) as u8
}

pub(super) fn blend(source: Rgb, destination: Rgb, amount: f64) -> Rgb {
    Rgb {
        red: blend_channel(source.red, destination.red, amount),
        green: blend_channel(source.green, destination.green, amount),
        blue: blend_channel(source.blue, destination.blue, amount),
    }
}

/// Move a web color toward black or white until all lab colors share a useful
/// perceived brightness. Equalizing luminance avoids a near-black OpenAI accent
/// beside a neon-orange Amazon accent while preserving their recognizable hue.
/// Move a colour toward the terminal background so it reads as a subtle
/// surface tint rather than a painted slab. Used for diff-add/diff-remove
/// backgrounds so they adapt to the user's terminal profile.
pub(crate) fn balance_background(source: &str, background: TerminalBackground) -> String {
    let Some(source) = parse_hex_color(source) else {
        return source.to_owned();
    };
    // Most terminals do not export COLORFGBG (Ghostty included), so treating an
    // unknown profile as "no surface" silently removes diff semantics. Use the
    // universal midpoint already used for unknown-profile foregrounds: it
    // retains the surface while remaining readable with either a black or
    // white terminal-default foreground.
    let target_luminance = match background {
        TerminalBackground::Dark => 0.025,
        TerminalBackground::Light => 0.95,
        TerminalBackground::Unknown => UNIVERSAL_TARGET_LUMINANCE,
    };
    hex_color(balance_to_luminance(source, target_luminance))
}

pub(super) fn balance_to_luminance(source: Rgb, target_luminance: f64) -> Rgb {
    let source_luminance = relative_luminance(source);
    if (source_luminance - target_luminance).abs() <= 0.002 {
        return source;
    }
    let lighten = source_luminance < target_luminance;
    let destination = if lighten {
        Rgb {
            red: 255,
            green: 255,
            blue: 255,
        }
    } else {
        Rgb {
            red: 0,
            green: 0,
            blue: 0,
        }
    };
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..20 {
        let amount = (low + high) / 2.0;
        let candidate = blend(source, destination, amount);
        let reached = if lighten {
            relative_luminance(candidate) >= target_luminance
        } else {
            relative_luminance(candidate) <= target_luminance
        };
        if reached {
            high = amount;
        } else {
            low = amount;
        }
    }
    blend(source, destination, high)
}

pub(super) fn balance_foreground(source: &str, background: TerminalBackground) -> String {
    let Some(source) = parse_hex_color(source) else {
        return source.to_owned();
    };
    let target = match background {
        TerminalBackground::Dark => DARK_TARGET_LUMINANCE,
        TerminalBackground::Light => LIGHT_TARGET_LUMINANCE,
        TerminalBackground::Unknown => UNIVERSAL_TARGET_LUMINANCE,
    };
    let source_luminance = relative_luminance(source);
    if (source_luminance - target).abs() <= 0.002 {
        return hex_color(source);
    }

    let lighten = source_luminance < target;
    let destination = if lighten {
        Rgb {
            red: 255,
            green: 255,
            blue: 255,
        }
    } else {
        Rgb {
            red: 0,
            green: 0,
            blue: 0,
        }
    };
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..20 {
        let amount = (low + high) / 2.0;
        let candidate = blend(source, destination, amount);
        let reached = if lighten {
            relative_luminance(candidate) >= target
        } else {
            relative_luminance(candidate) <= target
        };
        if reached {
            high = amount;
        } else {
            low = amount;
        }
    }
    hex_color(blend(source, destination, high))
}

pub(super) fn background_from_colorfgbg(value: &str) -> Option<TerminalBackground> {
    // COLORFGBG conventionally ends in the ANSI background index, e.g. 15;0
    // for light-on-dark and 0;15 for dark-on-light.
    let index = value.rsplit(';').next()?.trim().parse::<u8>().ok()?;
    match index {
        0..=6 | 8 => Some(TerminalBackground::Dark),
        7 | 9..=15 => Some(TerminalBackground::Light),
        _ => None,
    }
}

pub(super) fn background_from_override(value: &str) -> Option<TerminalBackground> {
    match value.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(TerminalBackground::Dark),
        "light" => Some(TerminalBackground::Light),
        "universal" | "unknown" => Some(TerminalBackground::Unknown),
        // Returning None lets terminal_background continue to COLORFGBG.
        "auto" => None,
        _ => None,
    }
}

pub(crate) fn background_from_terminal_rgb(red: u8, green: u8, blue: u8) -> TerminalBackground {
    let background = Rgb { red, green, blue };
    let luminance = relative_luminance(background);
    let contrast_with_black = (luminance + 0.05) / 0.05;
    let contrast_with_white = 1.05 / (luminance + 0.05);
    if contrast_with_black >= contrast_with_white {
        TerminalBackground::Light
    } else {
        TerminalBackground::Dark
    }
}

pub(super) fn terminal_background() -> TerminalBackground {
    std::env::var("OCTET_COLOR_SCHEME")
        .ok()
        .as_deref()
        .and_then(background_from_override)
        .or_else(|| {
            std::env::var("COLORFGBG")
                .ok()
                .as_deref()
                .and_then(background_from_colorfgbg)
        })
        .unwrap_or(TerminalBackground::Unknown)
}

pub(super) fn sexy_tier(capabilities: TerminalCapabilities) -> CapabilityTier {
    if capabilities.color == ColorDepth::TrueColor {
        CapabilityTier::TrueColor
    } else {
        CapabilityTier::Baseline
    }
}
