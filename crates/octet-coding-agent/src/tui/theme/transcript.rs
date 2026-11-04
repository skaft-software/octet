//! Defaults for transcript-specific roles; file styles overlay these defaults.
use std::collections::BTreeMap;

use super::{
    balance_foreground, parse_hex_color, Color, OctetTheme, RoleStyleSpec, TerminalBackground,
    TextStyle,
};

#[derive(Clone, Copy)]
pub(super) enum TranscriptRole {
    Thinking,
    ToolHeader,
    ScrollHint,
}

impl TranscriptRole {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            "thinking" => Some(Self::Thinking),
            "tool_header" => Some(Self::ToolHeader),
            "scroll_hint" => Some(Self::ScrollHint),
            _ => None,
        }
    }

    pub(super) fn default_style(self, theme: &OctetTheme) -> TextStyle {
        match self {
            // No fixed gray guarantees contrast against an unobserved canvas.
            Self::Thinking if theme.background == TerminalBackground::Unknown => TextStyle::plain(),
            Self::Thinking => {
                let foreground = balance_foreground("#777777", theme.background);
                let color = parse_hex_color(&foreground)
                    .map(|color| Color::Rgb(color.red, color.green, color.blue))
                    .unwrap_or(Color::Default);
                TextStyle::plain().foreground(color)
            }
            Self::ToolHeader => {
                let role = if theme.metadata.profile == "pi" || theme.is_pi_theme() {
                    "tool_title"
                } else {
                    "foreground"
                };
                legacy_fg_style(theme, role).bold()
            }
            Self::ScrollHint => legacy_fg_style(theme, "muted"),
        }
    }
}

// `fg` used an explicitly configured semantic style, but otherwise only the
// token's color (not the rich renderer's default attributes, such as muted dim).
fn legacy_fg_style(theme: &OctetTheme, role: &str) -> TextStyle {
    theme.semantic_styles.get(role).copied().unwrap_or_else(|| {
        TextStyle::plain().foreground(theme.inner.resolve_color(role).unwrap_or_default())
    })
}

/// Interpret pre-profile Pi adapter snapshots as they were before theme data
/// could express these policies. Explicit new fields always take precedence;
/// this deliberately does not apply the new profile or change old padding.
pub(super) fn apply_legacy_pi_presentation(
    theme: &mut OctetTheme,
    specs: &BTreeMap<String, RoleStyleSpec>,
) {
    if !theme.surfaces.contains_key("activity") {
        let mut activity = theme.surfaces["reasoning"].clone();
        if activity.marker_gutter.is_none() {
            activity.marker_gutter = Some(true);
        }
        theme.surfaces.insert("activity".into(), activity);
    }
    for (kind, surface) in &mut theme.surfaces {
        if kind != "activity" && surface.marker_gutter.is_none() {
            surface.marker_gutter = Some(false);
        }
    }
    let user = theme
        .surfaces
        .get_mut("user")
        .expect("built-in user surface");
    if user.prompt_marker.is_none() {
        user.prompt_marker = Some(false);
    }
    if !theme.has_semantic_style("tool_header") {
        let style = TranscriptRole::ToolHeader.default_style(theme);
        theme.semantic_styles.insert("tool_header".into(), style);
    }
    for kind in ["tool", "shell"] {
        for (state, token) in [
            ("pending", "toolPendingBg"),
            ("success", "toolSuccessBg"),
            ("error", "toolErrorBg"),
        ] {
            let role = format!("surface.{kind}.{state}");
            if let Some(legacy) = theme
                .semantic_styles
                .get(&format!("extension.pi.{token}"))
                .copied()
            {
                match specs.get(&role) {
                    None => {
                        theme.semantic_styles.insert(role, legacy);
                    }
                    Some(spec) if spec.background.is_none() => {
                        // A partial new override should retain the old Pi state
                        // fill unless it explicitly chooses another background.
                        if let Some(style) = theme.semantic_styles.get_mut(&role) {
                            style.background = legacy.background;
                        }
                    }
                    Some(_) => {}
                }
            }
        }
    }
}
