//! Pure decoding of endpoint assertions; no discovery or environment access.
#![allow(missing_docs)]
use crate::{ModalitySet, OpenAiChatReasoningMode, Protocol, ReasoningConfig, ReasoningControl};
use anyhow::Context;

#[derive(Clone, Debug)]
pub struct DiscoveredReasoning {
    pub source: crate::types::ReasoningMetadataSource,
    pub supported: Option<bool>,
    pub control: Option<ReasoningControl>,
    pub options: Option<crate::types::ReasoningOptions>,
    pub profile: Option<OpenAiChatReasoningMode>,
    /// The endpoint published the enabled effort levels it accepts without
    /// saying anything about disabling thinking (DeepSeek's `effort` object).
    /// A declaration that owns an Off wire control keeps that choice.
    pub enabled_effort_levels: bool,
}

pub fn decode_reasoning_metadata(entry: &serde_json::Value) -> anyhow::Result<DiscoveredReasoning> {
    use crate::types::{ReasoningMetadataSource as Source, ReasoningOptions};
    let mut fields = Vec::new();
    for metadata in [
        Some(entry),
        entry.get("top_provider"),
        entry.get("provider"),
    ]
    .into_iter()
    .flatten()
    {
        for name in ["reasoning", "supports_reasoning", "reasoning_effort"] {
            if let Some(value) = metadata.get(name) {
                fields.push(value);
            }
            if let Some(value) = metadata.get("capabilities").and_then(|c| c.get(name)) {
                fields.push(value);
            }
        }
    }
    let mut result = DiscoveredReasoning {
        source: Source::Absent,
        supported: None,
        control: None,
        options: None,
        profile: None,
        enabled_effort_levels: false,
    };
    if fields
        .iter()
        .any(|v| metadata_capability_flag(v) == Some(false))
    {
        result.source = Source::Explicit;
        result.supported = Some(false);
        return Ok(result);
    }
    for field in fields {
        result.source = Source::Unknown;
        if field.is_null() {
            continue;
        }
        anyhow::ensure!(
            field.is_boolean() || field.is_object(),
            "malformed reasoning metadata"
        );
        if let Some(supported) = field.get("supported") {
            anyhow::ensure!(
                supported.is_boolean(),
                "malformed reasoning supported assertion"
            );
        }
        if let Some(supported) = metadata_capability_flag(field) {
            result.supported = Some(supported);
            result.source = Source::Explicit;
        }
        if let Some(control) = field.get("control") {
            let control = match control.as_str() {
                Some("effort" | "levels") => ReasoningControl::Effort,
                Some("toggle" | "binary") => ReasoningControl::Toggle,
                Some("always_on") => ReasoningControl::AlwaysOn,
                Some("token_budget") => ReasoningControl::TokenBudget,
                Some(_) => {
                    result.source = Source::Unknown;
                    result.supported = None;
                    return Ok(result);
                }
                None => anyhow::bail!("malformed reasoning control"),
            };
            result.control = Some(control);
        }
        if let Some(values) = field.get("values") {
            result.options = Some(decode_reasoning_options(values, field.get("default"))?);
        } else if let Some(default) = field.get("default") {
            // Some inventories publish the accepted levels as `effort` alongside
            // a structured default ({"type": "effort", "value": ...}). Without a
            // declared level list there is nothing exact to select, so keep
            // failing closed instead of inventing a range.
            let values = field
                .get("effort")
                .ok_or_else(|| anyhow::anyhow!("reasoning default requires exact values"))?;
            result.options = Some(decode_reasoning_options(values, Some(default))?);
        }
        if let Some(profile) = field.get("profile") {
            result.profile =
                Some(serde_json::from_value(profile.clone()).context("invalid reasoning profile")?);
        }
    }
    if let Some(values) = entry
        .get("supported_reasoning_levels")
        .or_else(|| entry.get("supported_reasoning_efforts"))
    {
        result.options = Some(decode_reasoning_options(
            values,
            entry
                .get("default_reasoning_level")
                .or_else(|| entry.get("default_reasoning_effort")),
        )?);
        result.source = Source::Explicit;
        result.supported = Some(true);
        result.control = Some(ReasoningControl::Effort);
    }
    if let Some(options) = entry.get("reasoning_options") {
        let options = options
            .as_array()
            .filter(|o| o.len() <= 3)
            .ok_or_else(|| anyhow::anyhow!("malformed reasoning options"))?;
        for option in options {
            match option.get("type").and_then(serde_json::Value::as_str) {
                Some("effort") => {
                    anyhow::ensure!(
                        result.options.is_none(),
                        "conflicting reasoning option sources"
                    );
                    result.options = Some(decode_reasoning_options(
                        option
                            .get("values")
                            .ok_or_else(|| anyhow::anyhow!("missing effort values"))?,
                        option.get("default"),
                    )?);
                    result.control = Some(ReasoningControl::Effort);
                }
                Some("toggle" | "budget_tokens") => {
                    if result.options.is_none() {
                        result.source = Source::Unknown;
                    }
                }
                _ => anyhow::bail!("malformed reasoning options"),
            }
        }
    }
    // DeepSeek publishes its effort contract as `effort`: the endpoint declares
    // which enabled levels it accepts and which one it uses unless asked. The
    // levels describe enabled effort only; an Off control stays declaration-owned
    // (`thinking.type`), exactly as the pinned contract already states.
    if result.options.is_none() && result.source != Source::Unknown {
        if let Some(effort) = entry.get("effort") {
            if let Some(levels) = effort.get("supported_levels") {
                result.options = Some(decode_reasoning_options(
                    levels,
                    effort.get("default_level"),
                )?);
                result.control = Some(ReasoningControl::Effort);
                result.supported = Some(true);
                result.source = Source::Explicit;
                result.enabled_effort_levels = true;
            }
        }
    }
    if result.options.is_none() {
        if let Some(parameters) = entry
            .get("supported_parameters")
            .and_then(serde_json::Value::as_array)
        {
            if parameters.iter().any(|p| {
                matches!(
                    p.as_str(),
                    Some("reasoning_effort" | "reasoning.effort" | "reasoning")
                )
            }) {
                result.supported = Some(true);
                result.source = Source::Explicit;
                if parameters
                    .iter()
                    .any(|p| matches!(p.as_str(), Some("reasoning_effort" | "reasoning.effort")))
                {
                    result.control = Some(ReasoningControl::Effort);
                }
            }
        }
    }
    if let Some(options) = &result.options {
        let choices = options.choices();
        let has_effort = choices
            .iter()
            .any(|v| matches!(v, ReasoningConfig::Effort(_)));
        let inferred = if result.control == Some(ReasoningControl::AlwaysOn) {
            anyhow::ensure!(
                choices.iter().all(|choice| *choice == ReasoningConfig::On),
                "always-on reasoning requires only On choices"
            );
            ReasoningControl::AlwaysOn
        } else if has_effort {
            ReasoningControl::Effort
        } else {
            ReasoningControl::Toggle
        };
        anyhow::ensure!(
            result.control.is_none_or(|c| c == inferred),
            "reasoning control disagrees with exact values"
        );
        result.control = Some(inferred);
        result.supported = Some(choices.iter().any(|v| *v != ReasoningConfig::Off));
        result.source = Source::Explicit;
    }
    // Binary controls are semantic, not an effort range.
    if result.control == Some(ReasoningControl::Toggle) && result.options.is_none() {
        result.options = Some(ReasoningOptions {
            values: vec!["false".into(), "true".into()],
            default: None,
        });
    }
    Ok(result)
}

