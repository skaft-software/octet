//! Native Pi 1.0.2 `system` color recipe, exposed as the built-in `pi` theme.
//!
//! Source: packages/coding-agent/src/modes/interactive/theme/system-theme.ts
//! at the reviewed cd32f7725fdbddbaecdff5b1e68491563394e0ca contract.
//! Upstream hashes, independently executed oracle vectors and MIT notice live
//! in pi/. No terminal I/O, configuration, adapter or JavaScript runs at runtime.

use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::extensions::resource_paths::pi_theme::{self, colors};

use super::{OctetTheme, TerminalBackground, TerminalCapabilities, ThemeSource, PI_THEME_NAME};

mod recipe;
#[cfg(test)]
mod tests;

use recipe::{FAMILIES, FOREGROUND_LEVEL, LEVELS, READABLE_FLOOR, RULES, SOLVE_ORDER, TOKENS};

type Rgb = (u8, u8, u8);
const TEXT_MINIMUM_CONTRAST: f64 = 4.5;
const BACKGROUND: usize = TOKENS.len();
const COLOR_COUNT: usize = BACKGROUND + 1;

struct Family {
    hue: f64,
    min: f64,
    max: f64,
}

struct Token {
    name: &'static str,
    family: usize,
    slot: usize,
    panel: bool,
    foreground: bool,
}

struct Curve {
    coefficients: [f64; 6],
    reachable: [f64; 2],
}

struct Rule {
    token: usize,
    on: &'static [usize],
    level: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[serde(rename_all = "lowercase")]
enum Appearance {
    Dark,
    Light,
}

impl Appearance {
    fn index(self) -> usize {
        match self {
            Self::Dark => 0,
            Self::Light => 1,
        }
    }

    fn background(self) -> TerminalBackground {
        match self {
            Self::Dark => TerminalBackground::Dark,
            Self::Light => TerminalBackground::Light,
        }
    }

