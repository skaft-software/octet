#![deny(missing_docs)]

//! `octet-ai` — provider-independent inference for Rust applications.
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

pub mod assistant_frame;
pub mod auth;
pub mod batch;
pub mod catalog;
pub mod client;
pub mod constrained_sampling;
pub mod declarations;
pub mod deferred;
pub mod discovery;
pub mod error;
pub mod faux;
#[allow(missing_docs)]
pub mod host_http;
pub mod host_transport;
pub mod images;
pub mod inference;
mod json_repair;
pub use json_repair::{validate_json_schema_definition, validate_json_schema_value};
mod google_files;
pub mod media;
pub mod model_metadata;
pub mod pricing;
mod recorded_audio;
mod request_trace;
pub mod responses;
mod responses_ws;
pub mod runtime;
pub mod steering;
pub mod stream;
mod transform;
pub mod types;
mod validate;
pub mod video;

pub(crate) mod protocol;

#[cfg(test)]
mod test_fixtures;

pub use assistant_frame::{
    reduce_assistant_message_frames, AssistantMessageFrame, AssistantMessageFrameEncoder,
};
pub use auth::{
    anthropic_bearer_auth, environment_variable_present, first_present_variable,
    resolve_extension_provider_credentials, select_vertex_credential, vertex_api_key_auth, Auth,
    AwsCredentials, AwsSigV4Signer, CredentialResolver, CredentialResolverRegistry,
    CredentialScheme, ExtensionProviderCredentials, RequestSigner, ResolvedCredential, Secret,
    SignedRequestHeaders, SigningRequest, VertexCredential, ANTHROPIC_BEARER_TOKEN_VARIABLES,
    GOOGLE_APPLICATION_CREDENTIALS_VAR, GOOGLE_VERTEX_API_KEY_VAR,
};
pub use batch::{
    BatchError, OpenRouterBatch, OpenRouterBatchList, OpenRouterBatchListOptions,
    OpenRouterBatchRequest, OpenRouterBatchRequestCounts, OpenRouterBatchRequestItem,
    OpenRouterBatchResponse, OpenRouterBatchResult, OpenRouterBatchUsage,
};
pub use catalog::{
    effective_output_token_cap, AuthConfig, CatalogConfig, EndpointConfig, Model, ModelCatalog,
    ModelConfig,
};
pub use client::{AiClient, PendingResponsesCompact};
pub use declarations::proxy::{proxy_env_value, resolve_http_proxy, ProxyError};
pub use declarations::{
    AzureRequestOptions, ChatTemplateValue, ChatTemplateVariable, DeclarationError,
    MistralReasoningProfile, ModelPreset, ProviderCredentialPreset, RequestOverrides,
    ThinkingFormat, ThinkingSelection, ThinkingTokenBudgetField, ThinkingVariable,
};
pub use deferred::{
    DeferredHandle, DeferredHandleRejection, DeferredPollPermit, DeferredPollRefusalKind,
};
pub use error::{
    AiError, AuthError, ConfigError, DecodeError, Diagnostic, HttpError, PricingError,
    ProviderError, StreamProgress, StreamProtocolError, TransportError, TransportPhase,
    UnsupportedError, ValidationError,
};
pub use faux::{
    FauxDeferredStatus, FauxMessage, FauxOptions, FauxProvider, FauxResponse, FauxState,
    FauxToolCall,
};
pub use host_http::{HostHttpRequest, HostHttpResponse, HostHttpSender};
pub use host_transport::{
    HostProviderRequestHooks, HostStreamModel, HostStreamTransport, NativeRequestContinuation,
};
pub use inference::{
    ClientInferenceMetrics, ClientTimingScope, DecodeEstimate, DecodeEstimateUnavailable,
    InferenceMetrics, OutputRateSample, ReportedTimingUnit, ServerGenerationMetrics,
    ServerTimingSource, ServerTimingUnavailable,
};
pub use protocol::google::cache::{GoogleCacheLimits, GoogleCacheSession, GoogleCachedContent};

