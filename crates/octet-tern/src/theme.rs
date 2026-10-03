//! Project an octet theme file onto the TSP theme palette.
//!
//! Tern "wears" a program's theme: the `t` verb carries the resolved palette as
//! `token → #rrggbb`, and the terminal derives its own chrome (surface fills,
//! card tints, the composer, chart colours) from those tokens. The token names
//! in the palette are the ones the terminal's surface framework already knows,
//! so octet must translate its own vocabulary into them.
//!
//! This module reads octet's documented theme-file shape — `[colors]`/
//! `[tokens]`, `[roles."…"]`, and the `[variants.universal|dark|light]`
//! overlays — and emits the palette. Tokens left at the terminal default
//! (`"default"`) are omitted, exactly as the protocol intends.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde_json::{Map, Value};

use crate::wire::{Palette, VariantNames};

/// A resolved palette: appearance variant → token → `#rrggbb`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OctetPalette {
    /// Theme name (used in the palette's `name` field).
    pub name: String,
    /// Dark-variant tokens.
    pub dark: BTreeMap<String, String>,
    /// Light-variant tokens, when the theme defines one.
    pub light: Option<BTreeMap<String, String>>,
}

/// Why a theme file could not be read.
#[derive(Debug)]
pub enum ThemeError {
    /// I/O failure reading the file.
    Io(std::io::Error),
    /// TOML parse failure.
    Toml(toml::de::Error),
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThemeError::Io(error) => write!(f, "reading theme: {error}"),
            ThemeError::Toml(error) => write!(f, "parsing theme: {error}"),
        }
    }
}

impl std::error::Error for ThemeError {}

/// Load and project an octet theme file.
pub fn load(path: impl AsRef<Path>) -> Result<OctetPalette, ThemeError> {
    let text = std::fs::read_to_string(path).map_err(ThemeError::Io)?;
    from_toml(&text)
}

/// Load and project an octet theme from TOML text.
pub fn from_toml(text: &str) -> Result<OctetPalette, ThemeError> {
    let doc: toml::Value = toml::from_str(text).map_err(ThemeError::Toml)?;
    Ok(project(&doc))
}

/// A theme's raw colour vocabulary for one appearance variant.
#[derive(Debug, Default, Clone)]
struct Resolved {
    colors: BTreeMap<String, String>,
    /// Role paths (`surface.user`, `surface.tool.border`) → colour.
    roles_fg: BTreeMap<String, String>,
    roles_bg: BTreeMap<String, String>,
}

impl Resolved {
    fn overlay(&mut self, other: &Resolved) {
        self.colors.extend(other.colors.clone());
        self.roles_fg.extend(other.roles_fg.clone());
        self.roles_bg.extend(other.roles_bg.clone());
    }
}

/// Collect `name → colour` leaves from a colour table (flat or nested).
fn collect_colors(value: &toml::Value, out: &mut BTreeMap<String, String>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        match child {
            toml::Value::String(text) => {
                out.insert(key.clone(), text.clone());
            }
            toml::Value::Table(_) => collect_colors(child, out),
            _ => {}
        }
    }
}

/// Collect `[roles.*]` foreground/background colours from a document section.
fn collect_roles(section: &toml::Value, resolved: &mut Resolved) {
    let Some(roles) = section.get("roles").and_then(toml::Value::as_table) else {
        return;
    };
    for (path, entry) in roles {
        let Some(table) = entry.as_table() else {
            continue;
        };
        if let Some(fg) = table.get("foreground").and_then(toml::Value::as_str) {
            resolved.roles_fg.insert(path.clone(), fg.to_owned());
        }
        if let Some(bg) = table.get("background").and_then(toml::Value::as_str) {
            resolved.roles_bg.insert(path.clone(), bg.to_owned());
        }
    }
}

/// Read the base document's colours and roles.
fn base(doc: &toml::Value) -> Resolved {
    let mut resolved = Resolved::default();
    for key in ["colors", "tokens"] {
        if let Some(section) = doc.get(key) {
            collect_colors(section, &mut resolved.colors);
        }
    }
    collect_roles(doc, &mut resolved);
    resolved
}

/// Read one `[variants.<name>]` overlay.
fn variant(doc: &toml::Value, name: &str) -> Resolved {
    let mut resolved = Resolved::default();
    let Some(section) = doc.get("variants").and_then(|v| v.get(name)) else {
        return resolved;
    };
    for key in ["colors", "tokens"] {
        if let Some(table) = section.get(key) {
            collect_colors(table, &mut resolved.colors);
        }
    }
    collect_roles(section, &mut resolved);
    resolved
}

/// Whether the document declares a light variant.
fn has_light(doc: &toml::Value) -> bool {
    doc.get("variants").and_then(|v| v.get("light")).is_some()
}

fn hex(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("default") || trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && (trimmed.len() == 4 || trimmed.len() == 7 || trimmed.len() == 9)
    {
        return Some(trimmed.to_lowercase());
    }
    None
}

