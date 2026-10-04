//! Bounded, data-only Pi JSON palette projection for the native theme loader.
//!
//! This does not execute the adapter, grant filesystem access, or resolve roots.
//! The shared resolver/reader still owns precedence, trust and no-follow reads;
//! the resulting TOML goes through the ordinary native schema/compiler. Pi layout,
//! HTML export and model-specific presentation are not native theme contracts.
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, ensure, Context};
use serde_json::{Map, Value};

const MAX_BYTES: usize = 256 * 1024;
const MAX_VARS: usize = 256;

// The original Pi palette plus the backwards-compatible optional additions.
const TOKENS: &[&str] = &[
    "accent",
    "border",
    "borderAccent",
    "borderMuted",
    "success",
    "error",
    "warning",
    "muted",
    "dim",
    "text",
    "thinkingText",
    "scrollbarTrack",
    "scrollbarThumb",
    "searchMatchText",
    "userMessageText",
    "customMessageText",
    "customMessageLabel",
    "toolTitle",
    "toolOutput",
    "mdHeading",
    "mdLink",
    "mdLinkUrl",
    "mdCode",
    "mdCodeBlock",
    "mdCodeBlockBorder",
    "mdQuote",
    "mdQuoteBorder",
    "mdHr",
    "mdListBullet",
    "toolDiffAdded",
    "toolDiffRemoved",
    "toolDiffContext",
    "syntaxComment",
    "syntaxKeyword",
    "syntaxFunction",
    "syntaxVariable",
    "syntaxString",
    "syntaxNumber",
    "syntaxType",
    "syntaxOperator",
    "syntaxPunctuation",
    "thinkingOff",
    "thinkingMinimal",
    "thinkingLow",
    "thinkingMedium",
    "thinkingHigh",
    "thinkingXhigh",
    "thinkingMax",
    "bashMode",
    "selectedBg",
    "searchMatchBg",
    "userMessageBg",
    "customMessageBg",
    "toolPendingBg",
    "toolSuccessBg",
    "toolErrorBg",
];

