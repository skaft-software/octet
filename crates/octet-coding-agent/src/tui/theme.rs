#![allow(missing_docs)]

use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::{Duration, Instant};

use octet_ai::{Model, ModelSpec};
#[cfg(test)]
use sexy_tui_rs::theme::capability::CapabilityTier;
use sexy_tui_rs::theme::Theme as SexyTheme;
use sexy_tui_rs::{
    CodeOverflow, Color, RenderOptions, RichRenderer, TextRole, TextStyle, UnorderedListMarker,
};

use crate::config::{ColorMode, Config};
use crate::resource_resolver::ResourceResolver;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
#[cfg(test)]
use crate::tui::theme_reload::{
    ReloadBoundary, ReloadDecision, ReloadFailureKind, ThemeChangeReceiver, ThemeChangeSender,
    ThemePathError, ThemeReloadEngine, ThemeReloadMode, ThemeWatch,
};
use crate::tui::theme_schema::{self, ParsedTheme, RoleStyleSpec, ThemeSurface};

mod discovery;
mod pi;
mod terminal_colors;
use crate::tui::terminal::TerminalThemeColors;
use pi::{pi_theme_for, pi_theme_with_colors};

pub use discovery::compiled_file_theme_names;
pub(crate) use discovery::{
    compiled_file_theme_name, is_compiled_file_theme_name, is_reserved_theme_name,
};
// The suite pins theme resolution against a real resource root, so these two
// stay reachable through this module's glob.
use discovery::{
    cards_theme_for, compiled_file_theme_for, discover_themes, read_theme_file_bounded,
    resolved_theme_resource, still_theme_for,
};
// Only the test-only `available_themes` names it here.
#[cfg(test)]
use discovery::theme_file_name;
#[cfg(test)]
pub use discovery::{theme_discovery_diagnostics, theme_path};

use terminal_colors::{
    apply_standard_technical_palette, balance_background, balance_foreground, balance_to_luminance,
    blend, named_color, nearest_ansi16_code, nearest_ansi256, parse_hex_color, rich_capabilities,
    sexy_tier, standard_surface, terminal_background, BALANCED_FOREGROUNDS, CONTEXT_COLOR_DEFAULTS,
    DEFAULT_ACCENT, DEFAULT_BACKGROUNDS, VERBATIM_FOREGROUNDS,
};
// The suite pins the approximation tables and the background probes directly,
// so they stay reachable through this module's glob. Gating the import keeps
// them out of the library build's reach, so it stays warning-free.
#[cfg(test)]
use terminal_colors::{
    ansi256_rgb, background_from_colorfgbg, background_from_override, relative_luminance, ANSI16,
    STANDARD_SYNTAX_COLORS,
};
// `modes::interactive` reaches this through the theme module, so the re-export
// is the public path; the type itself moved to `terminal_colors`.
pub(crate) use terminal_colors::background_from_terminal_rgb;

#[allow(unused_imports)]
pub use crate::tui::theme_schema::{
    ResolvedThemeLayout, ResolvedThemeSurface, ThemeDensity, ThemeLayout, ThemeMetadata,
    ThemeSurfaceAlign, ThemeSurfaceChrome, ThemeSurfaceHeading, ThemeSurfaceWidth, MAX_THEME_BYTES,
};

/// Stable name for octet's compiled-in default theme and legacy selectors.
pub const DEFAULT_THEME_NAME: &str = "default";

