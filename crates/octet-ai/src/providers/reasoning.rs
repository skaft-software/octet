//! Declaration-owned reasoning contracts, independent of frontend registration.
#![allow(missing_docs)]
use super::contract::ProviderDeclaration;
use super::models::{static_models, StaticModelPreset};
use crate::{ModalitySet, Protocol, ReasoningCapability, ReasoningControl};

/// Providers outside the OpenCode compatibility family whose inventories
/// publish identifiers only, leaving their pinned records as the reviewed
/// reasoning contract.
const PINNED_REASONING_PROVIDERS: &[&str] = &["zai-coding-cn"];

impl ProviderDeclaration {
    /// A declaration-owned wire contract may supplement a genuinely discovered
    /// sparse route. This is not a models.dev-derived encoding or inventory.
    pub fn static_reasoning_for(
        &self,
        api_name: &str,
        protocol: Protocol,
    ) -> Option<ReasoningCapability> {
        static_models(self.static_models)
            .iter()
            .find(|model| model.id == api_name && model.protocol == protocol)
            .and_then(static_reasoning_capability)
    }

    /// Reasoning for an OpenCode route that has no checked-in preset.
    ///
    /// OpenCode's `/models` inventory publishes identifiers only, so every
    /// model without a hand-written preset used to register with thinking
    /// `off`. The pinned, provider-scoped models.dev record states whether the
    /// model reasons and which effort levels it accepts; this maps that record
    /// onto the declared route's own wire primitive (Gemini `thinkingLevel`,
    /// Anthropic adaptive or budget thinking, Chat/Responses effort). Records
    /// that cannot be mapped exactly leave the model without a capability.
    ///
    /// The Z.AI coding endpoint publishes the same identifiers-only inventory
    /// (its `/models` entries carry no reasoning fields for any GLM release),
    /// so its models fail the same way. The provider-scoped pinned record
    /// carries the reviewed contract instead: GLM-5.3 accepts
    /// `reasoning_effort` low/high/max and always reasons
    /// (docs.z.ai/guides/llm/glm-5.3), so the pinned fallback is keyed to
    /// that host here rather than in the agent loop.
    pub fn pinned_reasoning_for(
        &self,
        api_name: &str,
        protocol: Protocol,
    ) -> Option<ReasoningCapability> {
        if self.compatibility != super::contract::CompatibilityProfile::OpenCode
            && !PINNED_REASONING_PROVIDERS.contains(&self.id)
        {
            return None;
        }
        let record = crate::model_metadata::model_capability_metadata(self.id, api_name)?;
        // A budget table is sized from the documented output ceiling; the
        // registration path still drops one that does not fit the route.
        let max_output_tokens = record
            .get("limit")
            .and_then(|limit| limit.get("output"))
            .and_then(serde_json::Value::as_u64)
            .filter(|output| *output > 0)
            .unwrap_or(32_768);
        pinned_reasoning_capability(&record, api_name, protocol, max_output_tokens)
    }

