//! Model-declared wire defaults, applied only after canonical validation.

use std::borrow::Cow;

use serde_json::{json, Value};

use crate::catalog::Model;
use crate::declarations::{ThinkingFormat, ThinkingSelection};
use crate::{AiError, ConfigError, ReasoningConfig, Request};

pub(super) fn request_defaults<'a>(
    model: &Model,
    req: &'a Request,
) -> Result<Cow<'a, Request>, AiError> {
    model
        .spec
        .preset
        .validate()
        .map_err(|error| ConfigError::Parse(error.to_string()))?;
    model
        .spec
        .preset
        .validate_protocol(model.spec.protocol)
        .map_err(|error| ConfigError::Parse(error.to_string()))?;
    let mut req = Cow::Borrowed(req);
    if req.temperature.is_none() {
        if let Some(value) = model.spec.preset.sampling_params.get("temperature") {
            req.to_mut().temperature = value.as_f64().map(|v| v as f32);
        }
    }
    if req.stop.is_empty() {
        if let Some(value) = model.spec.preset.sampling_params.get("stop") {
            req.to_mut().stop = if let Some(value) = value.as_str() {
                vec![value.to_owned()]
            } else {
                value.as_array().expect("validated stop array").iter()
                    .map(|value| value.as_str().expect("validated stop string").to_owned()).collect()
            };
        }
    }
    Ok(req)
}

pub(super) fn thinking(model: &Model, req: &Request) -> ThinkingSelection {
    let capability = model.spec.capabilities.reasoning.as_ref();
    let effort = match &req.reasoning {
        ReasoningConfig::Off => Some("off".to_owned()),
        ReasoningConfig::On => Some("on".to_owned()),
        selection => capability.and_then(|c| c.wire_value(selection)),
    };
    ThinkingSelection {
        enabled: req.reasoning != ReasoningConfig::Off,
        effort,
        budget: match req.reasoning {
            ReasoningConfig::Budget(budget) => Some(budget),
            ReasoningConfig::Effort(effort) => capability.and_then(|c| c.budget(effort)),
            _ => None,
        },
        level_map: model.spec.preset.thinking_level_map.clone(),
    }
}

pub(super) fn mapped_effort(model: &Model, selection: &ThinkingSelection) -> Option<String> {
    let raw = selection.effort.as_deref()?;
    match model.spec.preset.thinking_level_map.get(raw) {
        Some(value) => value.clone(),
        None if raw == "on" => None,
        None if raw == "off" => Some("none".into()),
        None => Some(raw.to_owned()),
    }
}

