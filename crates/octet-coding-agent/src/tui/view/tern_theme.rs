//! Resolved octet semantics, not Tern's fallback/omp theme or a TOML re-parser.

use octet_tern::wire::{Palette, VariantNames};
use serde_json::{Map, Value};
use sexy_tui_rs::Color;

use crate::tui::terminal::ColorDepth;
use crate::tui::theme::{apply_model_lab, ModelLab, OctetTheme, TerminalBackground};

pub(super) struct NativeTheme {
    pub(super) dark: OctetTheme,
    pub(super) light: OctetTheme,
    pub(super) palette: Palette,
}

impl NativeTheme {
    pub(super) fn resolve(
        theme: &OctetTheme,
        lab: Option<ModelLab>,
        surface: &str,
    ) -> anyhow::Result<Self> {
        let resolve = |background| -> anyhow::Result<OctetTheme> {
            let mut resolved = theme.for_native_background(background)?;
            if let Some(lab) = lab {
                apply_model_lab(&mut resolved, lab);
            }
            Ok(resolved)
        };
        let dark = resolve(TerminalBackground::Dark)?;
        let light = resolve(TerminalBackground::Light)?;
        let name = theme.metadata().name.clone();
        let palette = Palette {
            sf: surface.into(),
            dark: Some(variant(&dark)),
            light: Some(variant(&light)),
            name: Some(VariantNames {
                dark: Some(name.clone()),
                light: Some(name),
            }),
        };
        Ok(Self {
            dark,
            light,
            palette,
        })
    }

    pub(super) fn active(&self, dark: bool) -> &OctetTheme {
        if dark {
            &self.dark
        } else {
            &self.light
        }
    }
}

