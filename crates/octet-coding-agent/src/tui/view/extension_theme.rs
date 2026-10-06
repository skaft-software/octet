//! Callback-free palette access on the foreground input owner, never the renderer.
use super::*;
use crate::tui::theme::{
    compiled_file_theme_names, compiled_theme_for_selector, selectable_file_themes,
    TerminalBackground,
};
use serde_json::{Map, Value};
use sexy_tui_rs::Color;

const FOREGROUNDS: &[(&str, &str)] = &[
    ("accent", "model_accent"),
    ("border", "border"),
    ("borderAccent", "border_focused"),
    ("borderMuted", "border_idle"),
    ("success", "success"),
    ("error", "error"),
    ("warning", "warning"),
    ("muted", "muted"),
    ("dim", "dim"),
    ("text", "text"),
    ("thinkingText", "reasoning_text"),
    ("scrollbarTrack", "muted"),
    ("scrollbarThumb", "text"),
    ("searchMatchText", "text"),
    ("userMessageText", "user_msg_text"),
    ("customMessageText", "text"),
    ("customMessageLabel", "model_accent"),
    ("toolTitle", "tool_title"),
    ("toolOutput", "tool_output"),
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
    ("thinkingOff", "dim"),
    ("thinkingMinimal", "muted"),
    ("thinkingLow", "model_accent"),
    ("thinkingMedium", "model_accent"),
    ("thinkingHigh", "model_accent"),
    ("thinkingXhigh", "model_accent"),
    ("thinkingMax", "model_accent"),
    ("bashMode", "model_accent"),
];
const BACKGROUNDS: &[(&str, &str, &str)] = &[
    ("selectedBg", "selected", "selected_bg"),
    ("searchMatchBg", "selected", "selected_bg"),
    ("userMessageBg", "surface.user", "user_msg_bg"),
    ("customMessageBg", "surface.assistant", "assistant_msg_bg"),
    ("toolPendingBg", "surface.tool", "tool_pending_bg"),
    ("toolSuccessBg", "surface.tool", "tool_success_bg"),
    ("toolErrorBg", "surface.tool", "tool_error_bg"),
];
fn color(color: Color) -> Value {
    match color {
        Color::Default => Value::String(String::new()),
        Color::Rgb(r, g, b) => Value::String(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Ansi16(index) | Color::Indexed(index) => Value::from(index),
    }
}
fn palette(theme: &OctetTheme, selector: &str) -> Value {
    let enabled = theme.capabilities().color != crate::tui::terminal::ColorDepth::None;
    let mut foregrounds = Map::new();
    let mut backgrounds = Map::new();
    let mut colors = Map::new();
    for &(token, role) in FOREGROUNDS {
        let style = theme.semantic_style(role);
        let resolved = color(if enabled {
            style.foreground
        } else {
            Color::Default
        });
        colors.insert(token.into(), resolved.clone());
        foregrounds.insert(
            token.into(),
            serde_json::json!({"color": resolved, "dim": enabled && style.attributes.dim}),
        );
    }
    for &(token, role, fallback) in BACKGROUNDS {
        let background = theme.semantic_style(role).background;
        backgrounds.insert(
            token.into(),
            color(if !enabled {
                Color::Default
            } else if background == Color::Default {
                theme.semantic_style(fallback).foreground
            } else {
                background
            }),
        );
    }
    colors.extend(backgrounds.clone());
    serde_json::json!({
        "name": selector,
        "appearance": if theme.background() == TerminalBackground::Light { "light" } else { "dark" },
        "colors": colors, "foregrounds": foregrounds, "backgrounds": backgrounds,
        "capabilities": { "color": match theme.capabilities().color {
            crate::tui::terminal::ColorDepth::TrueColor => "truecolor",
            crate::tui::terminal::ColorDepth::Ansi256 => "256color",
            crate::tui::terminal::ColorDepth::Ansi16 => "16color",
            crate::tui::terminal::ColorDepth::None => "none",
        }, "bold": enabled, "dim": enabled, "italic": enabled && theme.capabilities().italics,
            "underline": enabled, "inverse": enabled, "strikethrough": enabled },
        "path": theme.source_path().map(|path| path.to_string_lossy().into_owned()),
    })
}
impl InteractiveShell {
    pub(super) fn preload_extension_themes(&self) {
        let current = self.theme();
        let mut themes = Vec::new();
        for name in ["auto", "light", "dark"]
            .into_iter()
            .chain(compiled_file_theme_names())
        {
            if let Some(Ok(theme)) =
                compiled_theme_for_selector(name, current.capabilities(), current.background())
            {
                themes.push((name.to_owned(), theme));
            }
        }
        if let Some(config) = self.runtime_config() {
            themes.extend(selectable_file_themes(config, current.background()));
        }
        *self.extension_themes.borrow_mut() = Some(themes);
    }
    fn selected_theme_name(&self) -> String {
        self.runtime_config()
            .and_then(|config| config.theme.clone())
            .unwrap_or_else(|| match self.theme().source() {
                crate::tui::theme::ThemeSource::CompiledCards => "Cards".into(),
                crate::tui::theme::ThemeSource::CompiledStill => "Still".into(),
                crate::tui::theme::ThemeSource::File(path) => path
                    .file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "auto".into()),
                crate::tui::theme::ThemeSource::CompiledDefault => "auto".into(),
            })
    }
    pub(crate) fn extension_theme_get(&self, name: Option<&str>) -> Option<Value> {
        let Some(name) = name else {
            return Some(palette(&self.theme(), &self.selected_theme_name()));
        };
        if self.extension_themes.borrow().is_none() {
            self.preload_extension_themes();
        }
        let themes = self.extension_themes.borrow();
        let (selector, theme) = themes
            .as_ref()?
            .iter()
            .find(|(selector, _)| selector.eq_ignore_ascii_case(name))?;
        let mut theme = theme.clone();
        if let Some(lab) = self.state.borrow().model_lab {
            crate::tui::theme::apply_model_lab(&mut theme, lab);
        }
        Some(palette(&theme, selector))
    }
    /// Catalog of selectable native palettes, in picker order.
    ///
    /// Records carry metadata only: an extension asks for the one palette it
    /// needs with `theme_get`, so a listing can neither become a palette
    /// channel nor exceed its metadata bound.
    pub(crate) fn extension_theme_list(&self) -> Vec<Value> {
        if self.extension_themes.borrow().is_none() {
            self.preload_extension_themes();
        }
        let themes = self.extension_themes.borrow();
        themes
            .as_ref()
            .unwrap()
            .iter()
            .map(|(name, theme)| {
                serde_json::json!({
                    "name": name,
                    "path": theme
                        .source_path()
                        .map(|path| path.to_string_lossy().into_owned()),
                })
            })
            .collect()
    }
    pub(crate) fn extension_theme_set(&mut self, name: &str) -> Result<Value, String> {
        if self.extension_themes.borrow().is_none() {
            self.preload_extension_themes();
        }
        let (selector, theme) = self
            .extension_themes
            .borrow()
            .as_ref()
            .unwrap()
            .iter()
            .find(|(selector, _)| selector.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| {
                format!("theme {name:?} is unavailable in the trusted native catalog")
            })?;
        self.restore_theme_preview();
        self.set_theme(theme);
        if let Some(config) = self.runtime_config.as_mut() {
            config.theme = Some(selector.clone());
            config.theme_explicit = true;
        }
        // Selection is committed before the host publishes its synchronous ACK.
        self.render();
        Ok(palette(&self.theme(), &selector))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_palette_lookup_is_cached_and_never_selects_or_changes_the_draft() {
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("kept draft".into());
        let original = shell.extension_theme_get(None).unwrap();
        let cards = shell.extension_theme_get(Some("Cards")).unwrap();
        assert_eq!(cards["name"], "Cards");
        assert!(cards["foregrounds"]["text"]["dim"].is_boolean());
        assert_eq!(
            cards["foregrounds"]["text"]["color"],
            cards["colors"]["text"]
        );
        assert_eq!(shell.extension_theme_get(None).unwrap(), original);
        assert!(shell.extension_theme_get(Some("missing-name")).is_none());
        assert!(shell.extension_theme_set("missing-name").is_err());
        assert_eq!(shell.extension_theme_get(None).unwrap(), original);
        let selected = shell.extension_theme_set("Cards").unwrap();
        assert_eq!(selected, shell.extension_theme_get(None).unwrap());
        assert_eq!(shell.pending(), "kept draft");
        assert!(shell
            .extension_theme_list()
            .iter()
            .any(|theme| theme["name"] == "Cards"));
    }
}
