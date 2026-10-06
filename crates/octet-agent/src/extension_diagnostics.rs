//! Bounded structured diagnostics carried by the existing tool-result metadata.
//!
//! Parsing this data grants no filesystem, artifact, or blob authority.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reserved key within tool-result metadata.
pub const METADATA_KEY: &str = "octet_diagnostics_v1";
const MAX_DIAGNOSTICS_BYTES: usize = 64 * 1024;

/// Diagnostic severity, independent of the tool's transport/error envelope.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Operation could not be completed as requested.
    Error,
    /// A result needs attention.
    Warning,
    /// Informational context.
    Info,
    /// A suggested improvement.
    Hint,
}

/// A revision-bound source, not a permission to read it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// Workspace file at an exact content revision.
    Workspace {
        /// Normalized workspace-relative path.
        path: String,
        /// Lowercase SHA-256 of the referenced file bytes.
        revision: String,
    },
    /// Immutable bulk-object identity.
    Blob {
        /// Host-issued blob identity (not a filesystem locator).
        id: String,
    },
    /// Existing immutable artifact identity.
    Artifact {
        /// Host-issued artifact identity.
        id: String,
    },
}

/// Zero-based half-open byte range within a source revision.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    /// Inclusive starting byte.
    pub start_byte: u64,
    /// Exclusive ending byte.
    pub end_byte: u64,
}

/// A location bound to immutable content, never just a line number.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    /// Source identity and revision.
    pub source: Source,
    /// Byte range in that revision.
    pub span: Span,
}

/// Supporting location and explanation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Related {
    /// Explanation of the relationship.
    pub message: String,
    /// Exact supporting location.
    pub location: Location,
}

/// Suggested revision-checked workspace edit; never automatically applied.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    /// Workspace source and bytes to replace.
    pub location: Location,
    /// Replacement UTF-8 text.
    pub replacement: String,
}

/// A bounded group of suggested workspace edits.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fix {
    /// Human-readable fix description.
    pub title: String,
    /// Revision-bound edits.
    pub edits: Vec<Edit>,
}

/// An immutable attachment; does not create or retain an object.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    /// Referenced immutable-object namespace.
    pub kind: AttachmentKind,
    /// Opaque object identity, never a locator.
    pub id: String,
    /// Optional display label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Supported immutable attachment namespaces.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// Bulk object.
    Blob,
    /// Existing media artifact.
    Artifact,
}

/// Closed structured diagnostic, supplemental to existing tool results.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    /// Stable severity.
    pub severity: Severity,
    /// Machine-readable identifier.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Optional revision-bound primary location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<Location>,
    /// Supporting locations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Related>,
    /// Suggested workspace changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<Fix>,
    /// Existing immutable objects, subject to normal access checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

fn display_text(text: &str, max: usize, empty: bool) -> bool {
    (empty || !text.is_empty())
        && text.len() <= max
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

fn opaque_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_graphic())
}

impl Location {
    fn valid(&self) -> bool {
        self.span.start_byte <= self.span.end_byte
            && self.span.end_byte < (1_u64 << 53)
            && match &self.source {
                Source::Workspace { path, revision } => {
                    !path.is_empty()
                        && path.len() <= 4096
                        && !path
                            .chars()
                            .any(|c| c.is_control() || c == '\\' || c == ':')
                        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
                        && revision.len() == 64
                        && revision
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                }
                Source::Blob { id } | Source::Artifact { id } => opaque_id(id),
            }
    }
}