// TSP's token vocabulary is fixed, but the sources here are octet's compiled
// semantic roles. The runtime accent is deliberately model_accent, never the
// static branding/accent token. Default colours are omitted so the terminal's
// own readable canvas remains authoritative.
fn variant(theme: &OctetTheme) -> Map<String, Value> {
    let mut out = Map::new();
    if theme.capabilities().color == ColorDepth::None {
        return out;
    }
    for (token, role) in [
        ("text", "text"),
        ("muted", "muted"),
        ("dim", "dim"),
        ("accent", "model_accent"),
        ("info", "info"),
        ("success", "success"),
        ("warning", "warning"),
        ("error", "error"),
        ("border", "border"),
        ("borderMuted", "border_idle"),
        ("borderAccent", "model_accent"),
        ("userMessageText", "user_msg_text"),
        ("customMessageText", "model_assistant"),
        ("customMessageLabel", "model_accent"),
        ("toolTitle", "tool_title"),
        ("toolOutput", "tool_output"),
        ("thinkingText", "reasoning_text"),
        ("thinkingOff", "dim"),
        ("thinkingMinimal", "muted"),
        ("thinkingLow", "model_accent"),
        ("thinkingMedium", "model_accent"),
        ("thinkingHigh", "model_accent"),
        ("thinkingXhigh", "model_accent"),
        ("thinkingMax", "model_accent"),
        ("bashMode", "model_accent"),
        ("pythonMode", "model_accent"),
        ("mdHeading", "heading"),
        ("mdLink", "link"),
        ("mdLinkUrl", "link"),
        ("mdCode", "inline_code"),
        ("mdCodeBlock", "code"),
        ("mdCodeBlockBorder", "md_code_border"),
        ("mdQuote", "quote"),
        ("mdQuoteBorder", "md_quote_border"),
        ("mdHr", "md_hr"),
        ("mdListBullet", "list_marker"),
        ("toolDiffAdded", "diff_add"),
        ("toolDiffRemoved", "diff_remove"),
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
        ("statusLineSep", "dim"),
        ("statusLineModel", "model_accent"),
        ("statusLinePath", "muted"),
        ("statusLineContext", "muted"),
        ("statusLineSpend", "muted"),
        ("statusLineCost", "muted"),
        ("statusLineOutput", "tool_output"),
        ("statusLineSubagents", "model_accent"),
        ("statusLineGitClean", "success"),
        ("statusLineGitDirty", "warning"),
        ("statusLineStaged", "success"),
        ("statusLineDirty", "warning"),
        ("statusLineUntracked", "error"),
    ] {
        put(&mut out, token, theme.semantic_style(role).foreground);
    }
    for (token, role, fallback) in [
        ("userMessageBg", "surface.user", "user_msg_bg"),
        ("customMessageBg", "surface.assistant", "assistant_msg_bg"),
        ("toolPendingBg", "surface.tool", "tool_pending_bg"),
        ("toolSuccessBg", "surface.tool", "tool_success_bg"),
        ("toolErrorBg", "surface.tool", "tool_error_bg"),
        ("statusLineBg", "surface.shell", "composer_bg"),
        ("pageBg", "surface.shell", "background"),
        ("cardBg", "surface.assistant", "surface"),
        ("infoBg", "code", "md_code_bg"),
        ("selectedBg", "selected", "selected_bg"),
    ] {
        let background = theme.semantic_style(role).background;
        put(
            &mut out,
            token,
            if background == Color::Default {
                theme.semantic_style(fallback).foreground
            } else {
                background
            },
        );
    }
    if let Some(accent) = theme.model_rgb(None) {
        // ANSI's default composer uses model ink for its rule and a quieter
        // version at rest. Export both, rather than leaving native chrome on
        // Tern's neutral/fallback borders. Custom themes keep their own borders.
        if theme.is_compiled_default() {
            let border = theme.role_rgb("composer_border").unwrap_or(accent);
            put(&mut out, "border", Color::Rgb(border.0, border.1, border.2));
            put(
                &mut out,
                "borderAccent",
                Color::Rgb(border.0, border.1, border.2),
            );
            let idle = theme.composer_idle_rgb(border);
            put(&mut out, "borderMuted", Color::Rgb(idle.0, idle.1, idle.2));
        }
        if theme.prompt_wash() {
            if let Some((r, g, b)) = theme.native_prompt_rgb(accent) {
                put(&mut out, "userMessageBg", Color::Rgb(r, g, b));
                let text = if theme.background() == TerminalBackground::Light {
                    (32, 35, 39)
                } else {
                    (230, 230, 235)
                };
                put(
                    &mut out,
                    "userMessageText",
                    Color::Rgb(text.0, text.1, text.2),
                );
            }
        }
        let mix = |b: (u8, u8, u8), amount: f32| {
            let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount) as u8;
            Color::Rgb(
                channel(accent.0, b.0),
                channel(accent.1, b.1),
                channel(accent.2, b.2),
            )
        };
        let base = if theme.background() == TerminalBackground::Light {
            (255, 255, 255)
        } else {
            (21, 24, 32)
        };
        for (token, amount) in [
            ("thinkingMinimal", 0.65),
            ("thinkingLow", 0.5),
            ("thinkingMedium", 0.3),
            ("selectedBg", 0.82),
        ] {
            if token != "selectedBg" || theme.is_compiled_default() {
                put(&mut out, token, mix(base, amount));
            }
        }
        let high = if theme.background() == TerminalBackground::Light {
            (0, 0, 0)
        } else {
            (255, 255, 255)
        };
        for (token, amount) in [
            ("thinkingXhigh", 0.25),
            ("thinkingMax", 0.45),
            ("thinkingUltra", 0.6),
        ] {
            put(&mut out, token, mix(high, amount));
        }
    }

    // Built-in and imported Pi themes own their exact palette, including tool
    // state fills and thinking ramps. Do not replace them with model-derived ink.
    if theme.is_pi_theme() {
        for role in theme.semantic_role_names() {
            let Some(token) = role.strip_prefix("extension.pi.") else {
                continue;
            };
            let style = theme.semantic_style(role);
            out.remove(token);
            put(
                &mut out,
                token,
                if token.ends_with("Bg") {
                    style.background
                } else {
                    style.foreground
                },
            );
        }
    }
    out
}

fn put(out: &mut Map<String, Value>, token: &str, color: Color) {
    if let Some(color) = hex(color) {
        out.insert(token.into(), Value::String(color));
    }
}

