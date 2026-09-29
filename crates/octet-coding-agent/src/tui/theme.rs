#![allow(missing_docs)]

use std::collections::BTreeMap;
#[cfg(any(test, feature = "serve"))]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::{Duration, Instant};

use octet_ai::{Model, ModelSpec};
use sexy_tui_rs::theme::{capability::CapabilityTier, Theme as SexyTheme};
use sexy_tui_rs::{
    CapabilityOverrides, CodeOverflow, Color, RenderOptions, RichRenderer, SupportLevel, TextRole,
    TextStyle, UnorderedListMarker,
};

use crate::config::{ColorMode, Config};
use crate::resource_resolver::{ResourceKind, ResourceResolver};
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
#[cfg(test)]
use crate::tui::theme_reload::{
    ReloadBoundary, ReloadDecision, ReloadFailureKind, ThemeChangeReceiver, ThemeChangeSender,
    ThemePathError, ThemeReloadEngine, ThemeReloadMode, ThemeWatch,
};
use crate::tui::theme_schema::{self, ParsedTheme, RoleStyleSpec, ThemeSurface};

#[allow(unused_imports)]
pub use crate::tui::theme_schema::{
    ResolvedThemeLayout, ResolvedThemeSurface, ThemeDensity, ThemeLayout, ThemeMetadata,
    ThemeSurfaceAlign, ThemeSurfaceChrome, ThemeSurfaceHeading, ThemeSurfaceWidth, MAX_THEME_BYTES,
};

/// Stable name for octet's compiled-in default theme and legacy selectors.
pub const DEFAULT_THEME_NAME: &str = "default";

/// Stable selector for the compiled-in `Cards` theme. The file is embedded at
/// build time from `examples/themes/Cards.toml` and validated by
/// `cards_example_theme_is_valid_for_every_background_profile`, so the shipped
/// example and this built-in can never drift apart.
pub const CARDS_THEME_NAME: &str = "Cards";

/// The embedded source for [`CARDS_THEME_NAME`].
const CARDS_THEME_SOURCE: &str = include_str!("../../../../examples/themes/Cards.toml");

/// Stable selector for the compiled-in `Still` theme. The file is embedded at
/// build time from `examples/themes/Still.toml` and validated by
/// `still_example_theme_is_valid_for_every_background_profile`, so the shipped
/// example and this built-in can never drift apart.
pub const STILL_THEME_NAME: &str = "Still";

/// The embedded source for [`STILL_THEME_NAME`].
const STILL_THEME_SOURCE: &str = include_str!("../../../../examples/themes/Still.toml");

/// Compiles one embedded file theme for a single terminal profile.
type CompiledFileTheme = fn(TerminalCapabilities, TerminalBackground) -> anyhow::Result<OctetTheme>;

/// Every selector answered by a compiled-in file theme rather than a discovered
/// file, in the order they are offered in the `/theme` picker. A built-in
/// selector always wins over a discovered file of the same stem, and the stem is
/// reserved so a user's `Cards.toml` or `Still.toml` can neither shadow, nor be
/// shadowed by, the built-in it selects.
const COMPILED_FILE_THEMES: &[(&str, CompiledFileTheme)] = &[
    (CARDS_THEME_NAME, cards_theme_for),
    (STILL_THEME_NAME, still_theme_for),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeSource {
    CompiledDefault,
    /// The compiled-in `Cards` theme, embedded from `examples/themes/Cards.toml`.
    CompiledCards,
    /// The compiled-in `Still` theme, embedded from `examples/themes/Still.toml`.
    CompiledStill,
    File(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub struct ThemeSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: ThemeSource,
}

// Artificial Analysis (https://artificialanalysis.ai/) uses stable creator
// colors in comparison charts. Keep those source colors here, then rebalance
// their luminance for the terminal background instead of using web colors
// verbatim: OpenAI's near-black and DeepSeek's dark blue, for example, are
// unreadable on many dark terminal profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelLab {
    OpenAi,
    Anthropic,
    Google,
    XAi,
    Meta,
    Mistral,
    DeepSeek,
    Alibaba,
    MiniMax,
    Kimi,
    ZAi,
    Nvidia,
    Xiaomi,
    Cohere,
    Amazon,
    Microsoft,
    Ai21,
    ByteDance,
    Perplexity,
    Ibm,
    Baidu,
    Tencent,
    AllenAi,
    Unknown,
}

impl ModelLab {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::XAi => "xai",
            Self::Meta => "meta",
            Self::Mistral => "mistral",
            Self::DeepSeek => "deepseek",
            Self::Alibaba => "alibaba",
            Self::MiniMax => "minimax",
            Self::Kimi => "kimi",
            Self::ZAi => "zai",
            Self::Nvidia => "nvidia",
            Self::Xiaomi => "xiaomi",
            Self::Cohere => "cohere",
            Self::Amazon => "amazon",
            Self::Microsoft => "microsoft",
            Self::Ai21 => "ai21",
            Self::ByteDance => "bytedance",
            Self::Perplexity => "perplexity",
            Self::Ibm => "ibm",
            Self::Baidu => "baidu",
            Self::Tencent => "tencent",
            Self::AllenAi => "allenai",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Some(match key.trim().to_ascii_lowercase().as_str() {
            "openai" => Self::OpenAi,
            "anthropic" => Self::Anthropic,
            "google" => Self::Google,
            "xai" => Self::XAi,
            "meta" => Self::Meta,
            "mistral" => Self::Mistral,
            "deepseek" => Self::DeepSeek,
            "alibaba" => Self::Alibaba,
            "minimax" => Self::MiniMax,
            "kimi" => Self::Kimi,
            "zai" => Self::ZAi,
            "nvidia" => Self::Nvidia,
            "xiaomi" => Self::Xiaomi,
            "cohere" => Self::Cohere,
            "amazon" => Self::Amazon,
            "microsoft" => Self::Microsoft,
            "ai21" => Self::Ai21,
            "bytedance" => Self::ByteDance,
            "perplexity" => Self::Perplexity,
            "ibm" => Self::Ibm,
            "baidu" => Self::Baidu,
            "tencent" => Self::Tencent,
            "allenai" => Self::AllenAi,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    pub(crate) fn source_color(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("#1f1f1f"),
            Self::Anthropic => Some("#cc785c"),
            Self::Google => Some("#34a853"),
            Self::XAi => Some("#736cd3"),
            Self::Meta => Some("#0089f4"),
            Self::Mistral => Some("#fd6f00"),
            Self::DeepSeek => Some("#2243e6"),
            Self::Alibaba => Some("#ff7018"),
            Self::MiniMax => Some("#eb3568"),
            Self::Kimi => Some("#047afe"),
            Self::ZAi => Some("#1c7ff8"),
            Self::Nvidia => Some("#86b737"),
            Self::Xiaomi => Some("#ff6900"),
            Self::Cohere => Some("#d18ee2"),
            Self::Amazon => Some("#ff9900"),
            Self::Microsoft => Some("#0078d5"),
            Self::Ai21 => Some("#d63864"),
            Self::ByteDance => Some("#3c8bff"),
            Self::Perplexity => Some("#1b818e"),
            Self::Ibm => Some("#0f62fe"),
            Self::Baidu => Some("#2436d8"),
            Self::Tencent => Some("#5cb9ff"),
            Self::AllenAi => Some("#f0529c"),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalBackground {
    Dark,
    Light,
    Unknown,
}

/// The activity shimmer implementation used by the renderer.
///
/// Physical mode is the default for terminals with a known background and
/// TrueColor/ANSI256 output. Classic remains available for A/B comparisons and
/// is also the safe fallback for unknown backgrounds and ANSI16 terminals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShimmerMode {
    Classic,
    Physical,
}

impl ShimmerMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "classic" => Some(Self::Classic),
            "physical" => Some(Self::Physical),
            _ => None,
        }
    }

    fn from_environment() -> Self {
        std::env::var("OCTET_SHIMMER")
            .ok()
            .and_then(|value| Self::parse(&value))
            .unwrap_or(Self::Physical)
    }
}

/// The three terminal-appearance choices exposed by the interactive TUI.
/// These are selectors for the compiled theme, not filesystem theme names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalThemeChoice {
    Auto,
    Light,
    Dark,
}