pub(super) fn chat(model: &Model, req: &Request, body: &mut Value) -> Result<(), AiError> {
    let preset = &model.spec.preset;
    if preset.mistral_reasoning.is_some()
        && model.endpoint.runtime.openai_chat_profile != crate::OpenAiChatRuntimeProfile::Mistral
    {
        return Err(ConfigError::InvalidModel(model.spec.id.clone()).into());
    }
    if let Some(priority) = preset.vllm_priority {
        body["priority"] = priority.into();
    }
    let selection = thinking(model, req);
    let effort = mapped_effort(model, &selection);
    if model.spec.capabilities.reasoning.is_some() {
        if let Some(format) = preset.thinking_format {
            let object = body.as_object_mut().expect("Chat request is an object");
            for name in [
                "reasoning",
                "reasoning_effort",
                "thinking",
                "enable_thinking",
                "chat_template_kwargs",
            ] {
                object.remove(name);
            }
            let enabled = selection.enabled;
            let off_omitted = !enabled && preset.thinking_level_map.get("off") == Some(&None);
            match format {
                ThinkingFormat::OpenAi => {
                    if preset.supports_reasoning_effort != Some(false) {
                        if let Some(effort) = &effort {
                            body["reasoning_effort"] = effort.clone().into();
                        }
                    }
                }
                ThinkingFormat::OpenRouter => {
                    if enabled {
                        if let Some(effort) = &effort {
                            body["reasoning"] = json!({"effort": effort});
                        }
                    }
                }
                ThinkingFormat::DeepSeek | ThinkingFormat::Zai => {
                    if format == ThinkingFormat::Zai || !off_omitted {
                        body["thinking"] =
                            json!({"type": if enabled { "enabled" } else { "disabled" }});
                        if format == ThinkingFormat::Zai && enabled {
                            body["thinking"]["clear_thinking"] = false.into();
                        }
                    }
                    if enabled && preset.supports_reasoning_effort != Some(false) {
                        if let Some(effort) = &effort {
                            body["reasoning_effort"] = effort.clone().into();
                        }
                    }
                }
                ThinkingFormat::Together | ThinkingFormat::Qwen => {
                    if format == ThinkingFormat::Together {
                        body["reasoning"] = json!({"enabled": enabled});
                    } else {
                        body["enable_thinking"] = enabled.into();
                    }
                    if enabled && preset.supports_reasoning_effort == Some(true) {
                        if let Some(effort) = &effort {
                            body["reasoning_effort"] = effort.clone().into();
                        }
                    }
                }
                ThinkingFormat::QwenChatTemplate => {
                    body["chat_template_kwargs"] =
                        json!({"enable_thinking": enabled, "preserve_thinking": true});
                }
                ThinkingFormat::StringThinking => {
                    if let Some(effort) = &effort {
                        body["thinking"] = effort.clone().into();
                    }
                }
                ThinkingFormat::AntLing => {
                    if enabled {
                        if let Some(Some(effort)) = selection
                            .effort
                            .as_ref()
                            .and_then(|key| preset.thinking_level_map.get(key))
                        {
                            body["reasoning"] = json!({"effort": effort});
                        }
                    }
                }
                ThinkingFormat::Baseten => {
                    if preset.supports_reasoning_effort == Some(true) {
                        if let Some(effort) = &effort {
                            body["reasoning_effort"] = effort.clone().into();
                        }
                    }
                }
                ThinkingFormat::ChatTemplate => {}
            }
        } else if !preset.thinking_level_map.is_empty() {
            body.as_object_mut()
                .expect("Chat object")
                .remove("reasoning_effort");
            if preset.supports_reasoning_effort != Some(false) {
                if let Some(effort) = &effort {
                    body["reasoning_effort"] = effort.clone().into();
                }
            }
        } else if preset.supports_reasoning_effort == Some(false) {
            body.as_object_mut()
                .expect("Chat object")
                .remove("reasoning_effort");
        }
        for (field, values) in [
            ("chat_template_args", &preset.chat_template_args),
            ("chat_template_kwargs", &preset.chat_template_kwargs),
        ] {
            if let Some(values) = values {
                body.as_object_mut().expect("Chat object").remove(field);
                if let Some(value) = selection.interpolate_chat_template(values) {
                    body[field] = value;
                }
            }
        }
        if let (Some(field), Some(budget)) = (preset.thinking_token_budget_field, selection.budget)
        {
            body[field.field_name()] = budget.into();
        }
    }
    if let Some(profile) = preset.mistral_reasoning {
        body.as_object_mut()
            .expect("Chat request object")
            .remove("reasoning_effort");
        if selection.enabled {
            match profile {
                crate::MistralReasoningProfile::PromptMode => {
                    body["prompt_mode"] = "reasoning".into()
                }
                crate::MistralReasoningProfile::ReasoningEffort => {
                    let mapped = selection
                        .effort
                        .as_ref()
                        .and_then(|level| preset.thinking_level_map.get(level));
                    let value = mapped.map_or(Some("high"), |value| value.as_deref());
                    if let Some(value) = value {
                        if !matches!(value, "high" | "none") {
                            return Err(ConfigError::InvalidModel(model.spec.id.clone()).into());
                        }
                        body["reasoning_effort"] = value.into();
                    }
                }
            }
        }
    }
    sampling(model, req, body)
}

pub(super) fn sampling(model: &Model, req: &Request, body: &mut Value) -> Result<(), AiError> {
    for (name, value) in &model.spec.preset.sampling_params {
        // Named controls were defaulted before canonical validation. Never
        // overwrite their validated request values in the final body merge.
        if !matches!(name.as_str(), "temperature" | "stop") {
            body[name] = value.clone();
        }
    }
    // Sampling restrictions belong to the qualified native route, never to a
    // third-party model merely sharing an OpenAI-looking name.
    let qualified = match model.endpoint.base_url.host_str() {
        Some("api.openai.com") => model.endpoint.id.0 == "openai",
        Some("chatgpt.com" | "chat.openai.com") => {
            model.endpoint.runtime.responses_profile == crate::ResponsesRuntimeProfile::Codex
        }
        _ => false,
    };
    if qualified
        && matches!(
            model.spec.api_name.as_str(),
            "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna"
        )
    {
        let effective = match req
            .responses
            .as_ref()
            .and_then(|options| options.input.as_ref())
        {
            Some(input) => input.effective_reasoning(&req.reasoning)?,
            None => req.reasoning.clone(),
        };
        if effective != ReasoningConfig::Off {
            let invalid_sampling = ["temperature", "top_p", "top_logprobs"]
                .iter()
                .any(|key| body.get(*key).is_some())
                || (model.spec.protocol == crate::Protocol::OpenAiChat
                    && body.get("logprobs").is_some())
                || body
                    .get("include")
                    .and_then(Value::as_array)
                    .is_some_and(|values| {
                        values
                            .iter()
                            .any(|value| value.as_str() == Some("message.output_text.logprobs"))
                    });
            if invalid_sampling {
                return Err(ConfigError::Parse(
                    "GPT-6 reasoning requests do not support sampling or output logprobs controls"
                        .into(),
                )
                .into());
            }
        }
    }
    Ok(())
}
