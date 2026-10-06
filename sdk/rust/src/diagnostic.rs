//! Reserved metadata profile; suggestions and references never grant authority.
use crate::{Deserialize, Error, Serialize};
use serde::Deserializer;

pub const DIAGNOSTICS_METADATA_KEY: &str = "octet_diagnostics_v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}
impl Severity {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Hint => "hint",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub primary: Option<Location>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Related>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<Fix>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}
// Optional fields may be absent, but present null is not a valid location/label.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub source: Source,
    pub span: Span,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    Workspace { path: String, revision: String },
    Blob { id: String },
    Artifact { id: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub start_byte: u64,
    pub end_byte: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Related {
    pub message: String,
    pub location: Location,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fix {
    pub title: String,
    pub edits: Vec<Edit>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub location: Location,
    pub replacement: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Blob,
    Artifact,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub id: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub label: Option<String>,
}

fn require(ok: bool) -> Result<(), Error> {
    if ok {
        Ok(())
    } else {
        Err(Error::invalid("invalid or oversized Diagnostic"))
    }
}
fn text(value: &str, max: usize, empty: bool) -> Result<(), Error> {
    require(
        (empty || !value.is_empty())
            && value.len() <= max
            && !value
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
    )
}
fn id(value: &str) -> Result<(), Error> {
    require(!value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_graphic()))
}
impl Location {
    pub fn validate(&self) -> Result<(), Error> {
        require(
            self.span.start_byte <= self.span.end_byte
                && self.span.end_byte <= crate::values::MAX_INTEGER as u64,
        )?;
        match &self.source {
            Source::Workspace { path, revision } => {
                require(
                    !path.is_empty()
                        && path.len() <= 4096
                        && !path.contains(['\\', ':'])
                        && !path.chars().any(char::is_control)
                        && path.split('/').all(|p| !matches!(p, "" | "." | "..")),
                )?;
                require(
                    revision.len() == 64
                        && revision
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                )
            }
            Source::Blob { id: value } | Source::Artifact { id: value } => id(value),
        }
    }
}
impl Diagnostic {
    pub fn new(severity: Severity, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity,
            code: code.into(),
            message: message.into(),
            primary: None,
            related: vec![],
            fixes: vec![],
            attachments: vec![],
        }
    }
    /// Validate shape-dependent semantic constraints without resolving sources/grants.
    pub fn validate(&self) -> Result<(), Error> {
        require(
            !self.code.is_empty()
                && self.code.len() <= 128
                && self.code.bytes().enumerate().all(|(i, b)| {
                    b.is_ascii_alphabetic() || i > 0 && (b.is_ascii_digit() || b"_.-".contains(&b))
                }),
        )?;
        text(&self.message, 4096, false)?;
        if let Some(location) = &self.primary {
            location.validate()?;
        }
        require(self.related.len() <= 16 && self.fixes.len() <= 8 && self.attachments.len() <= 16)?;
        for related in &self.related {
            text(&related.message, 4096, false)?;
            related.location.validate()?;
        }
        for fix in &self.fixes {
            text(&fix.title, 4096, false)?;
            require(!fix.edits.is_empty() && fix.edits.len() <= 16)?;
            for edit in &fix.edits {
                require(matches!(edit.location.source, Source::Workspace { .. }))?;
                edit.location.validate()?;
                text(&edit.replacement, 16 * 1024, true)?;
            }
        }
        for attachment in &self.attachments {
            id(&attachment.id)?;
            if let Some(label) = &attachment.label {
                text(label, 4096, false)?;
            }
        }
        Ok(())
    }
}

pub(crate) fn wire(diagnostics: &[Diagnostic]) -> Result<(serde_json::Value, String), Error> {
    require(diagnostics.len() <= 32)?;
    for diagnostic in diagnostics {
        diagnostic.validate()?;
    }
    let value = crate::values::encode(diagnostics)?;
    crate::protocol::bounded_value(&value, 64 * 1024)?;
    let mut summary = diagnostics
        .iter()
        .take(8)
        .map(|d| {
            format!(
                "{}[{}]: {}",
                d.severity.label(),
                d.code,
                d.message.replace(['\n', '\t'], " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut end = summary.len().min(4096);
    while !summary.is_char_boundary(end) {
        end -= 1;
    }
    summary.truncate(end);
    Ok((value, summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_diagnostic_profile() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../conformance/typed-values-v1.json")).unwrap();
        for value in fixture["diagnostics"]["valid"].as_array().unwrap() {
            let diagnostic: Diagnostic = serde_json::from_value(value.clone()).unwrap();
            diagnostic.validate().unwrap();
        }
        for value in fixture["diagnostics"]["invalid"].as_array().unwrap() {
            assert!(serde_json::from_value::<Diagnostic>(value.clone())
                .map_err(|_| ())
                .and_then(|d| d.validate().map_err(|_| ()))
                .is_err());
        }
        let value =
            serde_json::json!({"severity":"error","code":"valid","message":"x","primary":null});
        assert!(serde_json::from_value::<Diagnostic>(value).is_err());
    }
    #[test]
    fn diagnostic_summary_is_bounded_plain_utf8() {
        let diagnostic = Diagnostic::new(Severity::Warning, "solver.warn", "é\n\t".repeat(1024));
        let (_, summary) = wire(&vec![diagnostic; 8]).unwrap();
        assert!(summary.len() <= 4096);
        assert!(!summary.contains('\t'));
        assert!(wire(&vec![Diagnostic::new(Severity::Info, "ok", "fine"); 33]).is_err());
        assert!(Diagnostic::new(Severity::Info, "ok", "\u{1b}escape")
            .validate()
            .is_err());
    }
}
