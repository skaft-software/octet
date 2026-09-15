//! Protocol-owned request DTOs, strict JSON decoding, and boundary identifiers.
//!
//! This module has no transport or agent orchestration policy. It owns the
//! versioned wire shapes and the rejection rules that make decoding strict.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ID_BYTES: usize = 128;
pub(crate) const MAX_PROMPT_BYTES: usize = 512 * 1024;
pub(crate) const MAX_PROMPT_DISPLAY_BYTES: usize = 256 * 1024;
pub(crate) const MAX_EVENT_TEXT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_HISTORY_MESSAGES: usize = 256;
pub(crate) const MAX_HISTORY_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_MEDIA_COUNT: usize = 12;
pub(crate) const MAX_IMAGE_COUNT: usize = 8;
pub(crate) const MAX_AUDIO_COUNT: usize = 4;
pub(crate) const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
pub(crate) const MAX_AUDIO_BYTES: u64 = 20 * 1024 * 1024;
pub(crate) const MAX_TOTAL_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
pub(crate) const MAX_TOTAL_AUDIO_BYTES: u64 = 40 * 1024 * 1024;
pub(crate) const MAX_CUSTOM_HEADERS: usize = 64;
pub(crate) const MAX_CUSTOM_HEADER_BYTES: usize = 64 * 1024;
pub(crate) const MAX_API_KEY_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub(crate) struct HostRequest {
    pub(crate) protocol_version: u16,
    pub(crate) request_id: String,
    #[serde(flatten)]
    pub(crate) command: HostCommand,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum HostCommand {
    Hello,
    Models {
        #[serde(default)]
        offline: bool,
    },
    Run(Box<RunRequest>),
    Shutdown,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RunRequest {
    pub(crate) run_id: String,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    pub(crate) workspace: PathBuf,
    #[serde(default)]
    pub(crate) working_dir: Option<PathBuf>,
    #[serde(default)]
    pub(crate) session_dir: Option<PathBuf>,
    #[serde(default)]
    pub(crate) resume_session: Option<PathBuf>,
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) provider: Option<String>,
    #[serde(default)]
    pub(crate) base_url: Option<String>,
    #[serde(default)]
    pub(crate) api_key: Option<String>,
    #[serde(default)]
    pub(crate) custom_headers: HashMap<String, String>,
    #[serde(default)]
    pub(crate) provider_mode: Option<String>,
    #[serde(default)]
    pub(crate) context_window_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) max_output_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) vision: bool,
    #[serde(default)]
    pub(crate) input_modalities: Vec<HostInputModality>,
    #[serde(default)]
    pub(crate) supports_reasoning: bool,
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) prompt_display_text: Option<String>,
    #[serde(default)]
    pub(crate) system_prompt: Option<String>,
    #[serde(default)]
    pub(crate) reasoning: Option<String>,
    #[serde(default)]
    pub(crate) tools: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub(crate) allow_file_mutation: bool,
    #[serde(default)]
    pub(crate) allow_external_paths: bool,
    #[serde(default = "default_true")]
    pub(crate) context_files: bool,
    #[serde(default)]
    pub(crate) offline: bool,
    #[serde(default)]
    pub(crate) max_turns: Option<u64>,
    #[serde(default)]
    pub(crate) max_cost_microdollars: Option<u64>,
    #[serde(default)]
    pub(crate) history: Vec<SeedMessage>,
    #[serde(default)]
    pub(crate) media: Vec<MediaInput>,
    #[serde(default)]
    pub(crate) image_paths: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) prompt_paths: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) skill_paths: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) extension_paths: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) enabled_extensions: Vec<String>,
    #[serde(default)]
    pub(crate) trusted_extensions: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostInputModality {
    Image,
    Audio,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MediaInput {
    Image { path: PathBuf },
    Audio { path: PathBuf },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeedMessage {
    pub(crate) role: SeedRole,
    pub(crate) text: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SeedRole {
    User,
    Assistant,
}

fn default_true() -> bool {
    true
}

struct StrictJsonValue;

impl<'de> DeserializeSeed<'de> for StrictJsonValue {
    type Value = serde_json::Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictJsonValue {
    type Value = serde_json::Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a strict JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if !value.is_finite() {
            return Err(E::custom("non-finite JSON number"));
        }
        serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictJsonValue)? {
            values.push(value);
        }
        Ok(serde_json::Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON field {key:?}"
                )));
            }
            values.insert(key, map.next_value_seed(StrictJsonValue)?);
        }
        Ok(serde_json::Value::Object(values))
    }
}

fn parse_strict_json(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictJsonValue
        .deserialize(&mut deserializer)
        .map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())?;
    Ok(value)
}

pub(crate) fn parse_request(bytes: &[u8]) -> Result<HostRequest, String> {
    let value = parse_strict_json(bytes)?;
    let object = value
        .as_object()
        .ok_or_else(|| "request must be a JSON object".to_owned())?;
    let command = object
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if let Some(field) = object
        .keys()
        .find(|field| !known_request_field(command, field))
    {
        return Err(format!("unknown request field {field:?}"));
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn known_request_field(command: &str, field: &str) -> bool {
    if matches!(field, "protocol_version" | "request_id" | "command") {
        return true;
    }
    match command {
        "hello" | "shutdown" => false,
        "models" => field == "offline",
        "run" => matches!(
            field,
            "run_id"
                | "session_id"
                | "workspace"
                | "working_dir"
                | "session_dir"
                | "resume_session"
                | "model"
                | "provider"
                | "base_url"
                | "api_key"
                | "custom_headers"
                | "provider_mode"
                | "context_window_tokens"
                | "max_output_tokens"
                | "vision"
                | "input_modalities"
                | "supports_reasoning"
                | "prompt"
                | "prompt_display_text"
                | "system_prompt"
                | "reasoning"
                | "tools"
                | "allow_file_mutation"
                | "allow_external_paths"
                | "context_files"
                | "offline"
                | "max_turns"
                | "max_cost_microdollars"
                | "history"
                | "media"
                | "image_paths"
                | "prompt_paths"
                | "skill_paths"
                | "extension_paths"
                | "enabled_extensions"
                | "trusted_extensions"
        ),
        // Let the tagged-enum deserializer produce the canonical unknown-command
        // diagnostic rather than misclassifying its accompanying fields.
        _ => true,
    }
}
pub(crate) fn valid_protocol_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[cfg(test)]
mod tests;