impl TerminalThemeChoice {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    pub(crate) fn from_config(config: &Config) -> Option<Self> {
        config.theme.as_deref().and_then(Self::parse)
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (recommended)",
            Self::Light => "Light terminal",
            Self::Dark => "Dark terminal",
        }
    }

    pub(crate) fn explicit_background(self) -> Option<TerminalBackground> {
        match self {
            Self::Auto => None,
            Self::Light => Some(TerminalBackground::Light),
            Self::Dark => Some(TerminalBackground::Dark),
        }
    }

    #[cfg(test)]
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Auto => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }

    pub(crate) fn all() -> [Self; 3] {
        [Self::Auto, Self::Light, Self::Dark]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb {
    red: u8,
    green: u8,
    blue: u8,
}

/// octet-side styling boundary around sexy-tui's semantic token store. octet owns
/// model-family palette selection and contrast balancing; sexy-tui owns rich
/// text layout, sanitization, syntax highlighting, and semantic encoding.
#[derive(Clone, Debug)]
pub struct OctetTheme {
    inner: SexyTheme,
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
    shimmer: ShimmerMode,
    semantic_styles: BTreeMap<String, TextStyle>,
    glyphs: BTreeMap<String, String>,
    ascii_glyphs: BTreeMap<String, String>,
    surfaces: BTreeMap<String, ThemeSurface>,
    layout: ThemeLayout,
    metadata: ThemeMetadata,
    source: ThemeSource,
}

/// Semantic roles rendered as thinking prose. Code, diff, and syntax roles
/// deliberately stay out of this list so technical output remains upright,
/// crisp, and more prominent than the surrounding stream.
const REASONING_PROSE_ROLES: &[TextRole] = &[
    TextRole::Text,
    TextRole::Accent,
    TextRole::Heading,
    TextRole::Emphasis,
    TextRole::Strong,
    TextRole::Quote,
    TextRole::Link,
    TextRole::ListMarker,
];

fn unicode_glyph(name: &str) -> &'static str {
    match name {
        "top_left" => "╭",
        "top_right" => "╮",
        "bottom_left" => "╰",
        "bottom_right" => "╯",
        "horizontal" => "─",
        "vertical" | "branch" | "rail" => "│",
        "last_branch" => "└",
        "prompt" => "›",
        "shell" => "$",
        "success" => "✓",
        "warning" | "note" => "◇",
        "error" => "×",
        "interrupt" => "■",
        "pending" | "reasoning" => "·",
        "collapsed" => "▸",
        "expanded" => "▾",
        "separator" => " · ",
        "ellipsis" => "…",
        "bullet" => "•",
        "wordmark" => "octet",
        _ => "*",
    }
}

fn ascii_glyph(name: &str) -> &'static str {
    match name {
        "top_left" | "top_right" | "bottom_left" | "bottom_right" => "+",
        "horizontal" => "-",
        "vertical" | "rail" => "|",
        "branch" => "|-",
        "last_branch" => "`-",
        "prompt" => ">",
        "shell" => "$",
        "success" => "+",
        "warning" | "interrupt" => "!",
        "note" => "*",
        "error" => "x",
        "pending" | "reasoning" => ".",
        "collapsed" => "[+]",
        "expanded" => "[-]",
        "separator" => " - ",
        "ellipsis" => "...",
        "bullet" => "*",
        "wordmark" => "octet",
        _ => "*",
    }
}

fn default_glyphs() -> BTreeMap<String, String> {
    [
        "top_left",
        "top_right",
        "bottom_left",
        "bottom_right",
        "horizontal",
        "vertical",
        "branch",
        "last_branch",
        "rail",
        "prompt",
        "shell",
        "success",
        "warning",
        "note",
        "error",
        "interrupt",
        "pending",
        "reasoning",
        "collapsed",
        "expanded",
        "separator",
        "ellipsis",
        "bullet",
        "wordmark",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), unicode_glyph(name).to_owned()))
    .collect()
}

fn default_ascii_glyphs() -> BTreeMap<String, String> {
    default_glyphs()
        .into_keys()
        .map(|name| {
            let glyph = ascii_glyph(&name).to_owned();
            (name, glyph)
        })
        .collect()
}

fn default_surfaces() -> BTreeMap<String, ThemeSurface> {
    [
        "user",
        "assistant",
        "reasoning",
        "tool",
        "notice",
        "outcome",
        "shell",
        "compaction",
    ]
    .into_iter()
    .map(|kind| (kind.to_owned(), ThemeSurface::default()))
    .collect()
}

/// Published semantic role vocabulary (roadmap #419).
///
/// These are the terminal-independent role names that the theme-file `[roles]`
/// table maps onto octet's semantic text roles. `docs/themes.md` publishes the
/// same list; the `published_semantic_role_vocabulary_is_closed_and_accepted`
/// test keeps the two in sync. Extensions contribute additional
/// `extension.<namespace>.<role>` roles, which the schema accepts as open but
/// typed names.
#[cfg(test)]
pub const SEMANTIC_ROLE_VOCABULARY: &[&str] = &[
    "text",
    "foreground",
    "muted",
    "subtle",
    "dim",
    "accent",
    "success",
    "warning",
    "error",
    "heading",
    "md_heading",
    "emphasis",
    "md_emphasis",
    "strong",
    "md_strong",
    "inline_code",
    "md_code",
    "code",
    "md_code_block",
    "quote",
    "md_quote",
    "border",
    "link",
    "md_link",
    "list_marker",
    "md_list_bullet",
    "diff_add",
    "diff_added",
    "diff_remove",
    "diff_removed",
    "diff_context",
    "diff_hunk",
    "diff_header",
    "syntax_comment",
    "syntax_keyword",
    "syntax_function",
    "syntax_variable",
    "syntax_string",
    "syntax_number",
    "syntax_type",
    "syntax_operator",
    "syntax_punctuation",
];

fn semantic_text_role(name: &str) -> Option<(TextRole, &'static str)> {
    Some(match name {
        "text" | "foreground" => (TextRole::Text, "foreground"),
        "muted" => (TextRole::Muted, "muted"),
        "subtle" | "dim" => (TextRole::Subtle, "dim"),
        "accent" => (TextRole::Accent, "accent"),
        "success" => (TextRole::Success, "success"),
        "warning" => (TextRole::Warning, "warning"),
        "error" => (TextRole::Error, "error"),
        "heading" | "md_heading" => (TextRole::Heading, "md_heading"),
        "emphasis" | "md_emphasis" => (TextRole::Emphasis, "md_emphasis"),
        "strong" | "md_strong" => (TextRole::Strong, "md_strong"),
        "inline_code" | "md_code" => (TextRole::InlineCode, "md_code"),
        "code" | "md_code_block" => (TextRole::Code, "md_code_block"),
        "quote" | "md_quote" => (TextRole::Quote, "md_quote"),
        "border" => (TextRole::Border, "border"),
        "link" | "md_link" => (TextRole::Link, "md_link"),
        "list_marker" | "md_list_bullet" => (TextRole::ListMarker, "md_list_bullet"),
        "diff_add" | "diff_added" => (TextRole::DiffAdd, "diff_added"),
        "diff_remove" | "diff_removed" => (TextRole::DiffRemove, "diff_removed"),
        "diff_context" => (TextRole::DiffContext, "diff_context"),
        "diff_hunk" => (TextRole::DiffHunk, "diff_hunk"),
        "diff_header" => (TextRole::DiffHeader, "diff_header"),
        "syntax_comment" => (TextRole::SyntaxComment, "syntax_comment"),
        "syntax_keyword" => (TextRole::SyntaxKeyword, "syntax_keyword"),
        "syntax_function" => (TextRole::SyntaxFunction, "syntax_function"),
        "syntax_variable" => (TextRole::SyntaxVariable, "syntax_variable"),
        "syntax_string" => (TextRole::SyntaxString, "syntax_string"),
        "syntax_number" => (TextRole::SyntaxNumber, "syntax_number"),
        "syntax_type" => (TextRole::SyntaxType, "syntax_type"),
        "syntax_operator" => (TextRole::SyntaxOperator, "syntax_operator"),
        "syntax_punctuation" => (TextRole::SyntaxPunctuation, "syntax_punctuation"),
        _ => return None,
    })
}

impl Default for OctetTheme {
    fn default() -> Self {
        default_theme()
    }
}

impl OctetTheme {
    fn new(
        mut inner: SexyTheme,
        capabilities: TerminalCapabilities,
        background: TerminalBackground,
    ) -> Self {
        inner.set_capabilities(rich_capabilities(capabilities));
        for &(token, source) in CONTEXT_COLOR_DEFAULTS {
            let value = if source.starts_with('#') {
                balance_foreground(source, background)
            } else {
                source.to_owned()
            };
            inner.override_token(token, &value);
        }
        Self {
            inner,
            capabilities,
            background,
            shimmer: ShimmerMode::from_environment(),
            semantic_styles: BTreeMap::new(),
            glyphs: default_glyphs(),
            ascii_glyphs: default_ascii_glyphs(),
            surfaces: default_surfaces(),
            layout: ThemeLayout::default(),
            metadata: ThemeMetadata {
                name: "octet Default".to_owned(),
                description: "Terminal-neutral compiled theme".to_owned(),
                author: "octet".to_owned(),
                ..ThemeMetadata::default()
            },
            source: ThemeSource::CompiledDefault,
        }
    }