    /// Image input the pinned record documents for an OpenCode model.
    pub fn pinned_input_modalities(&self, api_name: &str) -> ModalitySet {
        if self.compatibility != super::contract::CompatibilityProfile::OpenCode {
            return ModalitySet::none();
        }
        let Some(record) = crate::model_metadata::model_capability_metadata(self.id, api_name)
        else {
            return ModalitySet::none();
        };
        let inputs = record
            .get("modalities")
            .and_then(|modalities| modalities.get("input"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        let mut result = ModalitySet::none();
        if inputs.contains(&"image") {
            result = result.with(crate::Modality::Image);
        }
        result
    }
}

pub fn pinned_reasoning_capability(
    record: &serde_json::Value,
    api_name: &str,
    protocol: Protocol,
    max_output_tokens: u64,
) -> Option<ReasoningCapability> {
    use super::models::{StaticOpenAiChatReasoningProfile as Profile, StaticReasoningMode as Mode};
    use crate::{types::ReasoningOptions, ReasoningConfig, ReasoningEffort as E};
    if record.get("reasoning").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let options = record.get("reasoning_options")?.as_array()?;
    if options.is_empty() || options.len() > 3 {
        return None;
    }
    let mut kinds = Vec::new();
    for option in options {
        let kind = option.get("type")?.as_str()?;
        if !matches!(kind, "effort" | "toggle" | "budget_tokens") || kinds.contains(&kind) {
            return None;
        }
        kinds.push(kind);
    }
    let has = |kind: &str| kinds.contains(&kind);
    let mut exact = options
        .iter()
        .find(|option| option["type"] == "effort")
        .map(|option| serde_json::from_value::<ReasoningOptions>(option.clone()))
        .transpose()
        .ok()?;
    if exact.as_ref().is_some_and(|options| {
        !options.is_valid()
            || options.choices().iter().any(|choice| {
                matches!(
                    choice,
                    ReasoningConfig::On | ReasoningConfig::Effort(E::Ultra)
                )
            })
    }) {
        return None;
    }
    let efforts = exact
        .as_ref()
        .map(|options| {
            options
                .values
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let portable = efforts
        .iter()
        .filter_map(
            |value| match crate::ReasoningConfig::from_provider_value(value) {
                Some(crate::ReasoningConfig::Effort(effort)) => Some(effort),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    let mut preset = StaticModelPreset {
        id: "",
        name: "",
        protocol,
        context_window: max_output_tokens,
        max_output_tokens,
        vision: false,
        audio: false,
        reasoning: true,
        reasoning_mode: Mode::Effort,
        min_reasoning_effort: portable.iter().copied().min().unwrap_or(E::Minimal),
        max_reasoning_effort: portable.iter().copied().max().unwrap_or(E::High),
        openai_chat_reasoning_profile: Profile::Standard,
    };
    match protocol {
        Protocol::GoogleGenerativeAi => {
            let levels = ["minimal", "low", "medium", "high", "xhigh"];
            if efforts.is_empty()
                || has("toggle")
                || efforts
                    .iter()
                    .any(|value| !levels.contains(&value.to_ascii_lowercase().as_str()))
            {
                return None;
            }
            preset.reasoning_mode = Mode::GoogleLevels {
                minimal: portable.contains(&E::Minimal),
                medium: portable.contains(&E::Medium),
            };
        }
        Protocol::AnthropicMessages => {
            let adaptive = api_name.starts_with("claude-")
                && !efforts.is_empty()
                && efforts
                    .iter()
                    .all(|value| ["none", "low", "medium", "high", "xhigh", "max"].contains(value));
            preset.reasoning_mode = if adaptive {
                Mode::Adaptive {
                    off: has("toggle") || efforts.contains(&"none"),
                    xhigh: efforts.contains(&"xhigh"),
                }
            } else if has("budget_tokens") {
                // The budget table spans the fixed Minimal..High efforts.
                preset.min_reasoning_effort = E::Minimal;
                preset.max_reasoning_effort = E::High;
                Mode::Budget
            } else {
                return None;
            };
        }
        Protocol::OpenAiChat => {
            if api_name.starts_with("deepseek-") {
                preset.openai_chat_reasoning_profile = Profile::DeepSeekThinking;
            }
        }
        Protocol::OpenAiResponses => {}
        _ => return None,
    }
    let mut capability = static_reasoning_capability(&preset)?;
    if let Some(options) = &mut exact {
        let min = portable.iter().copied().min()?;
        let max = portable.iter().copied().max()?;
        if has("toggle") && !options.choices().contains(&ReasoningConfig::Off) {
            options.values.insert(0, "none".into());
        }
        if !options.is_valid() {
            return None;
        }
        capability.min_effort = min;
        capability.max_effort = max;
        capability.options = exact;
    } else if capability.control != ReasoningControl::TokenBudget {
        return None;
    } else if !has("toggle") {
        capability.options.as_mut()?.values.retain(|value| {
            ReasoningConfig::from_provider_value(value) != Some(ReasoningConfig::Off)
        });
    }
    Some(capability)
}

pub fn static_reasoning_capability(model: &StaticModelPreset) -> Option<ReasoningCapability> {
    use super::models::StaticReasoningMode as Mode;
    use crate::{ReasoningEffort as E, ReasoningEffortBudgets};
    if !model.reasoning {
        return None;
    }
    let mut cap = ReasoningCapability {
        options: None,
        control: ReasoningControl::Effort,
        exposes_text: true,
        preserves_state: model.protocol != Protocol::OpenAiChat,
        effort_budgets: None,
        openai_chat_mode: model.openai_chat_reasoning_profile.openai_chat_mode(),
        min_effort: model.min_reasoning_effort,
        max_effort: model.max_reasoning_effort,
    };
    let (values, default): (Vec<&str>, Option<&str>) = match model.reasoning_mode {
        Mode::Effort | Mode::MistralEffort | Mode::MistralPrompt => return Some(cap),
        Mode::Budget | Mode::GoogleBudget { .. } => {
            cap.control = ReasoningControl::TokenBudget;
            let maximum = model.max_output_tokens.saturating_sub(1024);
            let maximum = if matches!(model.reasoning_mode, Mode::GoogleBudget { off: true }) {
                maximum.min(24576)
            } else {
                maximum.min(32768)
            };
            cap.effort_budgets = Some(ReasoningEffortBudgets {
                minimal: 1024,
                low: 2048.min(maximum),
                medium: 8192.min(maximum),
                high: 16384.min(maximum),
                xhigh: 24576.min(maximum),
                max: maximum,
            });
            let mut values = vec!["minimal", "low", "medium", "high"];
            if !matches!(model.reasoning_mode, Mode::GoogleBudget { off: false }) {
                values.insert(0, "none");
            }
            (values, Some("medium"))
        }
        Mode::Adaptive { off, xhigh } => {
            cap.min_effort = E::Low;
            cap.max_effort = E::Max;
            let mut values = vec!["low", "medium", "high"];
            if off {
                values.insert(0, "none");
            }
            if xhigh {
                values.push("xhigh");
            }
            values.push("max");
            (values, Some("high"))
        }
        Mode::GoogleLevels { minimal, medium } => {
            cap.min_effort = if minimal { E::Minimal } else { E::Low };
            cap.max_effort = E::High;
            let mut values = vec!["LOW", "HIGH"];
            if medium {
                values.insert(1, "MEDIUM");
            }
            if minimal {
                values.insert(0, "MINIMAL");
            }
            (values, Some("HIGH"))
        }
    };
    cap.options = Some(crate::types::ReasoningOptions {
        values: values.into_iter().map(str::to_owned).collect(),
        default: default.map(str::to_owned),
    });
    Some(cap)
}