const PROJECTION: &[(&str, &str)] = &[
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
    ("selectedBg", "selected_bg"),
    ("userMessageText", "user_msg_text"),
    ("userMessageBg", "user_msg_bg"),
    ("toolTitle", "tool_title"),
    ("toolOutput", "tool_output"),
    ("toolPendingBg", "tool_pending_bg"),
    ("toolSuccessBg", "tool_success_bg"),
    ("toolErrorBg", "tool_error_bg"),
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

/// Normalize a securely read file, retaining TOML byte-for-byte. Call at the
/// file/resolved-text boundary, not when recompiling an already-native snapshot.
pub(crate) fn native_source<'a>(path: &Path, source: &'a str) -> anyhow::Result<Cow<'a, str>> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
        return Ok(Cow::Borrowed(source));
    }
    ensure!(source.len() <= MAX_BYTES, "Pi theme exceeds 256 KiB");
    let value: Value = serde_json::from_str(source.strip_prefix('\u{feff}').unwrap_or(source))
        .context("Pi theme must be a JSON document")?;
    let document = object(&value, "document")?;
    fields(
        document,
        &["$schema", "name", "appearance", "vars", "colors", "export"],
    )?;
    let name = text(document.get("name").context("Pi theme requires name")?, 80)?;
    ensure!(!name.contains('/'), "Pi theme name contains a slash");
    if let Some(schema) = document.get("$schema") {
        text(schema, 4096)?;
    }
    let appearance = match document.get("appearance") {
        Some(value) => {
            let appearance = text(value, 16)?;
            ensure!(
                matches!(appearance, "light" | "dark"),
                "invalid Pi theme appearance"
            );
            appearance
        }
        None => "any",
    };
    let empty = Map::new();
    let vars = match document.get("vars") {
        Some(value) => object(value, "vars")?,
        None => &empty,
    };
    ensure!(vars.len() <= MAX_VARS, "Pi theme exceeds 256 variables");
    for (key, value) in vars {
        plain(key, 128)?;
        color_value(value)?;
    }
    let colors = object(
        document.get("colors").context("Pi theme requires colors")?,
        "colors",
    )?;
    fields(colors, TOKENS)?;
    let mut palette = toml::Table::new();
    for &token in TOKENS {
        let fallback = match token {
            "scrollbarTrack" => "muted",
            "scrollbarThumb" | "searchMatchText" => "text",
            "thinkingMax" => "thinkingXhigh",
            "searchMatchBg" => "selectedBg",
            _ => token,
        };
        let value = colors
            .get(token)
            .or_else(|| colors.get(fallback))
            .with_context(|| format!("Pi theme missing required color {token}"))?;
        palette.insert(token.into(), toml::Value::String(resolve(value, vars)?));
    }
    // Validate even presentation-only data rather than admitting malformed input
    // merely because a native renderer does not consume HTML export colors.
    if let Some(export) = document.get("export") {
        let export = object(export, "export")?;
        fields(export, &["pageBg", "cardBg", "infoBg"])?;
        for value in export.values() {
            resolve(value, vars)?;
        }
    }
    let mut metadata = toml::Table::new();
    metadata.insert("name".into(), toml::Value::String(name.into()));
    metadata.insert(
        "description".into(),
        toml::Value::String(
            "Pi palette projection; native layout and model presentation remain authoritative"
                .into(),
        ),
    );
    metadata.insert("terminal".into(), toml::Value::String(appearance.into()));
    metadata.insert("adaptive".into(), toml::Value::Boolean(false));
    let mut native_colors = toml::Table::new();
    for &(pi, native) in PROJECTION {
        native_colors.insert(native.into(), palette[pi].clone());
    }
    // Keep every Pi color inspectable/usable as an explicitly namespaced role;
    // this does not pretend that Pi-only chrome has a native layout equivalent.
    let mut roles = toml::Table::new();
    for (token, value) in palette {
        let mut role = toml::Table::new();
        role.insert(
            if token.ends_with("Bg") {
                "background"
            } else {
                "foreground"
            }
            .into(),
            value,
        );
        roles.insert(format!("extension.pi.{token}"), toml::Value::Table(role));
    }
    let mut native = toml::Table::new();
    native.insert("metadata".into(), toml::Value::Table(metadata));
    native.insert("colors".into(), toml::Value::Table(native_colors));
    native.insert("roles".into(), toml::Value::Table(roles));
    let output = toml::to_string(&native).context("cannot encode native Pi palette")?;
    ensure!(
        output.len() <= MAX_BYTES,
        "native Pi palette exceeds 256 KiB"
    );
    Ok(Cow::Owned(output))
}

fn object<'a>(value: &'a Value, label: &str) -> anyhow::Result<&'a Map<String, Value>> {
    value
        .as_object()
        .with_context(|| format!("Pi theme {label} must be an object"))
}

fn fields(object: &Map<String, Value>, allowed: &[&str]) -> anyhow::Result<()> {
    ensure!(
        object.keys().all(|key| allowed.contains(&key.as_str())),
        "unknown Pi theme field"
    );
    Ok(())
}

fn plain(value: &str, bound: usize) -> anyhow::Result<&str> {
    ensure!(
        value.len() <= bound,
        "Pi theme string exceeds {bound} bytes"
    );
    ensure!(
        !value.chars().any(char::is_control),
        "Pi theme contains terminal controls"
    );
    Ok(value)
}

fn text(value: &Value, bound: usize) -> anyhow::Result<&str> {
    plain(
        value.as_str().context("Pi theme value must be a string")?,
        bound,
    )
}

fn color_value(value: &Value) -> anyhow::Result<()> {
    if value.is_number() {
        ensure!(
            value.as_u64().is_some_and(|index| index <= 255),
            "Pi theme index must be an integer from 0 to 255"
        );
    } else {
        text(value, 1024)?;
    }
    Ok(())
}

