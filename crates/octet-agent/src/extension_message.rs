//! Shared custom-message vocabulary for process hosts, independent of sessions.
use base64::Engine as _;
use octet_ai::UserPart;
use serde::{Deserialize, Serialize};

use crate::session_types::SessionError;

/// Pi custom-message content, retained without joining distinct text blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    /// A single string.
    Text(String),
    /// Ordered text and inline image blocks.
    Parts(Vec<CustomMessagePart>),
}

/// A model-visible text block in a Pi custom message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CustomMessagePart {
    /// Text content.
    Text {
        /// Model-visible text.
        text: String,
    },
    /// Inline image content in Pi's persisted shape.
    Image {
        /// Canonical base64 encoded native image bytes.
        data: String,
        /// Native PNG/JPEG/GIF/WebP MIME type.
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

impl CustomMessagePart {
    /// Decode a bounded inline image without fetching remote media.
    pub fn image(&self) -> Result<Option<octet_ai::ImageMedia>, SessionError> {
        let Self::Image { data, mime_type } = self else {
            return Ok(None);
        };
        if data.len() > 4 * octet_ai::MAX_USER_IMAGE_BYTES.div_ceil(3)
            || !matches!(
                mime_type.as_str(),
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            )
        {
            return Err(SessionError::Limit(
                "custom image exceeds bounds or has unsupported MIME".into(),
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| SessionError::Limit("custom image must be canonical base64".into()))?;
        if bytes.len() > octet_ai::MAX_USER_IMAGE_BYTES
            || base64::engine::general_purpose::STANDARD.encode(&bytes) != *data
        {
            return Err(SessionError::Limit(
                "custom image exceeds bounds or has invalid base64".into(),
            ));
        }
        Ok(Some(octet_ai::ImageMedia {
            preparation: Default::default(),
            source: octet_ai::ImageSource::Inline(bytes.into()),
            media_type: Some(mime_type.parse().expect("validated image MIME")),
            detail: None,
        }))
    }
}

impl CustomMessageContent {
    /// Validate the complete text/media batch at message ingestion.
    pub fn validate(&self) -> Result<(), SessionError> {
        let text = self.text();
        if text.len() > 256 * 1024
            || text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(SessionError::Limit(
                "message text exceeds bounds or contains controls".into(),
            ));
        }
        if let Self::Parts(parts) = self {
            if parts.len() > 256 {
                return Err(SessionError::Limit("too many message parts".into()));
            }
            let (mut count, mut bytes) = (0usize, 0usize);
            for part in parts {
                if let Some(image) = part.image()? {
                    count += 1;
                    if let octet_ai::ImageSource::Inline(data) = image.source {
                        bytes += data.len();
                    }
                }
            }
            if count > 8 || bytes > 20 * 1024 * 1024 {
                return Err(SessionError::Limit(
                    "message image batch exceeds bounds".into(),
                ));
            }
        }
        Ok(())
    }

    /// Native input projection, preserving ordered text and image parts.
    pub fn input_parts(&self) -> Vec<crate::InputPart> {
        match self {
            Self::Text(text) => vec![crate::InputPart::Text(text.clone())],
            Self::Parts(parts) => parts
                .iter()
                .map(|part| match part {
                    CustomMessagePart::Text { text } => crate::InputPart::Text(text.clone()),
                    CustomMessagePart::Image { .. } => {
                        crate::InputPart::Media(octet_ai::Media::Image(
                            part.image()
                                .expect("validated custom image")
                                .expect("image variant"),
                        ))
                    }
                })
                .collect(),
        }
    }

    /// Payload-free display summary.
    pub fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Parts(parts) => parts
                .iter()
                .map(|part| match part {
                    CustomMessagePart::Text { text } => text.as_str(),
                    CustomMessagePart::Image { .. } => "[image]",
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// Typed durable Pi custom-message data. The canonical body is user content;
/// these fields remain outside provider context, including `details`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomMessage {
    /// Extension-defined message type.
    pub custom_type: String,
    /// Original string or ordered content blocks.
    pub content: CustomMessageContent,
    /// Whether this message appears in the transcript.
    pub display: bool,
    /// Inert extension data, never sent to the model. Explicit null is retained.
    #[serde(
        default,
        deserialize_with = "deserialize_custom_message_details",
        skip_serializing_if = "Option::is_none"
    )]
    pub details: Option<serde_json::Value>,
}

pub(crate) fn deserialize_custom_message_details<'de, D>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

impl CustomMessage {
    /// Validate bounded inert message data at the host ingestion boundary.
    pub fn validate(&self) -> Result<(), SessionError> {
        self.content.validate()?;
        if self.custom_type.len() > 128
            || self.custom_type.chars().any(char::is_control)
            || self.details.as_ref().is_some_and(|value| {
                serde_json::to_vec(value).map_or(true, |encoded| encoded.len() > 64 * 1024)
            })
        {
            return Err(SessionError::Limit(
                "custom message exceeds bounds or contains controls".into(),
            ));
        }
        Ok(())
    }

    /// Canonical model projection; display and details have no effect on it.
    pub fn user_parts(&self) -> Vec<UserPart> {
        crate::UserInput::from(self.content.input_parts()).into_user_parts()
    }

    /// Human-readable content summary.
    pub fn text(&self) -> String {
        self.content.text()
    }
}

impl CustomMessage {
    /// Pi message projection for a real durable lifecycle boundary.
    pub fn lifecycle_value(&self, timestamp_unix_ms: u64) -> serde_json::Value {
        let mut value = serde_json::json!({"role":"custom", "customType":self.custom_type,
            "content":self.content, "display":self.display, "timestamp":timestamp_unix_ms});
        if let Some(details) = &self.details {
            value["details"] = details.clone();
        }
        value
    }
}