/// Pi 1.0's terminal-adaptive default, implemented natively without the bridge.
pub const PI_THEME_NAME: &str = "pi";

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
    (PI_THEME_NAME, pi_theme_for),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeSource {
    CompiledDefault,
    /// The compiled-in `Cards` theme, embedded from `examples/themes/Cards.toml`.
    CompiledCards,
    /// The compiled-in `Still` theme, embedded from `examples/themes/Still.toml`.
    CompiledStill,
    /// Pi 1.0's generated system palette, independent of extension enablement.
    CompiledPi,
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

#[cfg(test)]
std::thread_local! {
    // Resolved theme constructions and clones, isolated from parallel tests.
    static THEME_WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn take_theme_work() -> (usize, usize) {
    THEME_WORK.with(|work| work.replace((0, 0)))
}

#[cfg(test)]
#[derive(Debug)]
struct ThemeCloneWork;

#[cfg(test)]
impl Clone for ThemeCloneWork {
    fn clone(&self) -> Self {
        THEME_WORK.with(|work| {
            let (constructions, clones) = work.get();
            work.set((constructions, clones + 1));
        });
        Self
    }
}

/// octet-side styling boundary around sexy-tui's semantic token store. octet owns
/// model-family palette selection and contrast balancing; sexy-tui owns rich
/// text layout, sanitization, syntax highlighting, and semantic encoding.
#[derive(Clone, Debug)]
pub struct OctetTheme {
    #[cfg(test)]
    _clone_work: ThemeCloneWork,
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
    // Already validated, bounded source; native palettes resolve both variants
    // from the same snapshot, without rereading a changed file during paint.
    native_source: Option<std::sync::Arc<str>>,
    // Owned by the shared input stream; retained across selection and reload.
    terminal_colors: TerminalThemeColors,
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
        #[cfg(test)]
        THEME_WORK.with(|work| {
            let (constructions, clones) = work.get();
            work.set((constructions + 1, clones));
        });
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
            #[cfg(test)]
            _clone_work: ThemeCloneWork,
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
            native_source: None,
            terminal_colors: TerminalThemeColors::default(),
        }
    }

    pub(crate) fn terminal_colors(&self) -> TerminalThemeColors {
        self.terminal_colors
    }

    /// Carry terminal observations through previews, selection and reload. Only
    /// Pi's generated theme consumes them; other themes retain their own colors.
    pub(crate) fn with_terminal_colors(mut self, colors: TerminalThemeColors) -> Self {
        if matches!(self.source, ThemeSource::CompiledPi) && colors != self.terminal_colors {
            self = pi_theme_with_colors(
                self.capabilities,
                self.background,
                colors.foreground,
                colors.background,
                colors.palette,
            )
            .expect("compiled Pi recipe must remain valid");
        }
        self.terminal_colors = colors;
        self
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

    /// The shared JSON projection retains this role in files and snapshots too.
    pub(crate) fn is_pi_theme(&self) -> bool {
        self.semantic_styles.contains_key("extension.pi.accent")
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
            | ThemeSource::CompiledStill
            | ThemeSource::CompiledPi => None,
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

    #[allow(dead_code)]
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
        if self.capabilities.color == ColorDepth::None {
            return text.to_owned();
        }
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
            ThemeSource::CompiledPi => pi_theme_for(self.capabilities, self.background)
                .map(|theme| theme.with_terminal_colors(self.terminal_colors)),
            ThemeSource::File(path) => {
                load_theme_path_for(path, self.capabilities, self.background)
            }
        }
    }

    /// Resolve this exact theme snapshot for Tern's native RGB appearance.
    /// Runtime model styling is applied by the native projector afterwards.
    pub(crate) fn for_native_background(
        &self,
        background: TerminalBackground,
    ) -> anyhow::Result<Self> {
        let mut capabilities = self.capabilities;
        if capabilities.color != ColorDepth::None {
            capabilities.color = ColorDepth::TrueColor;
        }
        if matches!(self.source, ThemeSource::CompiledPi) {
            let same_profile =
                self.background == background && self.terminal_colors.background.is_some();
            let (foreground, canvas) = if same_profile {
                (
                    self.terminal_colors.foreground,
                    self.terminal_colors.background,
                )
            } else if background == TerminalBackground::Light {
                (Some((0, 0, 0)), Some((255, 255, 255)))
            } else {
                (Some((229, 229, 231)), Some((0, 0, 0)))
            };
            return pi_theme_with_colors(
                capabilities,
                background,
                foreground,
                canvas,
                self.terminal_colors.palette,
            );
        }
        let Some(source) = &self.native_source else {
            return Ok(default_theme_for(background, capabilities));
        };
        load_theme_source_for(
            source,
            "native theme snapshot",
            self.source.clone(),
            &self.metadata.name,
            capabilities,
            background,
        )
    }

    pub fn fg(&self, token: &str, text: &str) -> String {
        if self.capabilities.color == ColorDepth::None {
            return text.to_owned();
        }
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
        if self.is_pi_theme() {
            return self.role_rgb("model_accent");
        }
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

    /// Native equivalent of the default prompt wash's contrast-balanced fill.
    pub(crate) fn native_prompt_rgb(&self, source: (u8, u8, u8)) -> Option<(u8, u8, u8)> {
        let target = match self.background {
            TerminalBackground::Dark => 0.10,
            TerminalBackground::Light => 0.88,
            TerminalBackground::Unknown => return None,
        };
        let color = balance_to_luminance(
            Rgb {
                red: source.0,
                green: source.1,
                blue: source.2,
            },
            target,
        );
        Some((color.red, color.green, color.blue))
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
    let mut theme = build_parsed_theme(parsed, source, fallback_name, capabilities, background)?;
    theme.native_source = Some(std::sync::Arc::from(source_text));
    Ok(theme)
}

fn load_theme_path_for(
    path: &Path,
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    let source_text = read_theme_file_bounded(path)?;
    let source_text =
        crate::extensions::resource_paths::pi_theme::native_source(path, &source_text)?;
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
    let source_text =
        crate::extensions::resource_paths::pi_theme::native_source(path, source_text)?;
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

/// Resolve compiled selectors with the already-owned terminal profile, without
/// any resource discovery or filesystem access.
pub(crate) fn compiled_theme_for_selector(
    name: &str,
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> Option<anyhow::Result<OctetTheme>> {
    if let Some(compile) = compiled_file_theme_for(name) {
        return Some(compile(capabilities, background));
    }
    let choice = TerminalThemeChoice::parse(name);
    if choice.is_some() || name.eq_ignore_ascii_case(DEFAULT_THEME_NAME) {
        let background = choice
            .and_then(TerminalThemeChoice::explicit_background)
            .unwrap_or(background);
        return Some(Ok(default_theme_for(background, capabilities)));
    }
    None
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
            !is_reserved_theme_name(&resource.name)
                && !resource.name.ends_with(".toml")
                && !resource.name.ends_with(".json")
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
#[cfg(test)]
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
    if theme.is_pi_theme() {
        return;
    }
    let background = theme.background;
    apply_model_lab_for(theme, lab, background);
}

#[cfg(test)]
mod tests;