impl Diagnostic {
    fn valid(&self) -> bool {
        self.code
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && self.code.len() <= 128
            && self
                .code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            && display_text(&self.message, 4096, false)
            && self.primary.as_ref().is_none_or(Location::valid)
            && self.related.len() <= 16
            && self
                .related
                .iter()
                .all(|r| display_text(&r.message, 4096, false) && r.location.valid())
            && self.fixes.len() <= 8
            && self.fixes.iter().all(|f| {
                display_text(&f.title, 4096, false)
                    && !f.edits.is_empty()
                    && f.edits.len() <= 16
                    && f.edits.iter().all(|e| {
                        matches!(e.location.source, Source::Workspace { .. })
                            && e.location.valid()
                            && display_text(&e.replacement, 16 * 1024, true)
                    })
            })
            && self.attachments.len() <= 16
            && self.attachments.iter().all(|a| {
                opaque_id(&a.id)
                    && a.label
                        .as_ref()
                        .is_none_or(|s| display_text(s, 4096, false))
            })
    }
}

/// Decode and validate reserved metadata, leaving all other metadata untouched.
///
/// This checks the closed wire shape and bounds only. Reading sources, applying
/// fixes and accessing attachments always require their normal host grants.
pub fn decode_metadata(metadata: &Value) -> Result<Vec<Diagnostic>, String> {
    let Some(value) = metadata.get(METADATA_KEY) else {
        return Ok(Vec::new());
    };
    if value.as_array().is_none_or(|items| items.len() > 32)
        || serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > MAX_DIAGNOSTICS_BYTES
    {
        return Err("diagnostics exceed list or byte bounds".into());
    }
    // Optional means absent, not null. Serde's Option otherwise accepts both.
    if value.as_array().unwrap().iter().any(|v| {
        v.get("primary").is_some_and(Value::is_null)
            || v.get("attachments")
                .and_then(Value::as_array)
                .is_some_and(|a| {
                    a.iter()
                        .any(|item| item.get("label").is_some_and(Value::is_null))
                })
    }) {
        return Err("optional diagnostic fields must be omitted, not null".into());
    }
    let diagnostics: Vec<Diagnostic> =
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    if diagnostics.iter().any(|d| !d.valid()) {
        return Err("invalid diagnostic value or bounds".into());
    }
    Ok(diagnostics)
}

/// Validate the reserved metadata without changing the result envelope.
pub fn validate_metadata(metadata: &Value) -> Result<(), String> {
    decode_metadata(metadata).map(|_| ())
}

/// Bounded plain-text projection of validated diagnostic metadata.
pub fn text_projection(metadata: &Value) -> Result<String, String> {
    let mut text = String::new();
    for diagnostic in decode_metadata(metadata)?.iter().take(8) {
        let severity = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        };
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "{severity}[{}]: {}",
            diagnostic.code,
            diagnostic.message.replace(['\n', '\t'], " ")
        ));
        if text.len() >= 4096 {
            let mut end = 4096;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            break;
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a04_shared_diagnostics_are_closed_and_bounded() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../sdk/conformance/typed-values-v1.json"
        ))
        .unwrap();
        for value in fixture["diagnostics"]["valid"].as_array().unwrap() {
            assert!(
                validate_metadata(&json!({METADATA_KEY: [value]})).is_ok(),
                "{value}"
            );
        }
        for value in fixture["diagnostics"]["invalid"].as_array().unwrap() {
            assert!(
                validate_metadata(&json!({METADATA_KEY: [value]})).is_err(),
                "{value}"
            );
        }
        assert!(validate_metadata(&json!({METADATA_KEY: null})).is_err());
        assert!(validate_metadata(
            &json!({METADATA_KEY: [{"severity":"info","code":"ok","message":"ok","primary":null}]})
        )
        .is_err());
        assert!(validate_metadata(&json!({"pi_details":{"anything":true}})).is_ok());
    }

    #[test]
    fn a04_projection_is_utf8_plain_text_and_bounded() {
        let diagnostic = json!({"severity":"warning","code":"long","message":"界".repeat(1365)});
        let metadata = json!({METADATA_KEY: vec![diagnostic; 9]});
        let projection = text_projection(&metadata).unwrap();
        assert!(projection.len() <= 4096);
        assert!(projection.starts_with("warning[long]: "));
        assert!(validate_metadata(
            &json!({METADATA_KEY: vec![json!({"severity":"info","code":"ok","message":"ok"}); 33]})
        )
        .is_err());
    }
}