pub fn decode_reasoning_options(
    values: &serde_json::Value,
    default: Option<&serde_json::Value>,
) -> anyhow::Result<crate::types::ReasoningOptions> {
    let values = values
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 9)
        .ok_or_else(|| anyhow::anyhow!("invalid reasoning values"))?;
    let values = values
        .iter()
        .map(|v| {
            v.as_str().or_else(|| {
                v.get("effort")
                    .or_else(|| v.get("value"))
                    .and_then(serde_json::Value::as_str)
            })
        })
        .map(|v| {
            v.map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("invalid reasoning value"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let default = default
        .filter(|d| !d.is_null())
        .map(|d| {
            // A structured default names its effort in `value`; a plain string
            // default is the same claim in the shorter form.
            d.as_str()
                .or_else(|| d.get("value").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("invalid reasoning default"))
        })
        .transpose()?;
    let options = crate::types::ReasoningOptions { values, default };
    anyhow::ensure!(
        options.is_valid(),
        "unknown, duplicate or inconsistent reasoning values/default"
    );
    Ok(options)
}

pub fn discovered_display_name(entry: &serde_json::Value, id: &str) -> Option<String> {
    ["display_name", "name"]
        .into_iter()
        .filter_map(|key| entry.get(key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .find(|name| !name.is_empty() && *name != id)
        .map(str::to_owned)
}

pub fn has_metadata_assertion(entry: &serde_json::Value, names: &[&str]) -> bool {
    [
        Some(entry),
        entry.get("provider"),
        entry.get("top_provider"),
    ]
    .into_iter()
    .flatten()
    .any(|metadata| {
        let asserted = |object: &serde_json::Value, name: &str| {
            if let Some((container, field)) = name.split_once('/') {
                object
                    .get(container)
                    .is_some_and(|value| !value.is_object() || value.get(field).is_some())
            } else {
                object.get(name).is_some()
            }
        };
        names.iter().any(|name| asserted(metadata, name))
            || metadata.get("capabilities").is_some_and(|caps| {
                !caps.is_object() || names.iter().any(|name| asserted(caps, name))
            })
    })
}

pub fn metadata_capability_flag(value: &serde_json::Value) -> Option<bool> {
    value
        .as_bool()
        .or_else(|| value.get("supported").and_then(serde_json::Value::as_bool))
}

pub fn asserted_capability(entry: &serde_json::Value, names: &[&str]) -> Option<bool> {
    let mut supported = None;
    for metadata in [
        Some(entry),
        entry.get("top_provider"),
        entry.get("provider"),
    ]
    .into_iter()
    .flatten()
    {
        for object in [Some(metadata), metadata.get("capabilities")]
            .into_iter()
            .flatten()
        {
            for name in names {
                if let Some(value) = object.get(*name) {
                    match metadata_capability_flag(value) {
                        Some(true) => supported = Some(true),
                        // Unknown/malformed explicit flags are not positive support.
                        Some(false) | None => return Some(false),
                    }
                }
            }
        }
    }
    supported
}

pub fn input_modalities_from_entry(entry: &serde_json::Value) -> ModalitySet {
    modalities_from_entry(entry, "input")
}

pub fn output_modalities_from_entry(entry: &serde_json::Value) -> ModalitySet {
    modalities_from_entry(entry, "output")
}

pub fn modalities_from_entry(entry: &serde_json::Value, direction: &str) -> ModalitySet {
    let field = format!("{direction}_modalities");
    let values = [
        Some(entry),
        entry.get("top_provider"),
        entry.get("provider"),
    ]
    .into_iter()
    .flatten()
    .flat_map(|metadata| [Some(metadata), metadata.get("capabilities")])
    .flatten()
    .find_map(|metadata| {
        metadata
            .get("architecture")
            .and_then(|architecture| {
                if architecture.is_object() {
                    architecture.get(&field)
                } else {
                    Some(architecture)
                }
            })
            .or_else(|| metadata.get(&field))
            .or_else(|| {
                metadata.get("modalities").and_then(|modalities| {
                    if modalities.is_object() {
                        modalities.get(direction)
                    } else if direction == "input" {
                        Some(modalities)
                    } else {
                        None
                    }
                })
            })
    })
    .and_then(serde_json::Value::as_array)
    .filter(|values| values.iter().all(serde_json::Value::is_string));
    let mut result = ModalitySet::none();
    for value in values.into_iter().flatten() {
        match value.as_str().unwrap().to_ascii_lowercase().as_str() {
            "image" | "vision" => result = result.with(crate::Modality::Image),
            "audio" => result = result.with(crate::Modality::Audio),
            // OpenCode inventories advertise concrete input MIME types. Recognize
            // the formats we can send; an arbitrary audio/* is not evidence.
            "audio/wav" | "audio/x-wav" | "audio/mp3" | "audio/mpeg" | "audio/aac"
            | "audio/ogg" | "audio/opus" | "audio/flac"
                if direction == "input" =>
            {
                result = result.with(crate::Modality::Audio);
            }
            "image/png" | "image/jpeg" | "image/gif" | "image/webp" if direction == "input" => {
                result = result.with(crate::Modality::Image);
            }
            // models.dev says `pdf`; OpenRouter's architecture says `file`.
            "pdf" | "file" | "application/pdf" if direction == "input" => {
                result = result.with(crate::Modality::Pdf);
            }
            _ => {}
        }
    }
    result
}

pub fn conversation_output_modalities(protocol: Protocol, advertised: ModalitySet) -> ModalitySet {
    if protocol == Protocol::OpenAiChat && advertised.contains(crate::Modality::Audio) {
        ModalitySet::none().with(crate::Modality::Audio)
    } else {
        ModalitySet::none()
    }
}