/// Insert `token` when the source token resolves to a hex colour.
fn put(out: &mut BTreeMap<String, String>, out_token: &str, resolved: &Resolved, in_token: &str) {
    if let Some(value) = resolved.colors.get(in_token).and_then(|v| hex(v)) {
        out.insert(out_token.to_owned(), value);
    }
}

/// Insert from a role path's foreground/background.
fn put_role(
    out: &mut BTreeMap<String, String>,
    out_token: &str,
    resolved: &Resolved,
    role: &str,
    background: bool,
) {
    let table = if background {
        &resolved.roles_bg
    } else {
        &resolved.roles_fg
    };
    if let Some(value) = table.get(role).and_then(|v| hex(v)) {
        out.insert(out_token.to_owned(), value);
    }
}

/// The octet `[colors]` token → TSP/omp palette token mapping.
const COLOR_MAP: &[(&str, &str)] = &[
    ("foreground", "text"),
    ("muted", "muted"),
    ("dim", "dim"),
    ("accent", "accent"),
    ("success", "success"),
    ("error", "error"),
    ("warning", "warning"),
    ("border", "border"),
    ("border_idle", "borderMuted"),
    ("border_focused", "borderAccent"),
    ("user_msg_bg", "userMessageBg"),
    ("user_msg_text", "userMessageText"),
    ("assistant_msg_bg", "customMessageBg"),
    ("assistant_msg_text", "customMessageText"),
    ("tool_title", "toolTitle"),
    ("tool_output", "toolOutput"),
    ("diff_added", "toolDiffAdded"),
    ("diff_removed", "toolDiffRemoved"),
    ("diff_context", "toolDiffContext"),
    ("md_heading", "mdHeading"),
    ("md_link", "mdLink"),
    ("md_code", "mdCode"),
    ("md_code_block", "mdCodeBlock"),
    ("md_code_border", "mdCodeBlockBorder"),
    ("md_quote", "mdQuote"),
    ("md_quote_border", "mdQuoteBorder"),
    ("md_hr", "mdHr"),
    ("md_list_bullet", "mdListBullet"),
    ("syntax_comment", "syntaxComment"),
    ("syntax_keyword", "syntaxKeyword"),
    ("syntax_function", "syntaxFunction"),
    ("syntax_variable", "syntaxVariable"),
    ("syntax_string", "syntaxString"),
    ("syntax_number", "syntaxNumber"),
    ("syntax_type", "syntaxType"),
    ("syntax_operator", "syntaxOperator"),
    ("syntax_punctuation", "syntaxPunctuation"),
];

/// Build the palette for one appearance variant.
fn project_variant(resolved: &Resolved) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();

    for (from, to) in COLOR_MAP {
        put(&mut out, to, resolved, from);
    }

    // Surface fills come from the theme's shaded role backgrounds.
    put_role(&mut out, "userMessageBg", resolved, "surface.user", true);
    put_role(
        &mut out,
        "customMessageBg",
        resolved,
        "surface.assistant",
        true,
    );
    put_role(&mut out, "toolPendingBg", resolved, "surface.tool", true);
    put_role(&mut out, "toolSuccessBg", resolved, "surface.tool", true);
    put_role(&mut out, "toolErrorBg", resolved, "surface.tool", true);
    put_role(&mut out, "statusLineBg", resolved, "surface.shell", true);

    // Reasoning/status tokens have no octet source; derive them from the
    // accent and semantic colours so Tern's thinking chrome is coherent.
    let accent = out.get("accent").cloned();
    let dim = out.get("dim").cloned();
    let error = out.get("error").cloned();
    let warning = out.get("warning").cloned();
    let success = out.get("success").cloned();
    let muted = out.get("muted").cloned();
    let code = out.get("mdCode").cloned();

    fn derive(out: &mut BTreeMap<String, String>, token: &str, value: Option<&String>) {
        if let Some(value) = value {
            out.entry(token.to_owned()).or_insert_with(|| value.clone());
        }
    }
    let tool_output = out.get("toolOutput").cloned();
    derive(&mut out, "thinkingText", muted.as_ref().or(dim.as_ref()));
    derive(&mut out, "thinkingOff", dim.as_ref());
    derive(&mut out, "thinkingMinimal", muted.as_ref());
    derive(&mut out, "thinkingLow", accent.as_ref());
    derive(&mut out, "thinkingMedium", accent.as_ref());
    derive(&mut out, "thinkingHigh", accent.as_ref());
    derive(&mut out, "thinkingXhigh", error.as_ref());
    derive(&mut out, "bashMode", accent.as_ref());
    derive(&mut out, "pythonMode", code.as_ref().or(accent.as_ref()));
    derive(&mut out, "selectedBg", accent.as_ref());
    derive(&mut out, "statusLineSep", dim.as_ref());
    derive(&mut out, "statusLineModel", accent.as_ref());
    derive(&mut out, "statusLinePath", code.as_ref());
    derive(&mut out, "statusLineGitClean", success.as_ref());
    derive(&mut out, "statusLineGitDirty", warning.as_ref());
    derive(&mut out, "statusLineContext", code.as_ref());
    derive(&mut out, "statusLineSpend", code.as_ref());
    derive(&mut out, "statusLineCost", warning.as_ref());
    derive(&mut out, "statusLineSubagents", accent.as_ref());
    derive(&mut out, "statusLineOutput", tool_output.as_ref());
    derive(&mut out, "statusLineStaged", success.as_ref());
    derive(&mut out, "statusLineDirty", warning.as_ref());
    derive(&mut out, "statusLineUntracked", error.as_ref());

    // Export colours used by HTML/PDF renders of the same theme.
    if let Some(page) = resolved.colors.get("composer_bg").and_then(|v| hex(v)) {
        out.insert("pageBg".to_owned(), page);
    }
    if let Some(card) = out.get("userMessageBg").cloned() {
        out.insert("cardBg".to_owned(), card);
    }
    if let Some(info) = resolved.colors.get("md_code_bg").and_then(|v| hex(v)) {
        out.insert("infoBg".to_owned(), info);
    }

    out
}

