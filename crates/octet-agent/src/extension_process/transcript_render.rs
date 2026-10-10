//! Bounded, presentation-only transcript rendering. No result is session content.

use super::{ExtensionExecutionContext, ExtensionResourceOwner};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Frontend-bound API 0.4 transcript presentation profile.
pub const EXTENSION_FEATURE_TRANSCRIPT_RENDER: &str = "transcript_render_v1";
/// Maximum source or transformed Markdown bytes.
pub const MAX_TRANSCRIPT_MARKDOWN_BYTES: usize = 262_144;

/// One semantic source, separate from its cached presentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)]
pub enum TranscriptRenderContent {
    /// A visible custom message, retaining its original content and details.
    Message {
        message: Value,
        expanded: bool,
        output_pad: u16,
    },
    /// A private entry sent only to its registered namespace.
    Entry { entry: Value, expanded: bool },
    /// A Markdown source, never a replacement for canonical model text.
    Markdown {
        text: String,
        message_type: String,
        is_streaming: bool,
    },
    /// Full lifecycle rendering, including partial results and non-model details.
    Tool {
        name: String,
        tool_call_id: String,
        arguments: Value,
        result: Option<Value>,
        expanded: bool,
        is_partial: bool,
        is_error: bool,
        execution_started: bool,
        args_complete: bool,
        show_images: bool,
    },
}

/// Host request; source identity is stable across resize/disclosure, not an ordinal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptRenderRequest {
    /// Host-owned presentation source identity.
    pub source_id: String,
    /// Actual content rectangle width, in terminal cells.
    pub width: u16,
    /// Canonical source snapshot.
    pub render: TranscriptRenderContent,
    /// Exact session/process owner, never silently retargeted to a new generation.
    pub context: ExtensionExecutionContext,
}

impl TranscriptRenderRequest {
    /// Checks the bounded adapter profile before dispatch.
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0
            || self.source_id.is_empty()
            || self.source_id.len() > 256
            || self.source_id.chars().any(char::is_control)
            || self.context.resource_owner.is_none()
        {
            return Err("invalid transcript source, width or owner".into());
        }
        let limit = match &self.render {
            TranscriptRenderContent::Markdown {
                text, message_type, ..
            } => {
                if text.len() > MAX_TRANSCRIPT_MARKDOWN_BYTES
                    || !matches!(
                        message_type.as_str(),
                        "user" | "assistant" | "assistant-thinking"
                    )
                {
                    return Err("invalid transcript Markdown source".into());
                }
                786_432
            }
            TranscriptRenderContent::Tool {
                name,
                tool_call_id,
                arguments,
                result,
                ..
            } => {
                if name.is_empty()
                    || name.len() > 128
                    || tool_call_id.is_empty()
                    || tool_call_id.len() > 256
                    || name.chars().any(char::is_control)
                    || tool_call_id.chars().any(char::is_control)
                    || encoded_bytes(arguments)? > 262_144
                    || result.as_ref().map(encoded_bytes).transpose()?.unwrap_or(0) > 524_288
                {
                    return Err("invalid transcript tool source".into());
                }
                900_000
            }
            TranscriptRenderContent::Message { message, .. }
            | TranscriptRenderContent::Entry { entry: message, .. } => {
                let custom = message
                    .get("customType")
                    .and_then(Value::as_str)
                    .ok_or("missing transcript customType")?;
                if custom.is_empty() || custom.len() > 128 || custom.chars().any(char::is_control) {
                    return Err("invalid transcript customType".into());
                }
                786_432
            }
        };
        if encoded_bytes(&self.render)? > limit {
            return Err("transcript source exceeds byte bound".into());
        }
        Ok(())
    }
}

fn encoded_bytes(value: &impl Serialize) -> Result<usize, String> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| error.to_string())
}

/// Validated immutable presentation. Null lines mean use the host fallback;
/// an empty array is a deliberate empty rendering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptRenderResponse {
    /// Whether a renderer claimed the source.
    pub registered: bool,
    /// Printable lines with only bounded safe SGR.
    pub lines: Option<Vec<String>>,
    /// Transformed Markdown, only for the Markdown request kind.
    pub markdown: Option<String>,
    /// Tool framing policy. Non-tool responses omit this field.
    #[serde(default)]
    pub render_shell: Option<String>,
}

