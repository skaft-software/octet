#![deny(missing_docs)]

//! `octet-ai` — provider-independent inference for octet's agent loop.
//!
//! This crate turns a provider-independent [`Request`] plus a selected [`Model`]
//! into either a streamed sequence of [`StreamEvent`]s or a single assembled
//! [`Response`], across three wire protocols (OpenAI Responses, OpenAI Chat
//! Completions, Anthropic Messages).
//!
//! The public surface is the canonical [`types`], the [`stream`] events,
//! [`auth`], [`error`], [`pricing`], and the model [`catalog`]. Everything under
//! `protocol` is private: canonical types never mirror provider JSON.
//!
//! See the [AI design](https://github.com/skaft-software/octet/blob/main/docs/design/octet-ai.md)
//! for the normative design.
//!
//! # Example
//!
//! ```no_run
//! use octet_ai::{AiClient, ModelCatalog, ModelId};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let catalog = ModelCatalog::builtin()?;
//! let model = catalog.resolve(&ModelId("gpt-4o-mini".into()))?;
//! let client = AiClient::new();
//! // Build a provider-independent `Request`, then call `client.stream(...)` or
//! // `client.complete(...)` from your async runtime.
//! let _ = (client, model);
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};

pub mod auth;
pub mod batch;
pub mod catalog;
pub mod client;
pub mod error;
pub mod host_transport;
mod json_repair;
pub mod model_metadata;
pub mod pricing;
pub mod responses;
mod responses_ws;
pub mod stream;
mod transform;
pub mod types;
mod validate;

pub(crate) mod protocol;

pub use auth::{
    Auth, AwsCredentials, AwsSigV4Signer, CredentialResolver, CredentialResolverRegistry,
    CredentialScheme, RequestSigner, ResolvedCredential, Secret, SignedRequestHeaders,
    SigningRequest,
};
pub use batch::{
    BatchError, OpenRouterBatch, OpenRouterBatchList, OpenRouterBatchListOptions,
    OpenRouterBatchRequest, OpenRouterBatchRequestCounts, OpenRouterBatchRequestItem,
    OpenRouterBatchResponse, OpenRouterBatchResult, OpenRouterBatchUsage,
};
pub use catalog::{AuthConfig, CatalogConfig, EndpointConfig, Model, ModelCatalog, ModelConfig};
pub use client::{AiClient, PendingResponsesCompact};
pub use error::{
    AiError, AuthError, ConfigError, DecodeError, Diagnostic, HttpError, PricingError,
    ProviderError, StreamProgress, StreamProtocolError, TransportError, TransportPhase,
    UnsupportedError, ValidationError,
};
pub use host_transport::{HostStreamModel, HostStreamTransport};
pub use mime::Mime;
pub use pricing::{Cost, Pricing, PricingTier, TokenRate, PICODOLLARS_PER_MICRODOLLAR};
pub use responses::{
    ResponsesCompactRequest, ResponsesCompactResponse, ResponsesInput, ResponsesItem,
    ResponsesItemError, ResponsesOptions, ResponsesOutput, ResponsesReplayItem,
};
pub use stream::{
    CanonicalStreamAssembler, ProviderLifecycle, ProviderLifecycleState, ResponseStream,
    StreamEvent,
};
pub use transform::transform_messages;
pub use types::{
    AgentDelegation, AssistantMessage, AssistantPart, AudioCapabilities, AudioFormat, AudioMedia,
    AudioOutputDelivery, AudioOutputOptions, AudioPayload, AudioVoice, CacheCompatibility,
    CacheControlFormat, CacheRetention, Capabilities, Endpoint, EndpointId, EndpointTransport,
    ImageDetail, ImageMedia, ImageSource, JsonSchemaFormat, Media, Message, Modality, ModalitySet,
    ModelId, ModelLimits, ModelSpec, OpenAiChatReasoningMode, OpenAiChatRuntimeProfile,
    OutputFormat, OutputModalities, Protocol, ProviderMediaRef, ProviderPartMetadata,
    ReasoningCapability, ReasoningConfig, ReasoningControl, ReasoningEffort,
    ReasoningEffortBudgets, ReasoningMode, ReasoningPart, ReasoningState, ReasoningStateKind,
    Request, RequestBodyEncoding, RequestRuntime, Response, ResponsesRuntimeProfile,
    SessionAffinityFormat, StopReason, ToolArgumentValidation, ToolCall, ToolCallArgumentError,
    ToolCallId, ToolChoice, ToolDef, ToolResult, ToolResultPart, Usage, UserMessage, UserPart,
};

/// Selects reasoning for a host-generated auxiliary request.
///
/// [`ReasoningConfig::Off`] is preferred only when strict validation accepts it
/// for this exact model route. Otherwise this returns the model capability's
/// valid advertised default (or its first advertised choice when no default was
/// declared). It never invents a missing effort or changes explicit user input.
pub fn select_auxiliary_reasoning(model: &Model) -> Result<ReasoningConfig, AiError> {
    catalog::validate_model_spec(&model.spec)?;
    let off = ReasoningConfig::Off;
    if validate::validate_reasoning_selection(&off, &model.spec.capabilities, model.spec.protocol)
        .is_ok()
    {
        return Ok(off);
    }

    let Some(capability) = model.spec.capabilities.reasoning.as_ref() else {
        return Err(UnsupportedError::Reasoning.into());
    };
    let selected = capability
        .default_selection()
        .ok_or(UnsupportedError::Reasoning)?;
    validate::validate_reasoning_selection(
        &selected,
        &model.spec.capabilities,
        model.spec.protocol,
    )?;
    Ok(selected)
}

/// Validates normalized arguments against the exact tool-definition snapshot.
///
/// A completed JSON object that merely violates a valid schema returns
/// [`ToolArgumentValidation::SchemaMismatch`]. Malformed schemas and bounded
/// validation failures return a fatal [`DecodeError`]. Unknown tool names are
/// retained as [`ToolArgumentValidation::UnknownTool`] so callers can produce
/// their normal unknown-tool result.
pub fn validate_tool_arguments(
    tool_name: &str,
    arguments: &serde_json::Value,
    tools: &[ToolDef],
) -> Result<ToolArgumentValidation, DecodeError> {
    json_repair::validate_tool_arguments(tool_name, arguments, tools)
}

/// Strictness for cross-protocol / capability degradation.
///
/// Before either mode is validated, [`transform_messages`] derives a
/// target-compatible copy of conversation history (including explicit media
/// placeholders and interrupted-tool recovery) without mutating canonical
/// history. `Strict` (the default) then rejects unsupported request features;
/// `Lossy` additionally permits reported derived-wire degradation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompatibilityMode {
    /// Reject unsupported capabilities with a structured error.
    #[default]
    Strict,
    /// Drop unsupported data with a reported diagnostic; never silently. This
    /// also opts into ambiguous provider-response repairs, such as holding a
    /// complete bare-JSON content stream to recognize a textual tool envelope.
    Lossy,
}