fn resolve<'a>(mut value: &'a Value, vars: &'a Map<String, Value>) -> anyhow::Result<String> {
    let mut visited = BTreeSet::new();
    loop {
        color_value(value)?;
        if let Some(index) = value.as_u64() {
            return Ok(format!("index:{index}"));
        }
        let color = value.as_str().expect("validated string color");
        if color.is_empty() {
            return Ok("default".into());
        }
        if let Some(hex) = color.strip_prefix('#') {
            ensure!(
                matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid Pi hex color"
            );
            return Ok(if hex.len() == 3 {
                let mut full = String::from("#");
                for c in hex.chars() {
                    full.push(c);
                    full.push(c);
                }
                full
            } else {
                color.to_owned()
            });
        }
        if color.to_ascii_lowercase().starts_with("oklch(")
            || color.to_ascii_lowercase().starts_with("okhsl(")
        {
            bail!("Pi perceptual colors require adapter conversion; native JSON accepts hex, ANSI indices and terminal defaults");
        }
        ensure!(
            visited.insert(color),
            "circular Pi theme variable reference"
        );
        value = vars
            .get(color)
            .context("unknown Pi theme variable reference")?;
    }
}

#[cfg(test)]
pub(super) fn fixture(name: &str, accent: &str) -> Value {
    let colors = TOKENS
        .iter()
        .map(|&token| (token.to_owned(), Value::String("primary".into())))
        .collect::<Map<_, _>>();
    serde_json::json!({
        "name": name,
        "appearance": "dark",
        "vars": {"primary": "secondary", "secondary": accent},
        "colors": colors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_projection_resolves_variables_indices_defaults_and_optional_tokens() {
        let mut input = fixture("Pi fixture", "#0af");
        input["colors"]["text"] = Value::String(String::new());
        input["colors"]["border"] = Value::from(255);
        for key in [
            "scrollbarTrack",
            "scrollbarThumb",
            "thinkingMax",
            "searchMatchBg",
            "searchMatchText",
        ] {
            input["colors"].as_object_mut().unwrap().remove(key);
        }
        let source = format!("\u{feff}{}", input);
        let native = native_source(Path::new("fixture.json"), &source).unwrap();
        let parsed: toml::Value = toml::from_str(&native).unwrap();
        assert_eq!(parsed["colors"]["accent"].as_str(), Some("#00aaff"));
        assert_eq!(parsed["colors"]["border"].as_str(), Some("index:255"));
        assert_eq!(parsed["colors"]["foreground"].as_str(), Some("default"));
        assert_eq!(
            parsed["roles"]["extension.pi.scrollbarThumb"]["foreground"].as_str(),
            Some("default")
        );
        assert_eq!(
            parsed["roles"]["extension.pi.searchMatchBg"]["background"].as_str(),
            Some("#00aaff")
        );
        assert!(matches!(
            native_source(Path::new("native.toml"), "[colors]").unwrap(),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn native_projection_rejects_malformed_unsafe_and_unsupported_documents() {
        let mut cases = Vec::new();
        for bad in [
            Value::Null,
            Value::from(-1),
            Value::from(256),
            Value::from(1.5),
            Value::from("#ffff"),
            Value::from("\u{1b}[31m"),
            Value::from("missing"),
            Value::from("oklch(62% 0.1 200)"),
        ] {
            let mut value = fixture("Invalid", "#abcdef");
            value["colors"]["accent"] = bad;
            cases.push(value);
        }
        let mut cycle = fixture("Cycle", "primary");
        cycle["vars"]["secondary"] = Value::from("primary");
        cases.push(cycle);
        let mut unknown = fixture("Unknown", "#abcdef");
        unknown["colors"]["unknown"] = Value::from(1);
        cases.push(unknown);
        let mut missing = fixture("Missing", "#abcdef");
        missing["colors"].as_object_mut().unwrap().remove("accent");
        cases.push(missing);
        let mut bad_export = fixture("Export", "#abcdef");
        bad_export["export"] = serde_json::json!({"pageBg": "missing"});
        cases.push(bad_export);
        for value in cases {
            assert!(
                native_source(Path::new("bad.json"), &value.to_string()).is_err(),
                "{value}"
            );
        }
        assert!(native_source(Path::new("big.json"), &" ".repeat(MAX_BYTES + 1)).is_err());
        assert!(native_source(Path::new("wrong.json"), "[metadata]\nname = 'not JSON'").is_err());
    }
}