    pub fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities
    }

    pub fn unicode(&self) -> bool {
        self.capabilities.unicode
    }

    #[allow(dead_code)]
    pub fn metadata(&self) -> &ThemeMetadata {
        &self.metadata
    }

    pub(crate) fn is_compiled_default(&self) -> bool {
        matches!(self.source, ThemeSource::CompiledDefault)
    }

    #[allow(dead_code)]
    pub fn source(&self) -> &ThemeSource {
        &self.source
    }

    #[allow(dead_code)]
    pub fn source_path(&self) -> Option<&Path> {
        match &self.source {
            ThemeSource::File(path) => Some(path),
            ThemeSource::CompiledDefault
            | ThemeSource::CompiledCards
            | ThemeSource::CompiledStill => None,
        }
    }

    #[allow(dead_code)]
    pub fn layout(&self) -> &ThemeLayout {
        &self.layout
    }

    pub fn layout_for_width(&self, width: u16) -> ResolvedThemeLayout {
        self.layout.resolve(width)
    }

    pub fn surface_for_width(&self, kind: &str, width: u16) -> ResolvedThemeSurface<'_> {
        let narrow = self.layout_for_width(width).narrow;
        self.surfaces
            .get(kind)
            .expect("built-in transcript surface kind")
            .resolve(narrow)
    }

    #[allow(dead_code)]
    pub(crate) fn background(&self) -> TerminalBackground {
        self.background
    }

    pub(crate) fn shimmer_mode(&self) -> ShimmerMode {
        self.shimmer
    }

    /// Return a theme glyph with deterministic ASCII fallback. Theme files can
    /// change semantic marks, but cannot force Unicode into a conservative
    /// terminal profile.
    pub fn glyph<'a>(&'a self, name: &str) -> &'a str {
        if !self.unicode() {
            return self
                .ascii_glyphs
                .get(name)
                .map(String::as_str)
                .unwrap_or_else(|| ascii_glyph(name));
        }
        self.glyphs
            .get(name)
            .map(String::as_str)
            .unwrap_or_else(|| unicode_glyph(name))
    }

    #[allow(dead_code)] // used only with the `serve` extension feature
    pub fn semantic_role_names(&self) -> impl Iterator<Item = &str> {
        self.semantic_styles.keys().map(String::as_str)
    }

    pub fn semantic_style(&self, role: &str) -> TextStyle {
        self.semantic_styles
            .get(role)
            .copied()
            .or_else(|| semantic_text_role(role).map(|(role, _)| self.inner.style(role)))
            .unwrap_or_else(|| {
                TextStyle::plain()
                    .foreground(self.inner.resolve_color(role).unwrap_or(Color::Default))
            })
    }

    /// Render a built-in or extension-defined semantic role. Extensions use
    /// stable role names and never need access to octet's private application
    /// state or raw terminal escape sequences.
    pub fn apply_semantic_role(&self, role: &str, text: &str) -> String {
        self.inner.apply_style(self.semantic_style(role), text)
    }

    /// Apply an outer semantic layer while preserving it across trusted ANSI
    /// runs produced by the rich renderer. Theme data remains typed; only the
    /// renderer creates these reset/reopen sequences.
    pub(crate) fn apply_semantic_role_layered(&self, role: &str, text: &str) -> String {
        self.apply_style_layered(self.semantic_style(role), text)
    }

    /// Paint a full row with a background colour while preserving inner ANSI
    /// runs (the shaded composer rectangle). No-colour terminals get the text
    /// back unchanged.
    pub(crate) fn paint_row_background(&self, rgb: (u8, u8, u8), text: &str) -> String {
        self.apply_style_layered(
            TextStyle::plain().background(Color::Rgb(rgb.0, rgb.1, rgb.2)),
            text,
        )
    }

    /// Search decoration uses the active theme accent and capability downgrade.
    /// Current and ordinary matches differ in attributes, not raw ANSI colours.
    pub(crate) fn transcript_search_match(&self, text: &str, current: bool) -> String {
        let mut style = self.semantic_style("accent").underline();
        style.attributes.bold = current;
        style.attributes.inverse = current;
        self.inner.apply_style(style, text)
    }

    fn apply_style_layered(&self, style: TextStyle, text: &str) -> String {
        let wrapped_empty = self.inner.apply_style(style, "");
        let Some(opening) = wrapped_empty.strip_suffix("\x1b[0m") else {
            return text.to_owned();
        };
        if opening.is_empty() {
            return text.to_owned();
        }

        let mut layered = String::with_capacity(text.len().saturating_add(opening.len() * 2 + 4));
        layered.push_str(opening);
        let mut rest = text;
        while let Some(index) = rest.find("\x1b[0m") {
            let (before, after) = rest.split_at(index + 4);
            layered.push_str(before);
            layered.push_str(opening);
            rest = after;
        }
        layered.push_str(rest);
        layered.push_str("\x1b[0m");
        layered
    }

    /// Reload the active file theme while preserving this terminal
    /// capability/background profile. Runtime model styling is reapplied by
    /// the shell when it swaps the returned theme in.
    #[allow(dead_code)]
    pub fn reload(&self) -> anyhow::Result<Self> {
        match &self.source {
            ThemeSource::CompiledDefault => {
                Ok(default_theme_for(self.background, self.capabilities))
            }
            ThemeSource::CompiledCards => cards_theme_for(self.capabilities, self.background),
            ThemeSource::CompiledStill => still_theme_for(self.capabilities, self.background),
            ThemeSource::File(path) => {
                load_theme_path_for(path, self.capabilities, self.background)
            }
        }
    }

    pub fn fg(&self, token: &str, text: &str) -> String {
        if let Some(style) = self.semantic_styles.get(token) {
            return self.inner.apply_style(*style, text);
        }
        let Some(color) = self.resolve_rgb(token) else {
            return text.to_owned();
        };
        self.color_text(color, text)
    }

    /// Resolve the accent colour of a specific model family. `None` uses the
    /// active theme token; a concrete lab remains stable across model switches.
    pub(crate) fn model_rgb(&self, lab: Option<ModelLab>) -> Option<(u8, u8, u8)> {
        let Some(lab) = lab else {
            return self.role_rgb("model_accent");
        };
        let configured_key = format!("model.{}", lab.key());
        let configured = self
            .resolve::<String>(&configured_key)
            .filter(|color| parse_hex_color(color).is_some());
        let use_lab_color =
            configured.is_some() || self.resolve::<bool>("model.use_lab_color").unwrap_or(false);
        let source = configured
            .or_else(|| {
                if use_lab_color {
                    lab.source_color().map(str::to_owned)
                } else {
                    None
                }
            })
            .or_else(|| {
                self.resolve::<String>("accent")
                    .filter(|color| parse_hex_color(color).is_some())
            })
            .unwrap_or_else(|| DEFAULT_ACCENT.to_owned());
        let color = parse_hex_color(&balance_foreground(&source, self.background))?;
        Some((color.red, color.green, color.blue))
    }

    /// Whether this theme opts into model-adaptive lab coloring. The default
    /// sets `model.use_lab_color = true`; custom themes can disable it when a
    /// fixed palette is part of their design.
    pub(crate) fn uses_model_lab_color(&self) -> bool {
        self.resolve::<bool>("model.use_lab_color").unwrap_or(true)
    }

    /// Render `text` in the accent colour of a specific model family.
    /// When `lab` is `None` or has no source colour the global `model_accent`
    /// token is used instead.
    pub fn model_fg(&self, lab: Option<ModelLab>, text: &str) -> String {
        let Some((red, green, blue)) = self.model_rgb(lab) else {
            return text.to_owned();
        };
        self.color_text(Rgb { red, green, blue }, text)
    }

    /// Render the small provenance marker of a historical prompt from the exact
    /// stored source colour. The source remains durable; display contrast is
    /// adapted to the current terminal.
    pub(crate) fn prompt_color_marker(&self, color: Option<&str>, text: &str) -> String {
        let Some(source) = color.filter(|source| parse_hex_color(source).is_some()) else {
            return text.to_owned();
        };
        let Some(color) = parse_hex_color(&balance_foreground(source, self.background)) else {
            return text.to_owned();
        };
        self.color_text(color, text)
    }

    /// Render a historical prompt cell using the exact model colour stored
    /// with that turn and a readable foreground chosen for that colour.
    pub(crate) fn prompt_color_cell(&self, color: Option<&str>, text: &str) -> String {
        let Some(color) = color.and_then(parse_hex_color) else {
            return text.to_owned();
        };
        let luminance =
            u32::from(color.red) * 299 + u32::from(color.green) * 587 + u32::from(color.blue) * 114;
        let foreground = if luminance >= 150_000 {
            Color::Rgb(0, 0, 0)
        } else {
            Color::Rgb(255, 255, 255)
        };
        self.inner.apply_style(
            TextStyle::plain()
                .foreground(foreground)
                .background(Color::Rgb(color.red, color.green, color.blue)),
            text,
        )
    }

    /// Whether prompt rows are painted with each turn's stored model colour as
    /// a full-cell provenance card. The default theme opts in explicitly; a
    /// theme that sets `prompt_wash = false` keeps prompt rows on the surface's
    /// own fill while the chevron retains its prompt colour.
    pub(crate) fn prompt_wash(&self) -> bool {
        self.resolve::<bool>("prompt_wash").unwrap_or(true)
    }

    /// Paint one stored prompt colour as a full-cell provenance card. The
    /// background covers every cell of the row — padding, blank spacing, and
    /// trailing canvas included — and the rich renderer's own inline runs are
    /// layered inside it, so Markdown emphasis, links, and inline code keep
    /// their styling. Unknown backgrounds and no-colour terminals get the text
    /// back unpainted; limited palettes use the existing contrast-tested
    /// surface treatment.
    pub(crate) fn prompt_provenance_card(&self, color: Option<&str>, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let Some(source) = color.and_then(parse_hex_color) else {
            return text.to_owned();
        };
        if self.capabilities.color == ColorDepth::None
            || self.background == TerminalBackground::Unknown
        {
            return text.to_owned();
        }
        if self.capabilities.color != ColorDepth::TrueColor {
            return self.prompt_color_cell(color, text);
        }
        let (background, foreground) = match self.background {
            TerminalBackground::Dark => (
                balance_to_luminance(source, 0.10),
                Rgb {
                    red: 0xe6,
                    green: 0xe6,
                    blue: 0xeb,
                },
            ),
            TerminalBackground::Light => (
                balance_to_luminance(source, 0.88),
                Rgb {
                    red: 0x20,
                    green: 0x23,
                    blue: 0x27,
                },
            ),
            TerminalBackground::Unknown => unreachable!("handled above"),
        };
        self.apply_style_layered(
            TextStyle::plain()
                .foreground(Color::Rgb(
                    foreground.red,
                    foreground.green,
                    foreground.blue,
                ))
                .background(Color::Rgb(
                    background.red,
                    background.green,
                    background.blue,
                )),
            text,
        )
    }

    pub(crate) fn role_rgb(&self, token: &str) -> Option<(u8, u8, u8)> {
        self.resolve_rgb(token)
            .map(|color| (color.red, color.green, color.blue))
    }

    /// Resting composer chrome is a background-adjacent form of the UI accent.
    /// Moving toward white on light profiles and black on dark ones keeps the
    /// outline quiet until the composer regains focus.
    pub(crate) fn composer_idle_rgb(&self, accent: (u8, u8, u8)) -> (u8, u8, u8) {
        let source = Rgb {
            red: accent.0,
            green: accent.1,
            blue: accent.2,
        };
        let destination = match self.background {
            TerminalBackground::Light => Rgb {
                red: 255,
                green: 255,
                blue: 255,
            },
            // Unknown profiles may be light, dark, or custom. Keep the idle
            // border near the readable midpoint instead of painting a dark
            // terminal assumption into a user's custom background.
            TerminalBackground::Dark => Rgb {
                red: 0,
                green: 0,
                blue: 0,
            },
            TerminalBackground::Unknown => Rgb {
                red: 128,
                green: 128,
                blue: 128,
            },
        };
        let idle = blend(source, destination, 0.88);
        (idle.red, idle.green, idle.blue)
    }

    pub(crate) fn rgb_fg(&self, color: (u8, u8, u8), text: &str) -> String {
        self.color_text(
            Rgb {
                red: color.0,
                green: color.1,
                blue: color.2,
            },
            text,
        )
    }

    fn color_text(&self, color: Rgb, text: &str) -> String {
        match self.capabilities.color {
            ColorDepth::None => text.to_owned(),
            ColorDepth::TrueColor => format!(
                "\x1b[38;2;{};{};{}m{text}\x1b[39m",
                color.red, color.green, color.blue
            ),
            ColorDepth::Ansi256 => {
                format!("\x1b[38;5;{}m{text}\x1b[39m", nearest_ansi256(color))
            }
            ColorDepth::Ansi16 => {
                format!("\x1b[{}m{text}\x1b[39m", nearest_ansi16_code(color))
            }
        }
    }

    pub fn bold(&self, text: &str) -> String {
        if self.capabilities.color == ColorDepth::None {
            text.to_owned()
        } else {
            format!("\x1b[1m{text}\x1b[22m")
        }
    }

    /// Render secondary text using a real muted foreground rather than SGR
    /// faint. Terminal implementations disagree about SGR 2 (some make text
    /// look brighter or thinner), while a palette colour is predictable.
    pub fn dim(&self, text: &str) -> String {
        self.fg("muted", text)
    }

    pub(crate) fn settled_event_dot(&self, tone: &str, text: &str) -> String {
        let source = match tone {
            // One-cell outcome markers need full-strength signal colours;
            // blending them toward the terminal background makes both states
            // look muted or nearly white at a glance.
            "success" => {
                return self.color_text(
                    Rgb {
                        red: 82,
                        green: 200,
                        blue: 116,
                    },
                    text,
                );
            }
            "error" => {
                return self.color_text(
                    Rgb {
                        red: 230,
                        green: 83,
                        blue: 83,
                    },
                    text,
                );
            }
            _ => self.resolve_rgb("muted").unwrap_or(Rgb {
                red: 119,
                green: 119,
                blue: 119,
            }),
        };
        let destination = match self.background {
            TerminalBackground::Light => Rgb {
                red: 255,
                green: 255,
                blue: 255,
            },
            TerminalBackground::Dark => Rgb {
                red: 0,
                green: 0,
                blue: 0,
            },
            TerminalBackground::Unknown => Rgb {
                red: 85,
                green: 85,
                blue: 85,
            },
        };
        let color = blend(source, destination, 0.62);
        self.color_text(color, text)
    }

    pub fn override_token(&mut self, key: &str, value: &str) {
        self.inner.override_token(key, value);
    }

    pub fn resolve<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.inner.resolve(key)
    }

    /// Build the persistent semantic renderer used by assistant transcript
    /// blocks. The renderer uses its own semantic colour roles (heading,
    /// code, link, syntax, etc.) so the model accent never bleeds into prose.
    pub fn rich_renderer(&self) -> RichRenderer {
        self.rich_renderer_with_inner(self.inner.clone())
    }

    /// Build the renderer used for model reasoning. Every semantic role keeps
    /// its own colour while prose receives a muted foreground, so the stream
    /// recedes behind the final response without changing font shape.
    pub fn reasoning_renderer(&self) -> RichRenderer {
        let mut theme = self.inner.clone();
        let reasoning_foreground = balance_foreground("#777777", self.background);
        let reasoning_foreground = parse_hex_color(&reasoning_foreground)
            .map(|color| Color::Rgb(color.red, color.green, color.blue));
        for role in REASONING_PROSE_ROLES {
            let mut style = theme.style(*role);
            if let Some(foreground) = reasoning_foreground {
                style.foreground = foreground;
            }
            // Do not use SGR faint here. A muted foreground gives the desired
            // hierarchy without changing the terminal's font weight/shape.
            style.attributes.dim = false;
            style.attributes.italic = false;
            theme.override_style(*role, style);
        }
        // Muted/subtle roles are used by code-frame labels and ellipses. Keep
        // those annotations quiet, but upright, so an entire code block never
        // inherits the thinking prose treatment.
        for role in [TextRole::Muted, TextRole::Subtle] {
            let mut style = theme.style(role);
            if let Some(foreground) = reasoning_foreground {
                style.foreground = foreground;
            }
            style.attributes.dim = false;
            style.attributes.italic = false;
            style.attributes.underline = false;
            theme.override_style(role, style);
        }
        self.rich_renderer_with_inner(theme)
    }

    fn rich_renderer_with_inner(&self, mut theme: SexyTheme) -> RichRenderer {
        let capabilities = rich_capabilities(self.capabilities);
        theme.set_capabilities(capabilities);
        RichRenderer::new(
            theme,
            capabilities,
            RenderOptions {
                // Transcript code has no horizontal viewport. Wrapping keeps
                // every model-emitted grapheme visible while sexy-tui retains
                // the original source as semantic copy text.
                code_overflow: CodeOverflow::Wrap,
                code_borders: !self.is_compiled_default(),
                syntax_highlighting: true,
                tables: true,
                stable_block_geometry: true,
                prose_width: None,
                unordered_list_marker: UnorderedListMarker::Dash,
                ..RenderOptions::default()
            },
        )
    }

    fn resolve_rgb(&self, token: &str) -> Option<Rgb> {
        let mut value = self.inner.resolve::<String>(token)?;
        for _ in 0..6 {
            if value.eq_ignore_ascii_case("default") || value.eq_ignore_ascii_case("none") {
                return None;
            }
            if let Some(color) = parse_hex_color(&value).or_else(|| named_color(&value)) {
                return Some(color);
            }
            let next = self.inner.resolve::<String>(&value)?;
            if next == value {
                return None;
            }
            value = next;
        }
        None
    }
}

