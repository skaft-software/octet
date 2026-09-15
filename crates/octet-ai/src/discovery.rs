//! Bounded, endpoint-owned model self-description for existing Chat/Responses routes.
//!
//! This opt-in schema is not a model database or a protocol negotiation mechanism.
//! The caller supplies the selected endpoint/model/codec; response data cannot
//! redirect requests, change authentication, or enable a different protocol.

use serde::Deserialize;
use serde_json::Value;

use crate::types::ReasoningOptions;
use crate::{
    Capabilities, EndpointId, Modality, ModalitySet, ModelLimits, OpenAiChatReasoningMode,
    Protocol, ReasoningCapability, ReasoningConfig, ReasoningControl, ReasoningEffort,
};

/// Maximum serialized size of one `octet_capabilities` declaration.
pub const MAX_SELF_DESCRIPTION_BYTES: usize = 4096;

/// Host-assigned provenance, never taken from fields in the declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoverySource {
    /// Selected endpoint identity, without URL or authentication material.
    pub endpoint: EndpointId,
    /// Inventory-returned transport model name.
    pub api_name: String,
    /// Existing host-selected codec, not an endpoint-selected new route.
    pub protocol: Protocol,
}

/// Validated v1 self-description. All unspecified capabilities are disabled.
#[derive(Clone, Debug)]
pub struct ModelSelfDescription {
    source: DiscoverySource,
    limits: ModelLimits,
    capabilities: Capabilities,
}

/// An invalid, oversized, or unsupported declaration. Contains no response data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid or unsupported endpoint capability self-description")]
pub struct SelfDescriptionError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    version: u8,
    protocol: Protocol,
    context_window: u64,
    max_output_tokens: u64,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
    #[serde(default)]
    tools: bool,
    #[serde(default)]
    parallel_tool_calls: bool,
    #[serde(default)]
    structured_output: bool,
    #[serde(default)]
    reasoning: Option<EffortOptions>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EffortOptions {
    values: Vec<String>,
    #[serde(default)]
    default: Option<String>,
}

impl ModelSelfDescription {
    /// Decode an optional `octet_capabilities` object in one inventory entry.
    ///
    /// The caller must obtain the entry from its selected, bounded discovery
    /// route (or an account/URL-isolated raw cache), never a third-party snapshot.
    /// A present null/malformed declaration is an error, not an absent assertion.
    /// Only existing Chat and Responses routes support v1; native codecs, Lite,
    /// delegation, audio, budget/toggle reasoning and arbitrary wire profiles do
    /// not gain authority through this schema.
    pub fn from_entry(
        entry: &Value,
        source: DiscoverySource,
    ) -> Result<Option<Self>, SelfDescriptionError> {
        let Some(value) = entry.get("octet_capabilities") else {
            return Ok(None);
        };
        if !matches!(
            source.protocol,
            Protocol::OpenAiChat | Protocol::OpenAiResponses
        ) || !valid_identity(&source.endpoint.0)
            || !valid_identity(&source.api_name)
        {
            return Err(SelfDescriptionError);
        }
        // Count while serializing rather than allocating an unbounded second
        // copy of a hostile inventory entry. HTTP/cache limits apply separately.
        struct Bound(usize);
        impl std::io::Write for Bound {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > self.0 {
                    return Err(std::io::Error::other("capability size limit"));
                }
                self.0 -= bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Bound(MAX_SELF_DESCRIPTION_BYTES), value)
            .map_err(|_| SelfDescriptionError)?;
        let declaration: Declaration =
            serde_json::from_value(value.clone()).map_err(|_| SelfDescriptionError)?;
        if declaration.version != 1
            || declaration.protocol != source.protocol
            || declaration.context_window == 0
            || declaration.max_output_tokens == 0
            || declaration.max_output_tokens > declaration.context_window
            || (declaration.parallel_tool_calls && !declaration.tools)
        {
            return Err(SelfDescriptionError);
        }
        let input_modalities = modalities(&declaration.input_modalities, true)?;
        let output_modalities = modalities(&declaration.output_modalities, false)?;
        let reasoning = declaration
            .reasoning
            .map(|reasoning| {
                let options = ReasoningOptions {
                    values: reasoning.values,
                    default: reasoning.default,
                };
                // v1 advertises exact effort controls only. Ultra requires separate
                // account/host delegation authority and must never be inferred here.
                if !options.is_valid() || options.values.len() > 8 {
                    return Err(SelfDescriptionError);
                }
                let mut efforts = Vec::new();
                for choice in options.choices() {
                    match choice {
                        ReasoningConfig::Off => {}
                        ReasoningConfig::Effort(effort) if effort != ReasoningEffort::Ultra => {
                            efforts.push(effort);
                        }
                        _ => return Err(SelfDescriptionError),
                    }
                }
                let min_effort = efforts.iter().copied().min().ok_or(SelfDescriptionError)?;
                let max_effort = efforts.iter().copied().max().ok_or(SelfDescriptionError)?;
                Ok(ReasoningCapability {
                    control: ReasoningControl::Effort,
                    exposes_text: true,
                    preserves_state: true,
                    min_effort,
                    max_effort,
                    effort_budgets: None,
                    openai_chat_mode: OpenAiChatReasoningMode::Standard,
                    options: Some(options),
                })
            })
            .transpose()?;
        Ok(Some(Self {
            source,
            limits: ModelLimits {
                context_window: declaration.context_window,
                max_output_tokens: declaration.max_output_tokens,
            },
            capabilities: Capabilities {
                input_modalities,
                output_modalities,
                tools: declaration.tools,
                parallel_tool_calls: declaration.parallel_tool_calls,
                structured_output: declaration.structured_output,
                reasoning,
                responses_lite: false,
                agent_delegation: None,
                deferred_tool_loading: false,
            },
        }))
    }

    /// Endpoint/model/codec provenance assigned by the discovery owner.
    pub fn source(&self) -> &DiscoverySource {
        &self.source
    }

    /// Validated positive token limits (output never exceeds context).
    pub fn limits(&self) -> &ModelLimits {
        &self.limits
    }

    /// Capabilities representable by the selected codec. Provider-specific
    /// reasoning wire profiles still require the host's route intersection.
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

fn modalities(values: &[String], input: bool) -> Result<ModalitySet, SelfDescriptionError> {
    let mut result = ModalitySet::none();
    if values.len() > 2 {
        return Err(SelfDescriptionError);
    }
    for (index, value) in values.iter().enumerate() {
        if values[..index].contains(value) {
            return Err(SelfDescriptionError);
        }
        match value.as_str() {
            "text" => {}
            "image" if input => result = result.with(Modality::Image),
            _ => return Err(SelfDescriptionError),
        }
    }
    Ok(result)
}