pub use images::{
    GeneratedImage, ImageApi, ImageCancellation, ImageGenerationOptions, ImageGenerationRequest,
    ImageGenerationResponse, ImageInput, ImageModality, ImageModel, ImageModelCatalog,
    ImageModelSpec, ImageOutput, ImagePricing, ImageStopReason, MAX_GENERATED_IMAGES,
    MAX_GENERATED_IMAGE_BYTES, MAX_IMAGE_INPUTS, MAX_IMAGE_INPUT_BYTES, MAX_IMAGE_PROMPT_BYTES,
    OPENROUTER_API_KEY_VAR, OPENROUTER_IMAGES_API,
};
pub use media::{
    prepare_user_image, ImageInputError, ImageInputLimits, MAX_USER_IMAGE_BYTES,
    MAX_USER_IMAGE_PIXELS,
};
pub use mime::Mime;
pub use pricing::{
    responses_cost_of, Cost, Pricing, PricingTier, TokenRate, PICODOLLARS_PER_MICRODOLLAR,
};
pub use responses::{
    validate_responses_input, ComputerUseEnvironment, ComputerUseTool, ResponsesCompactRequest,
    ResponsesCompactResponse, ResponsesConfigurationUpdate, ResponsesInput, ResponsesItem,
    ResponsesItemError, ResponsesOptions, ResponsesOutput, ResponsesReplayItem,
};
pub use runtime::{
    HeaderTransform, HookModelContext, HostRequestOptions, PayloadHook, ProviderRequestContext,
    ProviderRequestHook, ResponseHook, MAX_RUNTIME_METADATA_BYTES, MAX_RUNTIME_METADATA_ENTRIES,
};
pub use steering::{
    SteeringControl, SteeringEvent, SteeringSession, SteeringState, SteeringUpdate,
};
pub use stream::{
    CanonicalStreamAssembler, ProviderLifecycle, ProviderLifecycleState, ResponseStream,
    StreamEvent,
};
pub use transform::transform_messages;
pub use types::{
    AgentDelegation, AssistantMessage, AssistantPart, AudioCapabilities, AudioFormat, AudioMedia,
    AudioOutputDelivery, AudioOutputOptions, AudioPayload, AudioVoice, CacheCompatibility,
    CacheControlFormat, CacheRetention, Capabilities, ConstrainedSampling,
    ConstrainedSamplingStrict, ContextWindowOptions, CustomOpenAiChatProfile, DocumentMedia,
    Endpoint, EndpointId, EndpointTransport, GrammarVariants, ImageDetail, ImageMedia, ImageSource,
    JsonSchemaFormat, Media, Message, Modality, ModalitySet, ModelId, ModelLimits, ModelSpec,
    OpenAiChatReasoningMode, OpenAiChatRuntimeProfile, OutputFormat, OutputModalities,
    PromptCacheLifetimes, Protocol, ProviderMediaRef, ProviderPartMetadata, ReasoningCapability,
    ReasoningConfig, ReasoningControl, ReasoningEffort, ReasoningEffortBudgets, ReasoningMode,
    ReasoningPart, ReasoningState, ReasoningStateKind, Request, RequestBodyEncoding,
    RequestRuntime, Response, ResponsesFeatures, ResponsesRuntimeProfile, ServiceTier,
    SessionAffinityFormat, StopReason, ToolArgumentValidation, ToolCall, ToolCallArgumentError,
    ToolCallId, ToolChoice, ToolDef, ToolResult, ToolResultPart, Usage, UserMessage, UserPart,
    VideoMedia,
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

/// Validates an effective canonical request without transforming or repairing it.
///
/// Hosts preparing context projections use this before exposing a replacement
/// to another hook. This performs no credential resolution or provider I/O;
/// codec-specific encoding checks still run at ordinary dispatch.
pub fn validate_provider_request(
    model: &Model,
    request: &Request,
) -> Result<Vec<Diagnostic>, AiError> {
    let mut capabilities = model.spec.capabilities.clone();
    capabilities.responses_features = model.responses_features();
    json_repair::validate_tool_definitions(&request.tools)?;
    validate::validate_request(
        request,
        &capabilities,
        &model.spec.limits,
        model.spec.protocol,
        &model.spec.id,
        CompatibilityMode::Strict,
    )
}

/// Validates a context projection using dispatch normalization, while keeping
/// newly introduced media strict even when it is placed in historical turns.
/// `canonical` is the original host-owned request, not a prior hook projection.
pub fn validate_provider_projection(
    model: &Model,
    canonical: &Request,
    request: &Request,
) -> Result<Vec<Diagnostic>, AiError> {
    use crate::types::{AssistantPart, Message, ToolResultPart, UserPart};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;

    fn key(kind: u8, media: &crate::Media) -> (u8, [u8; 32]) {
        let bytes = serde_json::to_vec(media).expect("media serializes");
        (kind, Sha256::digest(bytes).into())
    }
    let mut original = HashMap::new();
    for message in &canonical.messages {
        match message {
            Message::User(user) => {
                for part in &user.content {
                    match part {
                        UserPart::Media(media) => {
                            *original.entry(key(0, media)).or_insert(0usize) += 1
                        }
                        UserPart::ToolResult(result) => {
                            for part in &result.content {
                                if let ToolResultPart::Media(media) = part {
                                    *original.entry(key(1, media)).or_insert(0usize) += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Assistant(assistant) => {
                for part in &assistant.content {
                    if let AssistantPart::Media(media) = part {
                        *original.entry(key(2, media)).or_insert(0usize) += 1;
                    }
                }
            }
        }
    }
    let mut probe = request.clone();
    let mut has_new = false;
    let mut is_original = |kind, media: &crate::Media| {
        let count = original.entry(key(kind, media)).or_default();
        if *count > 0 {
            *count -= 1;
            true
        } else {
            has_new = true;
            false
        }
    };
    for message in &mut probe.messages {
        match message {
            Message::User(user) => {
                for part in &mut user.content {
                    match part {
                        UserPart::Media(media) if is_original(0, media) => {
                            *part = UserPart::Text(String::new())
                        }
                        UserPart::ToolResult(result) => {
                            for part in &mut result.content {
                                if let ToolResultPart::Media(media) = part {
                                    if is_original(1, media) {
                                        *part = ToolResultPart::Text(String::new());
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Assistant(assistant) => {
                for part in &mut assistant.content {
                    if let AssistantPart::Media(media) = part {
                        if is_original(2, media) {
                            *part = AssistantPart::Text(String::new());
                        }
                    }
                }
            }
        }
    }
    if has_new {
        probe.messages = transform::transform_request_preserving_media(probe.messages, model);
        validate_provider_request(model, &probe)?;
    }
    let mut normalized = request.clone();
    let mut replay_diagnostics = transform::prepare_request_history(&mut normalized, model);
    replay_diagnostics.extend(validate_provider_request(model, &normalized)?);
    Ok(replay_diagnostics)
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

/// UI-neutral provider authentication and explicit storage adapters.
pub mod provider_auth;
/// Descriptor-bound private storage primitives.
pub mod secure_fs;

/// Provider declarations and account-scoped catalog binding.
pub mod providers;