fn rich_capabilities(capabilities: TerminalCapabilities) -> sexy_tui_rs::TerminalCapabilities {
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

fn named_color(value: &str) -> Option<Rgb> {
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

const ANSI16: [(Rgb, u8); 16] = [
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

fn nearest_ansi16_code(color: Rgb) -> u8 {
    ANSI16
        .iter()
        .min_by_key(|(candidate, _)| color_distance(color, *candidate))
        .map_or(37, |(_, code)| *code)
}

#[cfg(test)]
fn ansi256_rgb(index: u8) -> Rgb {
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

fn nearest_ansi256(color: Rgb) -> u8 {
    sexy_tui_rs::theme::palette::nearest_ansi256(color.red, color.green, color.blue)
}

const DEFAULT_ACCENT: &str = "#16876d";

// Context reports are a legend, not a status list. Keep each category on its
// own visual channel so adjacent slices remain distinguishable even when two
// categories happen to carry the same semantic status (for example free space
// and tool schemas both used to resolve to the terminal foreground).
//
// Hex values are balanced in `OctetTheme::new`; aliases continue to follow the
// active theme and can be overridden by a theme's `[colors]` table.
const CONTEXT_COLOR_DEFAULTS: &[(&str, &str)] = &[
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
const DARK_TARGET_LUMINANCE: f64 = 0.27;
const LIGHT_TARGET_LUMINANCE: f64 = 0.11;
// Symmetric midpoint: ~4.58:1 against both pure black and pure white.
// Light-terminal users can set OCTET_COLOR_SCHEME=light for a 0.11 target.
const UNIVERSAL_TARGET_LUMINANCE: f64 = 0.179;

// Tokens that receive terminal-background-aware luminance balancing.
// These are semantic UI signals (errors, warnings, model accent) whose
// source colours may be unreadable on dark or light terminals without
// adjustment. The compiled default additionally receives the standard
// technical code/diff palette below; user file themes keep their configured
// code colours unless they opt into their own role overrides.
const BALANCED_FOREGROUNDS: &[(&str, &str)] = &[
    ("muted", "#777777"),
    ("dim", "#777777"),
    ("accent", DEFAULT_ACCENT),
    ("error", "#c74747"),
    ("warning", "#9a6700"),
    ("border_focused", DEFAULT_ACCENT),
];

/// Foreground tokens applied verbatim — no luminance balancing.
/// "default" means the terminal's own foreground colour.
const VERBATIM_FOREGROUNDS: &[(&str, &str)] = &[
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
const DEFAULT_BACKGROUNDS: &[(&str, &str)] = &[("user_msg_bg", DEFAULT_ACCENT)];

// Standard technical palette for the compiled default. Source code uses one
// predictable language-neutral grammar: syntax owns foregrounds, diff owns
// quiet row surfaces, and the +/- marker carries the high-salience hue.
const STANDARD_SYNTAX_COLORS: &[(&str, &str, &str)] = &[
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

const STANDARD_DIFF_COLORS: &[(&str, &str, &str)] = &[
    // Preserve green hue and readable contrast after fixed-palette quantization.
    ("diff_added_marker", "#67d391", "#08652d"),
    ("diff_removed_marker", "#ff7d8a", "#b4233a"),
];

const STANDARD_DIFF_SURFACES: &[(&str, &str, &str)] = &[
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

fn standard_surface(dark: &str, light: &str, background: TerminalBackground) -> String {
    match background {
        TerminalBackground::Dark => dark.to_owned(),
        TerminalBackground::Light => light.to_owned(),
        // Unknown terminal backgrounds cannot safely receive absolute RGB row
        // surfaces. Preserve diff semantics through +/- text and marker colour.
        TerminalBackground::Unknown => "default".to_owned(),
    }
}

fn apply_standard_technical_palette(theme: &mut OctetTheme, background: TerminalBackground) {
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

fn parse_hex_color(value: &str) -> Option<Rgb> {
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

fn relative_luminance(color: Rgb) -> f64 {
    0.2126 * linear_channel(color.red)
        + 0.7152 * linear_channel(color.green)
        + 0.0722 * linear_channel(color.blue)
}

fn blend_channel(source: u8, destination: u8, amount: f64) -> u8 {
    (f64::from(source) + (f64::from(destination) - f64::from(source)) * amount)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn blend(source: Rgb, destination: Rgb, amount: f64) -> Rgb {
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

fn balance_to_luminance(source: Rgb, target_luminance: f64) -> Rgb {
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

fn balance_foreground(source: &str, background: TerminalBackground) -> String {
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

fn background_from_colorfgbg(value: &str) -> Option<TerminalBackground> {
    // COLORFGBG conventionally ends in the ANSI background index, e.g. 15;0
    // for light-on-dark and 0;15 for dark-on-light.
    let index = value.rsplit(';').next()?.trim().parse::<u8>().ok()?;
    match index {
        0..=6 | 8 => Some(TerminalBackground::Dark),
        7 | 9..=15 => Some(TerminalBackground::Light),
        _ => None,
    }
}

fn background_from_override(value: &str) -> Option<TerminalBackground> {
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

fn terminal_background() -> TerminalBackground {
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

fn sexy_tier(capabilities: TerminalCapabilities) -> CapabilityTier {
    if capabilities.color == ColorDepth::TrueColor {
        CapabilityTier::TrueColor
    } else {
        CapabilityTier::Baseline
    }
}

fn apply_required_surfaces(theme: &mut OctetTheme, background: TerminalBackground) {
    // Diff status belongs to the row surface, never to source foregrounds.
    theme.override_token("diff_added", "default");
    theme.override_token("diff_removed", "default");
    for &(token, source) in DEFAULT_BACKGROUNDS {
        if theme.inner.resolve_color(token).unwrap_or_default() == Color::Default {
            theme.override_token(token, &balance_background(source, background));
        }
    }
}

fn default_theme_for(
    background: TerminalBackground,
    capabilities: TerminalCapabilities,
) -> OctetTheme {
    let mut theme = OctetTheme::new(
        SexyTheme::load(None, sexy_tier(capabilities)),
        capabilities,
        background,
    );
    // Semantic UI signals get luminance-balanced; everything else is verbatim.
    for &(token, source) in BALANCED_FOREGROUNDS {
        theme.override_token(token, &balance_foreground(source, background));
    }
    for &(token, source) in VERBATIM_FOREGROUNDS {
        theme.override_token(token, source);
    }
    // Fenced code uses a quiet, copy-safe surface on known terminal profiles.
    // Indentation and syntax roles remain sufficient when the profile is unknown
    // or color is unavailable. Inline code stays on the terminal canvas.
    theme.override_token(
        "md_code_bg",
        &standard_surface("#202630", "#f1f5f4", background),
    );
    theme.override_token("md_code_inline_bg", "default");
    theme.override_token(
        "tool_output",
        match background {
            TerminalBackground::Dark => "#bec2c6",
            TerminalBackground::Light => "#50585c",
            TerminalBackground::Unknown => "default",
        },
    );
    apply_required_surfaces(&mut theme, background);
    apply_standard_technical_palette(&mut theme, background);
    // The default theme opts into the full-cell model-adaptive prompt card:
    // every submitted prompt keeps the stored colour of the model that received
    // it, filling the whole cell. Themes turn it off with `prompt_wash = false`.
    theme.override_token("prompt_wash", "true");
    // There is no model before the startup picker. Use octet green until the
    // selected model's lab is known.
    let neutral_model_accent = balance_foreground(DEFAULT_ACCENT, background);
    theme.override_token("model.use_lab_color", "true");
    theme.override_token("model_accent", &neutral_model_accent);
    theme.override_token("model_assistant", "default");
    theme
}

/// Build octet's compiled-in, terminal-balanced theme.
pub fn default_theme() -> OctetTheme {
    default_theme_for(
        terminal_background(),
        TerminalCapabilities::detect(ColorMode::Auto, false),
    )
}

#[cfg(test)]
pub(crate) fn test_theme() -> OctetTheme {
    default_theme_for(
        TerminalBackground::Unknown,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
    )
}

#[cfg(test)]
pub(crate) fn test_theme_with(capabilities: TerminalCapabilities) -> OctetTheme {
    default_theme_for(TerminalBackground::Unknown, capabilities)
}

#[cfg(test)]
pub(crate) fn test_theme_for(
    background: TerminalBackground,
    capabilities: TerminalCapabilities,
) -> OctetTheme {
    default_theme_for(background, capabilities)
}

#[cfg(test)]
pub(crate) fn test_theme_for_shimmer(
    background: TerminalBackground,
    capabilities: TerminalCapabilities,
    shimmer: ShimmerMode,
) -> OctetTheme {
    let mut theme = default_theme_for(background, capabilities);
    theme.shimmer = shimmer;
    theme
}

#[cfg(test)]
pub(crate) fn test_theme_from_source(source: &str) -> OctetTheme {
    test_theme_source_with(
        source,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Unknown,
    )
}

#[cfg(test)]
pub(crate) fn test_theme_source_with(
    source: &str,
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> OctetTheme {
    load_theme_source_for(
        source,
        "renderer-test",
        ThemeSource::File(PathBuf::from("renderer-test.toml")),
        "Renderer test",
        capabilities,
        background,
    )
    .expect("renderer test theme should compile")
}

pub(crate) fn is_reserved_theme_name(name: &str) -> bool {
    is_builtin_theme_name(name) || TerminalThemeChoice::parse(name).is_some()
}

/// Whether `name` selects a compiled-in file theme such as `Cards` or `Still`.
/// Their stems are reserved against discovered files, and this is the predicate
/// that lets configuration accept the built-in under its own name.
pub(crate) fn is_compiled_file_theme_name(name: &str) -> bool {
    compiled_file_theme_name(name).is_some()
}

/// The canonical spelling of a compiled-in file theme selector, so a persisted
/// `cards.toml` or `Cards` both resolve to the one built-in name.
pub(crate) fn compiled_file_theme_name(name: &str) -> Option<&'static str> {
    let stem = name.strip_suffix(".toml").unwrap_or(name);
    COMPILED_FILE_THEMES
        .iter()
        .find(|(built_in, _)| stem.eq_ignore_ascii_case(built_in))
        .map(|(built_in, _)| *built_in)
}

/// Every selector answered by a compiled-in theme rather than a discovered
/// file. Reserving these names keeps a user's `Cards.toml` or `Still.toml` from
/// shadowing, or being shadowed by, the built-in they select.
fn is_builtin_theme_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(DEFAULT_THEME_NAME) || compiled_file_theme_for(name).is_some()
}

/// Selectors for the compiled-in file themes, in the order the `/theme` picker
/// offers them.
pub fn compiled_file_theme_names() -> impl Iterator<Item = &'static str> {
    COMPILED_FILE_THEMES.iter().map(|(name, _)| *name)
}

/// Resolve a selector, with or without a `.toml` suffix, to the loader for a
/// compiled-in file theme. Returns `None` for the compiled default and for
/// discovered-file selectors.
fn compiled_file_theme_for(name: &str) -> Option<CompiledFileTheme> {
    let stem = name.strip_suffix(".toml").unwrap_or(name);
    COMPILED_FILE_THEMES
        .iter()
        .find(|(built_in, _)| stem.eq_ignore_ascii_case(built_in))
        .map(|(_, load)| *load)
}

/// Compile the embedded `Cards` theme for one background profile. A built-in
/// that fails to compile is a build-time defect the example test already
/// covers, so surface it as a load error rather than a silent fallback.
fn cards_theme_for(
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    load_theme_source_for(
        CARDS_THEME_SOURCE,
        CARDS_THEME_NAME,
        ThemeSource::CompiledCards,
        CARDS_THEME_NAME,
        capabilities,
        background,
    )
}

/// Compile the embedded `Still` theme for one background profile. A built-in
/// that fails to compile is a build-time defect the example test already
/// covers, so surface it as a load error rather than a silent fallback.
fn still_theme_for(
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    load_theme_source_for(
        STILL_THEME_SOURCE,
        STILL_THEME_NAME,
        ThemeSource::CompiledStill,
        STILL_THEME_NAME,
        capabilities,
        background,
    )
}

fn theme_file_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || Path::new(name).components().count() != 1
        || name
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b'\0'))
    {
        return None;
    }
    Some(if name.ends_with(".toml") {
        name.to_owned()
    } else {
        format!("{name}.toml")
    })
}

fn discover_themes(config: &Config) -> crate::resource_resolver::ResourceSnapshot {
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    resolver.discover(ResourceKind::Theme, &config.theme_paths)
}

/// Return best-effort diagnostics from the theme discovery pass. A diagnostic
/// is inspectable by callers but never turns discovery into a startup error.
#[cfg(test)]
pub fn theme_discovery_diagnostics(
    config: &Config,
) -> Vec<crate::resource_resolver::ResourceDiagnostic> {
    discover_themes(config).diagnostics().to_vec()
}

fn resolved_theme_resource(
    name: &str,
    config: &Config,
) -> anyhow::Result<(ResourceResolver, crate::resource_resolver::ResolvedResource)> {
    let file_name =
        theme_file_name(name).ok_or_else(|| anyhow::anyhow!("invalid theme name {name:?}"))?;
    let resource_name = file_name.strip_suffix(".toml").unwrap_or(&file_name);
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let snapshot = resolver.discover(ResourceKind::Theme, &config.theme_paths);
    let resource = snapshot
        .get(resource_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("theme {name:?} was not discovered"))?;
    Ok((resolver, resource))
}

/// Resolve a theme by name through the shared global/project/explicit resolver.
#[cfg(test)]
pub fn theme_path(name: &str, config: &Config) -> Option<PathBuf> {
    resolved_theme_resource(name, config)
        .ok()
        .map(|(_, resource)| resource.path)
}

fn read_theme_file_bounded(path: &Path) -> anyhow::Result<String> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("theme {} has no parent", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("theme {} has no file name", path.display()))?;
    // Reloads use the same no-follow, regular-file boundary as initial shared
    // resource reads. A trusted theme cannot be swapped for a symlink or FIFO
    // between discovery and `/theme reload`.
    let opened_path = parent.canonicalize()?.join(name);
    let bytes =
        octet_agent::secure_fs::read_regular_file_bounded(&opened_path, MAX_THEME_BYTES as usize)?;
    String::from_utf8(bytes)
        .map_err(|error| anyhow::anyhow!("theme {} is not UTF-8: {error}", path.display()))
}

fn background_token(token: &str) -> bool {
    token.ends_with("_bg") || matches!(token, "surface" | "overlay" | "raised" | "background")
}

fn adaptive_color_value(token: &str, value: &str, background: TerminalBackground) -> String {
    if parse_hex_color(value).is_none() {
        return value.to_owned();
    }
    if background_token(token) {
        balance_background(value, background)
    } else {
        balance_foreground(value, background)
    }
}

fn resolve_role_color(
    theme: &OctetTheme,
    token: &str,
    adaptive: bool,
    surface: bool,
) -> anyhow::Result<Color> {
    let value = if adaptive && parse_hex_color(token).is_some() {
        if surface {
            balance_background(token, theme.background)
        } else {
            balance_foreground(token, theme.background)
        }
    } else {
        token.to_owned()
    };
    Color::parse(&value)
        .or_else(|| theme.inner.resolve_color(&value))
        .ok_or_else(|| anyhow::anyhow!("unknown theme color or token {token:?}"))
}

fn apply_role_style(
    theme: &mut OctetTheme,
    name: &str,
    spec: &RoleStyleSpec,
    adaptive_by_default: bool,
) -> anyhow::Result<()> {
    let mapped = semantic_text_role(name);
    let token = mapped.map_or(name, |(_, token)| token);
    let adaptive = spec.adaptive.unwrap_or(adaptive_by_default);

    if let Some(foreground) = spec
        .foreground
        .as_deref()
        .filter(|foreground| *foreground != token)
    {
        let value = if adaptive {
            adaptive_color_value(token, foreground, theme.background)
        } else {
            foreground.to_owned()
        };
        theme.override_token(token, &value);
    }

    let mut style = mapped.map_or_else(
        || {
            TextStyle::plain()
                .foreground(theme.inner.resolve_color(token).unwrap_or(Color::Default))
        },
        |(role, _)| theme.inner.style(role),
    );
    if let Some(foreground) = spec.foreground.as_deref() {
        style.foreground = resolve_role_color(theme, foreground, adaptive, false)?;
    }
    if let Some(background) = spec.background.as_deref() {
        style.background = resolve_role_color(theme, background, adaptive, true)?;
    }
    if let Some(value) = spec.bold {
        style.attributes.bold = value;
    }
    if let Some(value) = spec.dim {
        style.attributes.dim = value;
    }
    if let Some(value) = spec.italic {
        style.attributes.italic = value;
    }
    if let Some(value) = spec.underline {
        style.attributes.underline = value;
    }
    if let Some(value) = spec.strikethrough {
        style.attributes.strikethrough = value;
    }
    if let Some(value) = spec.inverse {
        style.attributes.inverse = value;
    }

    if let Some((role, canonical)) = mapped {
        theme.inner.override_style(role, style);
        theme.semantic_styles.insert(canonical.to_owned(), style);
    }
    theme.semantic_styles.insert(name.to_owned(), style);
    Ok(())
}

fn build_parsed_theme(
    parsed: ParsedTheme,
    source: ThemeSource,
    fallback_name: &str,
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    let mut theme = OctetTheme::new(
        SexyTheme::load(None, sexy_tier(capabilities)),
        capabilities,
        background,
    );
    let adaptive = parsed.metadata.adaptive;
    for (token, value) in &parsed.tokens {
        let value = if adaptive {
            adaptive_color_value(token, value, background)
        } else {
            value.clone()
        };
        theme.override_token(token, &value);
    }
    apply_required_surfaces(&mut theme, background);
    for (name, style) in &parsed.roles {
        apply_role_style(&mut theme, name, style, adaptive)?;
    }
    theme.glyphs.extend(parsed.glyphs);
    theme.ascii_glyphs.extend(parsed.ascii_glyphs);
    theme.surfaces.extend(parsed.surfaces);
    theme.layout = parsed.layout;
    theme.metadata = parsed.metadata;
    if theme.metadata.name.trim().is_empty() {
        theme.metadata.name = fallback_name.to_owned();
    }
    theme.source = source;
    // The picker can render before a model is selected. Seed model-aware
    // chrome from the theme's own accent until the model family is known.
    apply_model_lab(&mut theme, ModelLab::Unknown);
    Ok(theme)
}

fn load_theme_source_for(
    source_text: &str,
    source_name: &str,
    source: ThemeSource,
    fallback_name: &str,
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    let parsed = theme_schema::parse_theme(source_text, source_name, background)?;
    build_parsed_theme(parsed, source, fallback_name, capabilities, background)
}

fn load_theme_path_for(
    path: &Path,
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    let source_text = read_theme_file_bounded(path)?;
    let fallback_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Custom theme");
    load_theme_source_for(
        &source_text,
        &path.display().to_string(),
        ThemeSource::File(path.to_owned()),
        fallback_name,
        capabilities,
        background,
    )
}

/// Load a theme from a resolver-selected path. The shared resource resolver
/// owns precedence and trust; this boundary owns bounded reads, schema
/// validation, terminal adaptation, and semantic compilation.
#[allow(dead_code)]
pub fn load_theme_path(path: &Path, config: &Config) -> anyhow::Result<OctetTheme> {
    load_theme_path_for(
        path,
        TerminalCapabilities::detect(config.color, config.plain),
        terminal_background(),
    )
}

fn load_resolved_theme_for(
    path: &Path,
    source_text: &str,
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    let fallback_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Custom theme");
    load_theme_source_for(
        source_text,
        &path.display().to_string(),
        ThemeSource::File(path.to_owned()),
        fallback_name,
        capabilities,
        background,
    )
}

/// Compile text already read by the shared resource resolver. This keeps
/// secure descriptor traversal, trust, diagnostics, and precedence in the
/// shared layer while retaining the resolved path for inspectability/reload.
#[allow(dead_code)]
pub fn load_resolved_theme(
    path: &Path,
    source_text: &str,
    config: &Config,
) -> anyhow::Result<OctetTheme> {
    load_resolved_theme_for(
        path,
        source_text,
        TerminalCapabilities::detect(config.color, config.plain),
        terminal_background(),
    )
}

/// Reference coordinator for active-theme reload conformance tests.
/// Production file changes use the unified reload supervisor.
///
/// The interactive frontend owns the `notify` adapter: it registers
/// [`Self::watch_spec`] with the watcher and forwards ordinary file changes
/// through [`Self::sender`]. Everything after the filesystem callback lives
/// here, so a reload reuses the same bounded, no-follow, regular-file
/// validation as startup and only commits at an idle prompt boundary.
///
/// Call [`Self::poll`] once per idle-loop tick with the current boundary. It
/// drains the bounded channel, admits at most one due request, runs
/// [`OctetTheme::reload`], and returns the decision to apply. Print, plain, and
/// RPC modes stay inert, and a compiled-default source never creates a watcher.
#[derive(Debug)]
#[cfg(test)]
pub struct ThemeFileReload {
    engine: ThemeReloadEngine<OctetTheme>,
    receiver: ThemeChangeReceiver,
}

#[cfg(test)]
impl ThemeFileReload {
    /// Build the coordinator for the active `theme`. Returns the bounded sender
    /// the frontend's watcher callback must use, or an error when the active
    /// file path cannot be watched.
    pub fn new(
        theme: &OctetTheme,
        mode: ThemeReloadMode,
        debounce: Duration,
    ) -> Result<(Self, ThemeChangeSender), ThemePathError> {
        let (sender, receiver) = crate::tui::theme_reload::theme_change_channel();
        let active_path = theme.source_path().map(Path::to_path_buf);
        let fallback = default_theme_for(theme.background, theme.capabilities);
        let engine = ThemeReloadEngine::new(mode, active_path, theme.clone(), fallback, debounce)?;
        Ok((Self { engine, receiver }, sender))
    }

    /// The non-recursive parent-directory watch the frontend must register, if
    /// any. `None` means the engine owns no file source or is non-interactive.
    pub fn watch_spec(&self) -> Option<ThemeWatch> {
        self.engine.watch_spec()
    }

    /// Switch runtime mode; leaving interactive cancels queued and in-flight work.
    pub fn set_mode(&mut self, mode: ThemeReloadMode) {
        self.engine.set_mode(mode);
    }

    /// Re-point the coordinator after a theme selection. Returns whether the
    /// active source changed; a change cancels queued and in-flight work.
    pub fn set_active_theme(&mut self, theme: &OctetTheme) -> Result<bool, ThemePathError> {
        self.engine.set_last_good(theme.clone());
        self.engine
            .set_compiled_fallback(default_theme_for(theme.background, theme.capabilities));
        self.engine
            .set_active_theme(theme.source_path().map(Path::to_path_buf))
    }

    /// Drain the bounded watcher channel and, at an idle boundary, load and
    /// commit at most one due reload. Invalid or unsafe edits retain the
    /// last-good theme; missing or broken sources install the compiled fallback.
    pub fn poll(
        &mut self,
        now: Instant,
        boundary: ReloadBoundary,
    ) -> Option<ReloadDecision<OctetTheme>> {
        self.engine.drain_notifications(&self.receiver, now);
        let request = self.engine.begin_if_ready(now, boundary)?;
        let path = request.path().to_path_buf();
        let token = request.token();
        let capabilities = self.engine.last_good().capabilities;
        let background = self.engine.last_good().background;
        let result = load_theme_path_for(&path, capabilities, background)
            .map_err(|error| classify_reload_failure(&error));
        Some(self.engine.finish(token, result))
    }

    /// The currently retained theme.
    pub fn last_good(&self) -> &OctetTheme {
        self.engine.last_good()
    }
}

/// Map the existing bounded loader's error into the reload retention policy
/// without changing that loader. Missing sources fall back to the compiled
/// default; unsafe replacements and schema failures retain the last-good theme.
#[cfg(test)]
fn classify_reload_failure(error: &anyhow::Error) -> ReloadFailureKind {
    use octet_agent::secure_fs::SecureFileError;
    if let Some(secure) = error.downcast_ref::<SecureFileError>() {
        return match secure {
            SecureFileError::NotRegular | SecureFileError::InvalidPath(_) => {
                ReloadFailureKind::Unsafe
            }
            SecureFileError::TooLarge { .. } => ReloadFailureKind::Broken,
            SecureFileError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                ReloadFailureKind::Missing
            }
            _ => ReloadFailureKind::Other,
        };
    }
    if let Some(io) = error.downcast_ref::<std::io::Error>() {
        return if io.kind() == std::io::ErrorKind::NotFound {
            ReloadFailureKind::Missing
        } else {
            ReloadFailureKind::Other
        };
    }
    ReloadFailureKind::Invalid
}

/// Load a named theme or return an error without altering the current theme.
pub(crate) fn load_named_theme_for_background(
    name: &str,
    config: &Config,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    let capabilities = TerminalCapabilities::detect(config.color, config.plain);
    let selector = name.trim();
    // A built-in selector always wins over a discovered file of the same stem,
    // and each built-in file theme's stem is reserved so a local copy can never
    // shadow it.
    if let Some(compile) = compiled_file_theme_for(selector) {
        return compile(capabilities, background);
    }
    if is_reserved_theme_name(selector)
        || selector
            .strip_suffix(".toml")
            .is_some_and(|stem| stem.eq_ignore_ascii_case(DEFAULT_THEME_NAME))
    {
        return Ok(default_theme_for(background, capabilities));
    }

    let (resolver, resource) = resolved_theme_resource(name, config)?;
    let source_text = resolver.read_text(&resource)?;
    load_resolved_theme_for(&resource.path, &source_text, capabilities, background)
}

/// Load a named theme or return an error without altering the current theme.
#[allow(dead_code)]
pub fn load_named_theme(name: &str, config: &Config) -> anyhow::Result<OctetTheme> {
    load_named_theme_for_background(name, config, terminal_background())
}

/// Load the startup theme for an already-detected terminal background. Missing
/// or malformed files intentionally fall back to octet's default token set instead
/// of affecting launch/print mode.
pub(crate) fn load_theme_for_background(
    config: &Config,
    background: TerminalBackground,
) -> OctetTheme {
    let background = TerminalThemeChoice::from_config(config)
        .and_then(TerminalThemeChoice::explicit_background)
        .unwrap_or(background);
    match config
        .theme
        .as_deref()
        .map(|name| load_named_theme_for_background(name, config, background))
    {
        Some(Ok(theme)) => theme,
        _ => default_theme_for(
            background,
            TerminalCapabilities::detect(config.color, config.plain),
        ),
    }
}

/// Load the startup theme. Missing or malformed files intentionally fall back
/// to octet's default token set instead of affecting launch/print mode.
pub fn load_theme(config: &Config) -> OctetTheme {
    load_theme_for_background(config, terminal_background())
}

/// Load picker previews from the same precedence-selected, trusted roots as
/// startup. Invalid files are omitted instead of presenting a broken choice;
/// a shadowed lower-precedence file is never substituted for an invalid winner.
pub(crate) fn selectable_file_themes(
    config: &Config,
    background: TerminalBackground,
) -> Vec<(String, OctetTheme)> {
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let capabilities = TerminalCapabilities::detect(config.color, config.plain);
    discover_themes(config)
        .resources()
        .iter()
        .filter(|resource| {
            !is_reserved_theme_name(&resource.name) && !resource.name.ends_with(".toml")
        })
        .filter_map(|resource| {
            let source = resolver.read_text(resource).ok()?;
            let theme =
                load_resolved_theme_for(&resource.path, &source, capabilities, background).ok()?;
            Some((resource.name.clone(), theme))
        })
        .collect()
}

/// Return the compiled default and all safe names selected by the shared
/// resolver. Parsing is deferred to the loader so discovery stays best-effort.
#[cfg(any(test, feature = "serve"))]
pub fn available_themes(config: &Config) -> Vec<String> {
    let mut names = BTreeSet::from([DEFAULT_THEME_NAME.to_owned()]);
    names.extend(compiled_file_theme_names().map(str::to_owned));
    for resource in discover_themes(config).resources() {
        if theme_file_name(&resource.name).is_some() {
            names.insert(resource.name.clone());
        }
    }
    names.into_iter().collect()
}

fn contains_any(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| text.contains(marker))
}

pub(crate) fn classify_model_text(text: &str) -> Option<ModelLab> {
    if contains_any(text, &["claude", "anthropic"]) {
        Some(ModelLab::Anthropic)
    } else if text.contains("deepseek") {
        Some(ModelLab::DeepSeek)
    } else if contains_any(text, &["gemini", "gemma", "google"]) {
        Some(ModelLab::Google)
    } else if contains_any(text, &["grok", "x.ai", "x-ai", "spacexai"]) {
        Some(ModelLab::XAi)
    } else if contains_any(text, &["llama", "meta-ai", "meta/"]) {
        Some(ModelLab::Meta)
    } else if contains_any(
        text,
        &["mistral", "mixtral", "codestral", "ministral", "devstral"],
    ) {
        Some(ModelLab::Mistral)
    } else if contains_any(text, &["qwen", "qwq", "alibaba", "dashscope"]) {
        Some(ModelLab::Alibaba)
    } else if text.contains("minimax") {
        Some(ModelLab::MiniMax)
    } else if contains_any(text, &["kimi", "moonshot"]) {
        Some(ModelLab::Kimi)
    } else if contains_any(text, &["z-ai", "zhipu", "chatglm", "glm-"]) {
        Some(ModelLab::ZAi)
    } else if contains_any(text, &["nvidia", "nemotron"]) {
        Some(ModelLab::Nvidia)
    } else if contains_any(text, &["xiaomi", "mimo-"]) {
        Some(ModelLab::Xiaomi)
    } else if contains_any(text, &["cohere", "command-r", "command-a"]) {
        Some(ModelLab::Cohere)
    } else if contains_any(text, &["amazon", "bedrock", "nova-"]) {
        Some(ModelLab::Amazon)
    } else if contains_any(text, &["microsoft", "azure", "phi-", "mai-"]) {
        Some(ModelLab::Microsoft)
    } else if contains_any(text, &["ai21", "jamba"]) {
        Some(ModelLab::Ai21)
    } else if contains_any(text, &["bytedance", "doubao", "seed-"]) {
        Some(ModelLab::ByteDance)
    } else if contains_any(text, &["perplexity", "sonar-"]) {
        Some(ModelLab::Perplexity)
    } else if contains_any(text, &["ibm", "granite"]) {
        Some(ModelLab::Ibm)
    } else if contains_any(text, &["baidu", "ernie"]) {
        Some(ModelLab::Baidu)
    } else if contains_any(text, &["tencent", "hunyuan"]) {
        Some(ModelLab::Tencent)
    } else if contains_any(text, &["allenai", "allen-ai", "olmo"]) {
        Some(ModelLab::AllenAi)
    } else if contains_any(text, &["openai", "chatgpt", "codex", "gpt-"])
        || text.starts_with("o1")
        || text.starts_with("o3")
        || text.starts_with("o4")
    {
        Some(ModelLab::OpenAi)
    } else {
        None
    }
}

pub(crate) fn classify_model_identity(id: &str, api_name: &str, endpoint: &str) -> ModelLab {
    let model_text = format!(
        "{} {}",
        id.to_ascii_lowercase(),
        api_name.to_ascii_lowercase()
    );
    if let Some(lab) = classify_model_text(&model_text) {
        return lab;
    }

    let endpoint = endpoint.trim().to_ascii_lowercase();
    match endpoint.as_str() {
        "xai" | "x-ai" => ModelLab::XAi,
        "meta" | "meta-ai" => ModelLab::Meta,
        "zai" | "z-ai" | "zhipu" => ModelLab::ZAi,
        "aws" | "amazon-bedrock" => ModelLab::Amazon,
        "ai2" | "allen-ai" => ModelLab::AllenAi,
        _ => classify_model_text(&endpoint).unwrap_or(ModelLab::Unknown),
    }
}

pub(crate) fn model_spec_lab(model: &ModelSpec) -> ModelLab {
    classify_model_identity(&model.id.0, &model.api_name, &model.endpoint.0)
}

pub(crate) fn model_lab(model: &Model) -> ModelLab {
    model_spec_lab(&model.spec)
}

/// Version-stable lab-to-colour assignment used only when a prompt is first
/// appended (and as a legacy replay fallback). The exact result is persisted,
/// so future palette changes cannot recolour existing prompts.
fn prompt_color_for_lab(lab: ModelLab) -> String {
    let source = lab.source_color().unwrap_or(DEFAULT_ACCENT);
    balance_foreground(source, TerminalBackground::Unknown)
}

pub(crate) fn prompt_color_for_model_id(model_id: &str) -> String {
    let normalized = model_id.trim().to_ascii_lowercase();
    prompt_color_for_lab(classify_model_identity(&normalized, "", ""))
}

pub(crate) fn prompt_color_for_model(model: &Model) -> String {
    prompt_color_for_lab(model_lab(model))
}

/// Install the active lab color into dedicated chrome/assistant tokens. A
/// custom theme keeps its existing accent roles unless it sets
/// `use_lab_color = true` or a lab source such as `anthropic = "#..."` under
/// `[model]`; model colors are always balanced for the terminal background.
fn apply_model_lab_for(theme: &mut OctetTheme, lab: ModelLab, background: TerminalBackground) {
    let configured_key = format!("model.{}", lab.key());
    let configured = theme
        .resolve::<String>(&configured_key)
        .filter(|color| parse_hex_color(color).is_some());
    let use_lab_color = configured.is_some()
        || theme
            .resolve::<bool>("model.use_lab_color")
            .unwrap_or(false);
    let source = configured
        .or_else(|| {
            if use_lab_color {
                lab.source_color().map(str::to_owned)
            } else {
                None
            }
        })
        .or_else(|| {
            theme
                .resolve::<String>("accent")
                .filter(|color| parse_hex_color(color).is_some())
        })
        .unwrap_or_else(|| DEFAULT_ACCENT.to_owned());
    let assistant_source = theme
        .resolve::<String>("assistant_msg_text")
        .unwrap_or_else(|| "default".into());
    theme.override_token("model_accent", &balance_foreground(&source, background));
    theme.override_token(
        "model_assistant",
        &balance_foreground(&assistant_source, background),
    );
}

pub(crate) fn apply_model_lab(theme: &mut OctetTheme, lab: ModelLab) {
    let background = theme.background;
    apply_model_lab_for(theme, lab, background);
}

#[cfg(test)]
mod tests;