    fn hint(background: TerminalBackground) -> Option<Self> {
        match background {
            TerminalBackground::Dark => Some(Self::Dark),
            TerminalBackground::Light => Some(Self::Light),
            TerminalBackground::Unknown => None,
        }
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(default, rename_all = "camelCase"))]
struct SystemInput {
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    palette: Option<[Rgb; 16]>,
    saturation: f64,
    appearance_hint: Option<Appearance>,
}

impl Default for SystemInput {
    fn default() -> Self {
        Self {
            foreground: None,
            background: None,
            palette: None,
            saturation: 1.0,
            appearance_hint: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct SystemColors {
    colors: Map<String, Value>,
    dim: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    appearance: Option<Appearance>,
}

#[derive(Clone, Copy)]
struct SourceColor {
    hue: f64,
    saturation: f64,
    lightness: f64,
    chroma: f64,
}

fn source_of(rgb: Rgb) -> SourceColor {
    let [hue, saturation, lightness] = colors::rgb_to_okhsl(rgb);
    SourceColor {
        hue,
        saturation,
        lightness,
        chroma: colors::rgb_to_oklch(rgb)[1],
    }
}

fn lightness(rgb: Rgb) -> f64 {
    colors::rgb_to_oklch(rgb)[0]
}

fn luminance((r, g, b): Rgb) -> f64 {
    let linear = |channel| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn contrast(first: Rgb, second: Rgb) -> f64 {
    let (a, b) = (luminance(first), luminance(second));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn terminal_appearance(background: Rgb, foreground: Option<Rgb>) -> Appearance {
    let white_contrast = contrast((255, 255, 255), background);
    let black_contrast = contrast((0, 0, 0), background);
    if let Some(foreground) = foreground {
        let (foreground_l, background_l) = (lightness(foreground), lightness(background));
        if (foreground_l - background_l).abs() > 0.05 {
            let appearance = if foreground_l > background_l {
                Appearance::Dark
            } else {
                Appearance::Light
            };
            let best = match appearance {
                Appearance::Dark => white_contrast,
                Appearance::Light => black_contrast,
            };
            if best >= TEXT_MINIMUM_CONTRAST {
                return appearance;
            }
        }
    }
    if white_contrast >= black_contrast {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn bell_weight(lightness: f64) -> f64 {
    let gaussian = |x: f64| (-((x - 0.5).powi(2)) / (2.0 * 0.25_f64.powi(2))).exp();
    (gaussian(lightness) - gaussian(0.0)) / (1.0 - gaussian(0.0))
}

fn saturation_curve(family: &Family, lightness: f64) -> f64 {
    let floor = if family.max > 0.0 {
        family.min / family.max
    } else {
        1.0
    };
    floor + (1.0 - floor) * bell_weight(lightness)
}

fn level_target(level: usize, appearance: Appearance, surface_l: f64) -> Option<f64> {
    let curve = &LEVELS[level][appearance.index()];
    if surface_l < curve.reachable[0] || surface_l > curve.reachable[1] {
        return None;
    }
    Some(
        curve
            .coefficients
            .iter()
            .enumerate()
            .fold(0.0, |sum, (power, coefficient)| {
                sum + coefficient * surface_l.powi(power as i32)
            }),
    )
}

fn anchored(source: SourceColor, family: &Family, lightness: f64, saturation: f64) -> Rgb {
    let anchor = saturation_curve(family, source.lightness);
    let falloff = if anchor > 0.0 {
        (saturation_curve(family, lightness) / anchor).min(1.0)
    } else {
        1.0
    };
    let color = colors::okhsl_rgb(
        source.hue,
        source.saturation * falloff * saturation,
        lightness,
    );
    let cap = source.chroma * falloff * saturation;
    let [l, c, _] = colors::rgb_to_oklch(color);
    if c <= cap {
        color
    } else {
        colors::oklch_rgb(l, cap, source.hue)
    }
}

fn with_text_contrast(color: Rgb, surfaces: &[Rgb], lighter: bool) -> Rgb {
    let meets = |candidate| {
        surfaces
            .iter()
            .all(|&surface| contrast(candidate, surface) >= TEXT_MINIMUM_CONTRAST)
    };
    if meets(color) {
        return color;
    }
    let [h, s, l] = colors::rgb_to_okhsl(color);
    let at = |lightness| colors::okhsl_rgb(h, s, lightness);
    let extreme = if lighter { 1.0 } else { 0.0 };
    if !meets(at(extreme)) {
        return at(extreme);
    }
    let (mut low, mut high) = (l, extreme);
    for _ in 0..20 {
        let middle = (low + high) / 2.0;
        if meets(at(middle)) {
            high = middle;
        } else {
            low = middle;
        }
    }
    at(high)
}

fn indexed_colors(saturation: f64, appearance: Option<Appearance>) -> SystemColors {
    let mut result = SystemColors {
        colors: Map::new(),
        dim: Vec::new(),
        appearance,
    };
    for token in TOKENS {
        let neutral = token.family == 0;
        let color = if token.panel || neutral || saturation <= 0.0 {
            Value::String(String::new())
        } else {
            Value::from(token.slot)
        };
        result.colors.insert(token.name.to_owned(), color);
        if !token.panel && neutral && !token.foreground {
            result.dim.push(token.name);
        }
    }
    result
}

fn generate(input: SystemInput) -> SystemColors {
    let saturation = input.saturation.clamp(0.0, 1.0);
    let Some(background) = input.background else {
        return indexed_colors(saturation, input.appearance_hint);
    };
    let palette = input.palette.map(|palette| palette.map(source_of));
    let appearance = terminal_appearance(background, input.foreground);
    let lighter = appearance == Appearance::Dark;
    let extreme = if lighter { 1.0 } else { 0.0 };
    let background_l = lightness(background);
    let paint = |token: usize, oklab_l| {
        let lightness = colors::oklab_to_okhsl_lightness(oklab_l);
        let token = &TOKENS[token];
        let family = &FAMILIES[token.family];
        if let Some(palette) = &palette {
            anchored(palette[token.slot], family, lightness, saturation)
        } else {
            colors::okhsl_rgb(
                family.hue,
                (family.min + (family.max - family.min) * bell_weight(lightness)) * saturation,
                lightness,
            )
        }
    };
    let target = |level, surface_l, t| {
        let reached = level_target(level, appearance, surface_l);
        if reached.is_none() && t == 0.0 {
            return None;
        }
        let distance = reached.unwrap_or(extreme) - surface_l;
        let floor = level_target(READABLE_FLOOR[appearance.index()], appearance, surface_l)
            .unwrap_or(extreme)
            - surface_l;
        let compressed = if distance.abs() > floor.abs() {
            distance - (distance - floor) * f64::min(t, 1.0)
        } else {
            distance
        };
        Some(surface_l + compressed * (1.0 - f64::max(0.0, t - 1.0)))
    };
    let extreme_text = if lighter { (255, 255, 255) } else { (0, 0, 0) };
    let readable = |color| contrast(extreme_text, color) >= TEXT_MINIMUM_CONTRAST;
    let limit_panel = |token, l| {
        let color = paint(token, l);
        if readable(color) {
            return color;
        }
        let (mut low, mut high) = (background_l, l);
        for _ in 0..20 {
            let middle = (low + high) / 2.0;
            if readable(paint(token, middle)) {
                low = middle;
            } else {
                high = middle;
            }
        }
        paint(token, low)
    };
    let solve = |t| -> Option<[Option<Rgb>; COLOR_COUNT]> {
        let mut colors = [None; COLOR_COUNT];
        colors[BACKGROUND] = Some(background);
        for &token in SOLVE_ORDER {
            let mut l = if lighter {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
            for rule in RULES.iter().filter(|rule| rule.token == token) {
                for &surface in rule.on {
                    let value = target(
                        rule.level,
                        lightness(colors[surface].unwrap_or(background)),
                        t,
                    )?;
                    if !(0.0..=1.0).contains(&value) {
                        return None;
                    }
                    l = if lighter { l.max(value) } else { l.min(value) };
                }
            }
            colors[token] = Some(if TOKENS[token].panel {
                limit_panel(token, l)
            } else {
                paint(token, l)
            });
        }
        Some(colors)
    };
    let mut relaxation = 0.0;
    let mut solved = solve(0.0);
    if solved.is_none() {
        let (mut low, mut high) = (0.0, 2.0);
        solved = solve(high);
        for _ in 0..20 {
            let middle = (low + high) / 2.0;
            if let Some(attempt) = solve(middle) {
                high = middle;
                solved = Some(attempt);
            } else {
                low = middle;
            }
        }
        relaxation = high;
    }
    let solved = solved.expect("Pi full relaxation fits every recipe level");
    let mut result = SystemColors {
        colors: TOKENS
            .iter()
            .enumerate()
            .map(|(index, token)| {
                (
                    token.name.to_owned(),
                    Value::String(hex(
                        solved[index].expect("Pi solve order covers every token")
                    )),
                )
            })
            .collect(),
        dim: Vec::new(),
        appearance: Some(appearance),
    };
    for (index, token) in TOKENS
        .iter()
        .enumerate()
        .filter(|(_, token)| token.foreground)
    {
        let surfaces: Vec<_> = RULES
            .iter()
            .filter(|rule| rule.token == index)
            .flat_map(|rule| {
                rule.on
                    .iter()
                    .map(|&surface| solved[surface].unwrap_or(background))
            })
            .collect();
        let mut text = solved[index].expect("Pi foreground token is solved");
        if let Some(foreground) = input.foreground {
            let targets: Option<Vec<_>> = surfaces
                .iter()
                .map(|&surface| target(FOREGROUND_LEVEL, lightness(surface), relaxation))
                .collect();
            if let Some(targets) =
                targets.filter(|targets| targets.iter().all(|value| (0.0..=1.0).contains(value)))
            {
                let needed = if lighter {
                    targets.into_iter().fold(f64::NEG_INFINITY, f64::max)
                } else {
                    targets.into_iter().fold(f64::INFINITY, f64::min)
                };
                let foreground_l = lightness(foreground);
                if if lighter {
                    foreground_l >= needed
                } else {
                    foreground_l <= needed
                } {
                    result
                        .colors
                        .insert(token.name.to_owned(), Value::String(String::new()));
                    continue;
                }
                text = anchored(
                    source_of(foreground),
                    &FAMILIES[0],
                    colors::oklab_to_okhsl_lightness(needed),
                    saturation,
                );
            }
        }
        result.colors.insert(
            token.name.to_owned(),
            Value::String(hex(with_text_contrast(text, &surfaces, lighter))),
        );
    }
    result
}

/// Deterministic native reference profiles when no terminal RGB is available.
/// The guessed canvas/foreground pair is Pi's theme.ts GUESSED_DEFAULT_COLORS,
/// not Octet's model palette. Unknown stays terminal-owned ANSI/default/faint.
pub(super) fn pi_theme_for(
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    let (foreground, terminal_background) = match background {
        TerminalBackground::Dark => (Some((229, 229, 231)), Some((0, 0, 0))),
        TerminalBackground::Light => (Some((0, 0, 0)), Some((255, 255, 255))),
        TerminalBackground::Unknown => (None, None),
    };
    pi_theme_with_colors(
        capabilities,
        background,
        foreground,
        terminal_background,
        None,
    )
}

/// Faithful reported-color tiers. `background` is an appearance hint only;
/// reported foreground/background decide actual appearance. Without reported
/// background this returns indexed fallback, even if a palette or hint exists.
pub(super) fn pi_theme_with_colors(
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
    foreground: Option<Rgb>,
    terminal_background: Option<Rgb>,
    palette: Option<[Rgb; 16]>,
) -> anyhow::Result<OctetTheme> {
    let generated = generate(SystemInput {
        foreground,
        background: terminal_background,
        palette,
        appearance_hint: Appearance::hint(background),
        ..SystemInput::default()
    });
    compile(capabilities, background, &generated)
}

// The shared data-only Pi projection is authoritative for token names. This
// correspondence is needed additionally for native semantic SGR faint/styles.
const NATIVE_FOREGROUNDS: &[(&str, &str)] = &[
    ("accent", "accent"),
    ("border", "border"),
    ("borderAccent", "border_focused"),
    ("borderMuted", "border_idle"),
    ("success", "success"),
    ("error", "error"),
    ("warning", "warning"),
    ("muted", "muted"),
    ("dim", "dim"),
    ("text", "foreground"),
    ("thinkingText", "reasoning_text"),
    ("userMessageText", "user_msg_text"),
    ("toolTitle", "tool_title"),
    ("toolOutput", "tool_output"),
    ("mdHeading", "md_heading"),
    ("mdLink", "md_link"),
    ("mdCode", "md_code"),
    ("mdCodeBlock", "md_code_block"),
    ("mdCodeBlockBorder", "md_code_border"),
    ("mdQuote", "md_quote"),
    ("mdQuoteBorder", "md_quote_border"),
    ("mdHr", "md_hr"),
    ("mdListBullet", "md_list_bullet"),
    ("toolDiffAdded", "diff_added"),
    ("toolDiffRemoved", "diff_removed"),
    ("toolDiffContext", "diff_context"),
    ("syntaxComment", "syntax_comment"),
    ("syntaxKeyword", "syntax_keyword"),
    ("syntaxFunction", "syntax_function"),
    ("syntaxVariable", "syntax_variable"),
    ("syntaxString", "syntax_string"),
    ("syntaxNumber", "syntax_number"),
    ("syntaxType", "syntax_type"),
    ("syntaxOperator", "syntax_operator"),
    ("syntaxPunctuation", "syntax_punctuation"),
];

fn native_value(color: &Value) -> String {
    if let Some(index) = color.as_u64() {
        format!("index:{index}")
    } else {
        let color = color
            .as_str()
            .expect("Pi generator emits color string or index");
        if color.is_empty() {
            "default".to_owned()
        } else {
            color.to_owned()
        }
    }
}

fn compile(
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
    generated: &SystemColors,
) -> anyhow::Result<OctetTheme> {
    let mut document = serde_json::json!({"name": PI_THEME_NAME, "colors": generated.colors});
    if let Some(appearance) = generated.appearance {
        document["appearance"] = serde_json::to_value(appearance)?;
    }
    let source = pi_theme::native_source(Path::new("pi.json"), &document.to_string())?.into_owned();
    let mut native: toml::Table = toml::from_str(&source)?;
    let metadata = native["metadata"]
        .as_table_mut()
        .expect("Pi projection metadata");
    metadata.insert(
        "description".into(),
        toml::Value::String(
            "Pi 1.0.2 system colors, generated natively from terminal colors".into(),
        ),
    );
    metadata.insert("author".into(), toml::Value::String("Mario Zechner".into()));
    let palette = native["colors"]
        .as_table_mut()
        .expect("Pi projection colors");
    for (key, value) in [
        ("prompt_wash", toml::Value::Boolean(false)),
        ("prompt_rail_model_adaptive", toml::Value::Boolean(false)),
        ("splash_model_adaptive", toml::Value::Boolean(false)),
        (
            "assistant_msg_text",
            toml::Value::String(native_value(&generated.colors["text"])),
        ),
        (
            "model_accent",
            toml::Value::String(native_value(&generated.colors["accent"])),
        ),
        (
            "model_assistant",
            toml::Value::String(native_value(&generated.colors["text"])),
        ),
        ("md_code_bg", toml::Value::String("default".into())),
        ("md_code_inline_bg", toml::Value::String("default".into())),
    ] {
        palette.insert(key.into(), value);
    }
    native.insert(
        "model".into(),
        toml::Value::Table(toml::Table::from_iter([(
            "use_lab_color".into(),
            toml::Value::Boolean(false),
        )])),
    );
    let roles = native["roles"].as_table_mut().expect("Pi projection roles");
    for token in TOKENS {
        roles
            .get_mut(&format!("extension.pi.{}", token.name))
            .expect("complete Pi projection")
            .as_table_mut()
            .expect("Pi role table")
            .insert(
                "dim".into(),
                toml::Value::Boolean(generated.dim.contains(&token.name)),
            );
    }
    // Native semantic roles are installed after required Octet defaults, so
    // diff foregrounds and faint neutral roles are not lost during compilation.
    for &(pi, native) in NATIVE_FOREGROUNDS {
        if super::semantic_text_role(native).is_some() {
            roles.insert(
                native.to_owned(),
                toml::Value::Table(toml::Table::from_iter([
                    (
                        "foreground".into(),
                        toml::Value::String(native_value(&generated.colors[pi])),
                    ),
                    (
                        "dim".into(),
                        toml::Value::Boolean(generated.dim.contains(&pi)),
                    ),
                ])),
            );
        }
    }
    let source = toml::to_string(&native)?;
    let actual_background = generated
        .appearance
        .map(Appearance::background)
        .unwrap_or(background);
    let mut theme = super::load_theme_source_for(
        &source,
        PI_THEME_NAME,
        ThemeSource::CompiledPi,
        PI_THEME_NAME,
        capabilities,
        actual_background,
    )?;
    // In particular, an explicit default user background must stay default:
    // Octet's required-surface pass otherwise fills it with its own green wash.
    for (token, value) in native["colors"].as_table().expect("Pi colors") {
        if let Some(value) = value.as_str() {
            theme.override_token(token, value);
        }
    }
    for &(pi, native) in NATIVE_FOREGROUNDS {
        super::apply_role_style(
            &mut theme,
            native,
            &super::RoleStyleSpec {
                foreground: Some(native_value(&generated.colors[pi])),
                dim: Some(generated.dim.contains(&pi)),
                adaptive: Some(false),
                ..Default::default()
            },
            false,
        )?;
    }
    Ok(theme)
}