// Native CSS cannot consume ANSI/index:N. Expand the standard xterm table at
// the adapter boundary instead of silently discarding limited-palette themes.
fn hex(color: Color) -> Option<String> {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    let (r, g, b) = match color {
        Color::Default => return None,
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Ansi16(index) | Color::Indexed(index @ 0..=15) => BASE[usize::from(index)],
        Color::Indexed(index @ 16..=231) => {
            let index = index - 16;
            let channel = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            (
                channel(index / 36),
                channel(index / 6 % 6),
                channel(index % 6),
            )
        }
        Color::Indexed(index) => {
            let n = 8 + (index - 232) * 10;
            (n, n, n)
        }
    };
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::terminal::TerminalCapabilities;

    #[test]
    fn native_custom_theme_reuses_the_validated_snapshot_and_variant_roles() {
        let source = r##"
[colors]
foreground = "#d8d8d8"
muted = "index:244"
[roles.tool_output]
foreground = "index:42"
[variants.dark.colors]
foreground = "#f0e8d8"
[variants.light.colors]
foreground = "#252020"
"##;
        let theme = crate::tui::theme::test_theme_source_with(
            source,
            TerminalCapabilities::test(true, false, ColorDepth::Ansi256),
            TerminalBackground::Dark,
        );
        let native = NativeTheme::resolve(&theme, None, "test").unwrap();
        assert_eq!(native.dark.metadata().name, theme.metadata().name);
        assert_ne!(
            native.dark.semantic_style("text").foreground,
            native.light.semantic_style("text").foreground
        );
        for (variant, resolved) in [
            (native.palette.dark.as_ref().unwrap(), &native.dark),
            (native.palette.light.as_ref().unwrap(), &native.light),
        ] {
            assert_eq!(
                variant["text"],
                hex(resolved.semantic_style("text").foreground).unwrap()
            );
            assert_eq!(
                variant["toolOutput"],
                hex(resolved.semantic_style("tool_output").foreground).unwrap()
            );
            assert!(!resolved.unicode());
        }
    }

    #[test]
    fn no_color_never_guesses_native_colors_or_enables_unicode() {
        let theme = crate::tui::theme::test_theme_for(
            TerminalBackground::Dark,
            TerminalCapabilities::test(true, false, ColorDepth::None),
        );
        let native = NativeTheme::resolve(&theme, Some(ModelLab::Anthropic), "test").unwrap();
        assert!(native.palette.dark.unwrap().is_empty());
        assert!(native.palette.light.unwrap().is_empty());
        assert!(!native.dark.unicode());
        assert!(!native.light.unicode());
    }

    #[test]
    fn native_palettes_are_model_adaptive_in_both_appearances() {
        let theme = crate::tui::theme::test_theme_for(
            TerminalBackground::Unknown,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        );
        let a = NativeTheme::resolve(&theme, Some(ModelLab::Anthropic), "test").unwrap();
        let b = NativeTheme::resolve(&theme, Some(ModelLab::OpenAi), "test").unwrap();
        assert_ne!(
            a.palette.dark.as_ref().unwrap()["accent"],
            b.palette.dark.as_ref().unwrap()["accent"]
        );
        assert_ne!(a.palette.dark, a.palette.light);
        for native in [&a, &b] {
            for (palette, resolved) in [
                (native.palette.dark.as_ref().unwrap(), &native.dark),
                (native.palette.light.as_ref().unwrap(), &native.light),
            ] {
                assert_eq!(palette["border"], palette["accent"]);
                assert_eq!(palette["borderAccent"], palette["accent"]);
                let idle = resolved.composer_idle_rgb(resolved.model_rgb(None).unwrap());
                assert_eq!(
                    palette["borderMuted"],
                    hex(Color::Rgb(idle.0, idle.1, idle.2)).unwrap()
                );
            }
        }
        assert_eq!(
            a.palette.dark.as_ref().unwrap()["accent"],
            hex(a.dark.semantic_style("model_accent").foreground).unwrap()
        );
    }

    #[test]
    fn custom_fills_survive_model_adaptation_when_prompt_wash_is_disabled() {
        let source = r##"
[metadata]
adaptive = false
[tokens]
prompt_wash = false
selected_bg = "#123456"
[roles."surface.user"]
background = "#203040"
"##;
        let theme = crate::tui::theme::test_theme_source_with(
            source,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            TerminalBackground::Dark,
        );
        let native = NativeTheme::resolve(&theme, Some(ModelLab::DeepSeek), "test").unwrap();
        let palette = native.palette.dark.unwrap();
        assert_eq!(palette["userMessageBg"], "#203040");
        assert_eq!(palette["selectedBg"], "#123456");
    }

    #[test]
    fn indexed_and_named_colors_do_not_disappear() {
        assert_eq!(hex(Color::Ansi16(1)).as_deref(), Some("#800000"));
        assert_eq!(hex(Color::Indexed(16)).as_deref(), Some("#000000"));
        assert_eq!(hex(Color::Indexed(231)).as_deref(), Some("#ffffff"));
        assert_eq!(hex(Color::Indexed(244)).as_deref(), Some("#808080"));
        assert!(hex(Color::Default).is_none());
    }
}