/// Project a parsed theme document.
fn project(doc: &toml::Value) -> OctetPalette {
    let name = doc
        .get("metadata")
        .and_then(|m| m.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or("octet")
        .to_owned();

    let mut dark_resolved = base(doc);
    dark_resolved.overlay(&variant(doc, "universal"));

    let mut light_resolved = dark_resolved.clone();
    light_resolved.overlay(&variant(doc, "light"));

    let mut dark = dark_resolved.clone();
    dark.overlay(&variant(doc, "dark"));

    let dark = project_variant(&dark);
    let light = if has_light(doc) {
        Some(project_variant(&light_resolved))
    } else {
        None
    };

    OctetPalette { name, dark, light }
}

impl OctetPalette {
    /// Build the TSP `t` message for a surface.
    pub fn to_wire(&self, surface: &str) -> Palette {
        let convert = |map: &BTreeMap<String, String>| {
            let mut out = Map::new();
            for (key, value) in map {
                out.insert(key.clone(), Value::String(value.clone()));
            }
            out
        };
        Palette {
            sf: surface.to_owned(),
            dark: Some(convert(&self.dark)),
            light: self.light.as_ref().map(convert),
            name: Some(VariantNames {
                dark: Some(self.name.clone()),
                light: self.light.as_ref().map(|_| self.name.clone()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"
[metadata]
name = "Cards"

[colors]
foreground = "default"
muted = "#8a8f98"
accent = "#d29c54"
error = "#cf7079"
md_code = "#c9c2b4"
syntax_string = "#8fa58c"
composer_bg = "#202020"

[roles."surface.user"]
background = "#202020"

[roles."surface.tool"]
background = "#1e1e1e"

[variants.light.colors]
muted = "#5a5f68"

[variants.light.roles."surface.user"]
background = "#efebe4"
"##;

    #[test]
    fn projects_octet_tokens_to_palette_tokens() {
        let palette = from_toml(SAMPLE).expect("parses");
        assert_eq!(palette.name, "Cards");
        assert_eq!(
            palette.dark.get("accent").map(String::as_str),
            Some("#d29c54")
        );
        assert_eq!(
            palette.dark.get("muted").map(String::as_str),
            Some("#8a8f98")
        );
        assert_eq!(
            palette.dark.get("syntaxString").map(String::as_str),
            Some("#8fa58c")
        );
        assert_eq!(
            palette.dark.get("userMessageBg").map(String::as_str),
            Some("#202020")
        );
        assert_eq!(
            palette.dark.get("toolPendingBg").map(String::as_str),
            Some("#1e1e1e")
        );
        // "default" is omitted, never sent.
        assert!(!palette.dark.contains_key("text"));
        // Derived thinking tokens exist.
        assert_eq!(
            palette.dark.get("thinkingLow").map(String::as_str),
            Some("#d29c54")
        );
    }

    #[test]
    fn light_variant_overlays_the_base() {
        let palette = from_toml(SAMPLE).expect("parses");
        let light = palette.light.expect("light variant");
        assert_eq!(light.get("muted").map(String::as_str), Some("#5a5f68"));
        assert_eq!(
            light.get("userMessageBg").map(String::as_str),
            Some("#efebe4")
        );
        // Untouched tokens inherit the base.
        assert_eq!(light.get("accent").map(String::as_str), Some("#d29c54"));
    }

    #[test]
    fn wire_palette_carries_both_variants() {
        let palette = from_toml(SAMPLE).expect("parses");
        let wire = palette.to_wire("octet.session");
        assert_eq!(wire.sf, "octet.session");
        assert!(wire.dark.is_some());
        assert!(wire.light.is_some());
        assert_eq!(
            wire.name.as_ref().and_then(|n| n.dark.as_deref()),
            Some("Cards")
        );
    }
}