impl TranscriptRenderResponse {
    /// Validates the response against the requested kind before any cache write.
    pub fn validate(&self, request: &TranscriptRenderContent) -> Result<(), String> {
        if let Some(lines) = &self.lines {
            if lines.len() > 256
                || lines.iter().any(|line| line.len() > 16_384)
                || lines.iter().map(String::len).sum::<usize>() > 524_288
            {
                return Err("transcript frame exceeds byte/row bound".into());
            }
            for line in lines {
                crate::extension_remote_ui::validate_remote_ui_line(line)?;
            }
        }
        if let Some(markdown) = &self.markdown {
            if markdown.len() > MAX_TRANSCRIPT_MARKDOWN_BYTES {
                return Err("transcript Markdown exceeds byte bound".into());
            }
            for line in markdown.split('\n') {
                if line.len() > 16_384 {
                    return Err("transcript Markdown line exceeds byte bound".into());
                }
                crate::extension_remote_ui::validate_remote_ui_line(&line.replace('\t', " "))?;
            }
        }
        if self.lines.is_some() && self.markdown.is_some()
            || (!matches!(request, TranscriptRenderContent::Markdown { .. })
                && self.markdown.is_some())
            || (matches!(request, TranscriptRenderContent::Markdown { .. }) && self.lines.is_some())
            || (self.render_shell.is_some()
                && !matches!(request, TranscriptRenderContent::Tool { .. }))
            || self
                .render_shell
                .as_deref()
                .is_some_and(|shell| !matches!(shell, "self" | "default"))
            || (!self.registered && self.lines.is_some())
        {
            return Err("transcript response does not match request kind".into());
        }
        Ok(())
    }

    /// Retained presentation byte budget (source is accounted separately).
    pub fn bytes(&self) -> usize {
        self.lines
            .as_ref()
            .map_or(0, |lines| lines.iter().map(String::len).sum())
            + self.markdown.as_ref().map_or(0, String::len)
    }
}

/// Project only one namespace's private entry. No unrelated metadata is disclosed.
pub fn transcript_private_entry(entry: &crate::session::Entry, namespace: &str) -> Option<Value> {
    let payload: crate::session::ExtensionEntry = serde_json::from_value(
        entry
            .metadata
            .as_ref()?
            .extension_metadata
            .get(namespace)?
            .value
            .clone(),
    )
    .ok()?;
    let mut value = serde_json::json!({"type":"custom", "id":entry.id.0,
        "parentId":entry.parent.as_ref().map(|id| &id.0), "customType":payload.entry_type, "data":payload.data});
    if let Some(timestamp) = entry.timestamp_unix_ms {
        value["timestamp"] = timestamp.into();
    }
    Some(value)
}

/// Coalescible invalidation, fenced to an issued foreground owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptInvalidation {
    /// Complete issued owner.
    pub resource_owner: ExtensionResourceOwner,
    /// None invalidates the renderer catalog; otherwise one stable source.
    pub source_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> TranscriptRenderContent {
        TranscriptRenderContent::Markdown {
            text: "source".into(),
            message_type: "assistant".into(),
            is_streaming: false,
        }
    }
    #[test]
    fn transcript_render_rejects_terminal_controls_and_mismatched_kinds() {
        let mut reply = TranscriptRenderResponse {
            registered: true,
            lines: None,
            markdown: Some("\x1b[31mtext\x1b[0m".into()),
            render_shell: None,
        };
        assert!(reply.validate(&source()).is_ok());
        for bad in ["\x1b]2;title\x07", "\x1b[2J", "\x1b[38;2;256;0;0m", "\r"] {
            reply.markdown = Some(bad.into());
            assert!(reply.validate(&source()).is_err());
        }
        reply.markdown = None;
        reply.lines = Some(vec!["safe".into()]);
        assert!(reply.validate(&source()).is_err());
        let tool = TranscriptRenderContent::Entry {
            entry: serde_json::json!({"customType":"x"}),
            expanded: false,
        };
        assert!(reply.validate(&tool).is_ok());
        reply.lines = Some(vec![String::new(); 257]);
        assert!(reply.validate(&tool).is_err());
    }
}
